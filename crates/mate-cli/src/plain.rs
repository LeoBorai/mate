//! The plain frontend (§10, `M5-2`/`M5-3`/`M5-4`): a single-session, line-based renderer with
//! no ANSI output, so `mate --plain` and `mate -p "…" | cat` stay script-clean regardless of
//! whether stdout is a TTY. `--print` runs exactly the prompt handed on the command line and
//! exits; `--plain` (without `--print`) starts an interactive loop that runs one turn per line
//! read from stdin, seeded with the prompt argument if one was given.
//!
//! This frontend doesn't route through `mate_core::session`'s session manager (`M6`), so it
//! doesn't carry conversation history between turns — each line is an independent one-shot
//! prompt against the same agent. History threading is the session task's job; wiring this
//! frontend through it is future work, not part of `M6`'s own (core-only) scope.

use std::io::Write;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use mate_core::agent::{BuiltAgent, build_agent};
use mate_core::agents_md::discover_agents_md_capped;
use mate_core::backend::Backend;
use mate_core::config::AgentSpec;
use mate_core::preamble::{PreambleRole, SkillDescriptor, render_preamble};
use mate_core::provider_error::ProviderError;
use mate_core::streaming::{AgentEvent, AgentEventEnvelope};
use mate_core::toolset::tool_descriptors;
use mate_tool_api::{AgentId, ToolCtx};
use mate_tool_http::HttpShared;
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

use crate::cli::Cli;
use crate::config::{Config, api_token};
use crate::error::MateError;

/// Not exposed by `Config`/`Cli` yet (§10 lists neither as a knob) — fixed until a real need
/// to tune them shows up.
pub(crate) const DEFAULT_TEMPERATURE: f64 = 0.2;
pub(crate) const DEFAULT_MAX_TOKENS: u64 = 4096;

/// Whether `main` should route this invocation through the plain frontend: an explicit
/// `--plain`/`--print`, or a prompt on a non-TTY stdout (`M5-3` — piping into another program
/// shouldn't require remembering `--plain`). A bare `mate` with no prompt and no flag is left
/// alone: with no seed prompt there's nothing to force plain mode *for*, and the fallback
/// (launching the default tabbed TUI) is `main`'s call, not this one's.
pub fn wants_plain(cli: &Cli, stdout_is_tty: bool) -> bool {
    cli.plain || cli.print || (cli.prompt.is_some() && !stdout_is_tty)
}

pub async fn run(cli: &Cli, config: &Config) -> Result<(), MateError> {
    let token =
        api_token().ok_or_else(|| MateError::Auth(anyhow::anyhow!("API_TOKEN is not set")))?;

    let backend = Backend::huggingface(&token, config.sub_provider.as_deref(), None)
        .map_err(|err| MateError::Provider(anyhow::anyhow!(err)))?;
    backend
        .verify()
        .await
        .map_err(|err| classify_verify_error(ProviderError::classify(&err)))?;

    let workspace_root = resolve_workspace_root(cli)?;
    let http = Arc::new(
        HttpShared::new(config.http.rate_limit_per_host_per_min)
            .map_err(|err| MateError::Other(anyhow::anyhow!(err)))?,
    );
    let skills = mate_tool_skills::discover_skills(&workspace_root);
    let skill_descriptors: Vec<SkillDescriptor> = skills
        .iter()
        .map(|s| SkillDescriptor::new(s.name.clone(), s.description.clone()))
        .collect();
    let agents_md = discover_agents_md_capped(
        &workspace_root,
        config.agents_md.enabled,
        config.agents_md.max_bytes,
    );
    // Always `false`: this frontend never routes through `mate_core::session::SessionManager`
    // (see this module's doc comment), so there's no `SubagentSpawner` to give `ctx.spawner`
    // below — advertising `spawn_agent` here would tell the model about a tool
    // `toolset::build_toolset` (gated on `ctx.spawner.is_some()`) would never actually attach.
    let preamble = render_preamble(
        PreambleRole::Root,
        &workspace_root,
        std::env::consts::OS,
        &tool_descriptors(false, config.http.enabled, !skills.is_empty()),
        &skill_descriptors,
        agents_md.as_ref(),
    );

    let spec = AgentSpec {
        model: config.model.clone(),
        sub_provider: config.sub_provider.clone(),
        base_url: None,
        preamble,
        temperature: DEFAULT_TEMPERATURE,
        max_tokens: DEFAULT_MAX_TOKENS,
        max_turns: config.max_turns,
        http: config.http.clone(),
        // Always `false` here for the same reason `tool_descriptors(false)` is above: no
        // `SessionManager` in this frontend means no `SubagentSpawner` ever reaches `ctx`.
        may_delegate: false,
        delegation: config.delegation.clone(),
    };

    let cancel = CancellationToken::new();
    let (activity, _activity_rx) = tokio::sync::mpsc::channel(64);
    let ctx = ToolCtx {
        agent: AgentId::ROOT,
        root: workspace_root,
        max_output_bytes: config.tools.max_output_bytes,
        spawner: None,
        activity,
        cancel: cancel.clone(),
        approvals: None,
        skills: Arc::from(skills),
        agents_md: agents_md.map(Arc::new),
    };
    let agent = build_agent(&backend, &http, &spec, ctx);

    // `M5-4`: Ctrl+C cancels the in-flight turn, which unwinds the loop below cleanly rather
    // than killing the process mid-write. Aborted once `run` returns so a normal exit doesn't
    // leave the listener task behind.
    let sigint_cancel = cancel.clone();
    let sigint_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sigint_cancel.cancel();
        }
    });

    let result = if cli.plain && !cli.print {
        run_interactive(&agent, cli.prompt.clone(), &cancel).await
    } else {
        let prompt = cli
            .prompt
            .clone()
            .expect("wants_plain only selects this path with --print or a prompt already set");
        run_turn(&agent, prompt, &cancel).await.map(|_cancelled| ())
    };

    sigint_task.abort();
    result
}

/// Runs one turn per line read from stdin, seeded with `first_prompt` if given, until EOF or
/// the turn is cancelled.
async fn run_interactive(
    agent: &BuiltAgent,
    first_prompt: Option<String>,
    cancel: &CancellationToken,
) -> Result<(), MateError> {
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut pending = first_prompt;
    loop {
        let prompt = match pending.take() {
            Some(prompt) => prompt,
            None => match lines
                .next_line()
                .await
                .map_err(|err| MateError::Io(err.into()))?
            {
                Some(line) => line,
                None => break,
            },
        };
        if run_turn(agent, prompt, cancel).await? {
            break;
        }
    }
    Ok(())
}

/// Streams one turn to stdout. Returns `true` if the turn was cancelled (the caller should
/// stop looping), or an error for anything the turn itself failed with.
async fn run_turn(
    agent: &BuiltAgent,
    prompt: String,
    cancel: &CancellationToken,
) -> Result<bool, MateError> {
    let outcome = agent
        .stream_turn(prompt, cancel, |envelope| {
            let mut stdout = std::io::stdout();
            render_event(&mut stdout, envelope);
            let _ = stdout.flush();
        })
        .await;
    println!();

    if outcome.cancelled {
        return Ok(true);
    }
    if let Some(err) = outcome.error {
        return Err(MateError::Provider(anyhow::anyhow!(err)));
    }
    Ok(false)
}

/// Renders one event as pipe-safe plain text (`M5-2`): raw token text as it streams, one line
/// per completed tool call with its name and outcome. Events with no producer yet
/// (`ApprovalRequired`, `SubagentSpawned`/`Finished`, `Usage`) render nothing — there's nothing
/// real to show until the milestones that produce them land.
fn render_event(out: &mut impl Write, envelope: AgentEventEnvelope) {
    match envelope.event {
        AgentEvent::Token(text) => {
            let _ = write!(out, "{text}");
        }
        AgentEvent::ToolResult { name, ok, summary } => {
            if ok {
                let _ = writeln!(out, "\n[{name}] ok");
            } else {
                let _ = writeln!(out, "\n[{name}] failed: {summary}");
            }
        }
        _ => {}
    }
}

pub(crate) fn classify_verify_error(classified: ProviderError) -> MateError {
    if classified == ProviderError::AuthFailed {
        MateError::Auth(anyhow::anyhow!(classified))
    } else {
        MateError::Provider(anyhow::anyhow!(classified))
    }
}

pub(crate) fn resolve_workspace_root(cli: &Cli) -> Result<PathBuf, MateError> {
    Ok(resolve_workspace_roots(cli)?.remove(0))
}

/// Every workspace root `-C`/`--dir` names, canonicalized (`M8-5`: one tab per path). A `Cli`
/// with no `--dir` at all still yields exactly one root — the current directory — matching
/// [`resolve_workspace_root`]'s existing single-session default.
pub(crate) fn resolve_workspace_roots(cli: &Cli) -> Result<Vec<PathBuf>, MateError> {
    if cli.dir.is_empty() {
        return Ok(vec![canonicalize(&PathBuf::from("."))?]);
    }
    cli.dir.iter().map(|dir| canonicalize(dir)).collect()
}

fn canonicalize(root: &Path) -> Result<PathBuf, MateError> {
    dunce::canonicalize(root).map_err(|err| {
        let context = format!("resolving workspace root {}", root.display());
        MateError::Io(anyhow::Error::from(err).context(context))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        let mut argv = vec!["mate"];
        argv.extend_from_slice(args);
        Cli::parse_from(argv)
    }

    fn envelope(event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            agent: AgentId::ROOT,
            event,
        }
    }

    #[test]
    fn explicit_plain_or_print_always_selects_the_plain_frontend() {
        assert!(wants_plain(&cli(&["--plain"]), true));
        assert!(wants_plain(&cli(&["--plain"]), false));
        assert!(wants_plain(&cli(&["--print", "hi"]), true));
        assert!(wants_plain(&cli(&["--print", "hi"]), false));
    }

    #[test]
    fn a_bare_prompt_on_a_non_tty_stdout_forces_plain_mode() {
        assert!(wants_plain(&cli(&["do the thing"]), false));
    }

    #[test]
    fn a_bare_prompt_on_a_tty_stdout_does_not_force_plain_mode() {
        assert!(!wants_plain(&cli(&["do the thing"]), true));
    }

    #[test]
    fn no_prompt_and_no_flags_never_selects_plain_regardless_of_tty() {
        assert!(!wants_plain(&cli(&[]), true));
        assert!(!wants_plain(&cli(&[]), false));
    }

    #[test]
    fn token_events_stream_raw_text_with_no_escape_codes() {
        let mut buf = Vec::new();
        render_event(&mut buf, envelope(AgentEvent::Token("hi".to_string())));
        render_event(&mut buf, envelope(AgentEvent::Token(" there".to_string())));
        assert_eq!(String::from_utf8(buf).unwrap(), "hi there");
    }

    #[test]
    fn a_successful_tool_call_renders_one_line_with_its_name() {
        let mut buf = Vec::new();
        render_event(
            &mut buf,
            envelope(AgentEvent::ToolResult {
                name: "read_file".to_string(),
                ok: true,
                summary: "ignored for a successful call".to_string(),
            }),
        );
        assert_eq!(String::from_utf8(buf).unwrap(), "\n[read_file] ok\n");
    }

    #[test]
    fn a_failed_tool_call_renders_its_summary() {
        let mut buf = Vec::new();
        render_event(
            &mut buf,
            envelope(AgentEvent::ToolResult {
                name: "read_file".to_string(),
                ok: false,
                summary: "not found: missing.txt".to_string(),
            }),
        );
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "\n[read_file] failed: not found: missing.txt\n"
        );
    }

    #[test]
    fn events_with_no_producer_yet_render_nothing() {
        let mut buf = Vec::new();
        for event in [
            AgentEvent::ToolCallStarted {
                name: "read_file".to_string(),
            },
            AgentEvent::TurnComplete,
            AgentEvent::Error("boom".to_string()),
        ] {
            render_event(&mut buf, envelope(event));
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn auth_failures_classify_to_the_auth_exit_code() {
        assert_eq!(
            classify_verify_error(ProviderError::AuthFailed).exit_code(),
            4
        );
    }

    #[test]
    fn other_provider_failures_classify_to_the_provider_exit_code() {
        assert_eq!(
            classify_verify_error(ProviderError::RateLimited).exit_code(),
            5
        );
    }

    #[test]
    fn workspace_root_defaults_to_the_current_directory() {
        let root = resolve_workspace_root(&cli(&[])).unwrap();
        assert_eq!(root, dunce::canonicalize(".").unwrap());
    }

    #[test]
    fn workspace_root_uses_the_first_dir_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let root = resolve_workspace_root(&cli(&["-C", tmp.path().to_str().unwrap()])).unwrap();
        assert_eq!(root, dunce::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn workspace_root_rejects_a_path_that_does_not_exist() {
        let err = resolve_workspace_root(&cli(&["-C", "/does/not/exist/anywhere"])).unwrap_err();
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn repeated_dir_flags_resolve_to_one_root_per_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let roots = resolve_workspace_roots(&cli(&[
            "-C",
            a.path().to_str().unwrap(),
            "-C",
            b.path().to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(
            roots,
            vec![
                dunce::canonicalize(a.path()).unwrap(),
                dunce::canonicalize(b.path()).unwrap(),
            ]
        );
    }

    #[test]
    fn no_dir_flag_resolves_to_a_single_root_the_current_directory() {
        let roots = resolve_workspace_roots(&cli(&[])).unwrap();
        assert_eq!(roots, vec![dunce::canonicalize(".").unwrap()]);
    }
}
