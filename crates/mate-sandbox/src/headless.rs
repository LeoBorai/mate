//! One-shot, non-interactive Claude Code turns.

use uuid::Uuid;

use crate::claude::{
    HeadlessResult, choose_session_arg, headless_command, parse_headless, tmux_has_session_command,
    transcript_path,
};
use crate::docker::{DockerApi, exec_collect, verify_ownership};
use crate::error::SandboxError;
use crate::session::SandboxSession;
use crate::spec::CLAUDE_CONFIG_DIR_ENV;

/// Shown when a headless turn is refused because tmux still hosts Claude Code.
pub const INTERACTIVE_ALIVE: &str =
    "an interactive Claude Code session is still alive: exit Claude Code in the attached terminal first";

/// True when the sandbox's tmux session is alive.
pub async fn interactive_alive(
    docker: &dyn DockerApi,
    session: &SandboxSession,
) -> Result<bool, SandboxError> {
    let out = exec_collect(
        docker,
        &session.container_id,
        &session.id,
        tmux_has_session_command(),
    )
    .await?;
    Ok(out.exit_code == Some(0))
}

/// `CLAUDE_CONFIG_DIR` as set in the container's own env.
fn config_dir(env: Option<&Vec<String>>) -> Option<String> {
    env?.iter()
        .find_map(|e| e.strip_prefix(&format!("{CLAUDE_CONFIG_DIR_ENV}=")).map(str::to_owned))
}

/// Whether the stored Claude session's transcript exists in the container.
pub async fn transcript_exists(
    docker: &dyn DockerApi,
    session: &SandboxSession,
    id: Uuid,
    config_dir: &str,
) -> Result<bool, SandboxError> {
    let path = transcript_path(config_dir, id);
    let out = exec_collect(
        docker,
        &session.container_id,
        &session.id,
        vec!["test".into(), "-f".into(), path],
    )
    .await?;
    Ok(out.exit_code == Some(0))
}

/// Run one headless turn. Refuses while tmux is alive. On success the
/// session's `claude_session_id` is updated; the caller persists the store.
pub async fn run_headless(
    docker: &dyn DockerApi,
    session: &mut SandboxSession,
    prompt: &str,
) -> Result<HeadlessResult, SandboxError> {
    let inspect = verify_ownership(docker, &session.container_id, &session.id).await?;
    if interactive_alive(docker, session).await? {
        return Err(SandboxError::Headless {
            exit_code: None,
            stderr: String::new(),
            detail: INTERACTIVE_ALIVE.to_owned(),
        });
    }
    let stored = session.claude_session_id;
    let exists = match (stored, config_dir(inspect.config.as_ref().and_then(|c| c.env.as_ref()))) {
        (Some(id), Some(dir)) => transcript_exists(docker, session, id, &dir).await?,
        _ => false,
    };
    let arg = choose_session_arg(stored, exists, Uuid::new_v4);
    // Persist the id before the turn so a crash cannot orphan the conversation.
    session.claude_session_id = Some(arg.id());
    let out = exec_collect(
        docker,
        &session.container_id,
        &session.id,
        headless_command(prompt, arg),
    )
    .await?;
    let result = parse_headless(&out.stdout, &out.stderr, out.exit_code)?;
    if let Some(id) = result.session_id {
        session.claude_session_id = Some(id);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDocker, labelled_inspect};
    use crate::session::tests::sample;

    fn fake_for(s: &SandboxSession) -> FakeDocker {
        let fake = FakeDocker::default();
        let mut i = labelled_inspect(
            &[("managed-by", "mate"), ("session-id", &s.id.to_string())],
            "running",
        );
        i.config.as_mut().unwrap().env = Some(vec!["CLAUDE_CONFIG_DIR=/home/u/.claude".into()]);
        fake.add(&s.container_id, i);
        fake
    }

    #[tokio::test]
    async fn refuses_when_tmux_session_alive() {
        let mut s = sample();
        let fake = fake_for(&s);
        fake.script("tmux has-session -t claude", "", "", 0);
        let err = run_headless(&fake, &mut s, "hi").await.unwrap_err();
        assert!(
            matches!(&err, SandboxError::Headless { detail, .. } if detail == INTERACTIVE_ALIVE),
            "a live tmux session means a second writer; refuse, got {err:?}"
        );
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("exec claude")),
            "claude was never started"
        );
    }

    #[tokio::test]
    async fn happy_path_stores_returned_session_id() {
        let mut s = sample();
        let fake = fake_for(&s);
        let sid = "99999999-2222-4333-8444-555555555555";
        // Pre-store an id so the command (and thus the script key) is
        // deterministic; has-session is unscripted, so tmux reads as dead.
        s.claude_session_id = Some(Uuid::parse_str("11111111-2222-4333-8444-555555555555").unwrap());
        fake.script(
            "test -f /home/u/.claude/projects/-workspace/11111111-2222-4333-8444-555555555555.jsonl",
            "", "", 0,
        );
        fake.script(
            "claude -p hi --output-format json --resume 11111111-2222-4333-8444-555555555555",
            &format!(r#"{{"is_error":false,"result":"ok","session_id":"{sid}"}}"#),
            "", 0,
        );
        let r = run_headless(&fake, &mut s, "hi").await.unwrap();
        assert_eq!(r.result, "ok", "result text returned");
        assert_eq!(
            s.claude_session_id,
            Some(Uuid::parse_str(sid).unwrap()),
            "the session id from the output overwrites the stored one"
        );
    }
}
