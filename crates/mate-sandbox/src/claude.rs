//! Claude Code command construction (and, later, headless output parsing).

use uuid::Uuid;

use crate::error::SandboxError;

/// Name of the tmux session that hosts interactive Claude Code.
pub const TMUX_SESSION: &str = "claude";

/// How a Claude Code invocation identifies its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionArg {
    /// `--session-id <uuid>`: start a conversation with this id.
    New(Uuid),
    /// `--resume <uuid>`: continue an existing conversation.
    Resume(Uuid),
}

impl SessionArg {
    fn push_to(self, args: &mut Vec<String>) {
        let (flag, id) = match self {
            SessionArg::New(id) => ("--session-id", id),
            SessionArg::Resume(id) => ("--resume", id),
        };
        args.push(flag.to_owned());
        args.push(id.to_string());
    }

    pub fn id(self) -> Uuid {
        match self {
            SessionArg::New(id) | SessionArg::Resume(id) => id,
        }
    }
}

/// Pick the session argument.
///
/// - nothing stored: a fresh id (the caller persists it before the exec);
/// - stored and its transcript exists: resume;
/// - stored but no transcript (for example the user quit during login):
///   start again with the same id.
pub fn choose_session_arg(
    stored: Option<Uuid>,
    transcript_exists: bool,
    fresh: impl FnOnce() -> Uuid,
) -> SessionArg {
    match stored {
        None => SessionArg::New(fresh()),
        Some(id) if transcript_exists => SessionArg::Resume(id),
        Some(id) => SessionArg::New(id),
    }
}

/// Transcript path to probe, relative to `CLAUDE_CONFIG_DIR`. Claude Code
/// names a project's directory after its cwd, which is always `/workspace`.
pub fn transcript_path(config_dir: &str, id: Uuid) -> String {
    format!("{config_dir}/projects/-workspace/{id}.jsonl")
}

/// `tmux new-session -A -s claude -- claude <session args>`.
///
/// `-A` attaches to the existing tmux session if there is one (ignoring the
/// command), otherwise starts Claude Code.
pub fn interactive_command(arg: SessionArg) -> Vec<String> {
    let mut cmd: Vec<String> = ["tmux", "new-session", "-A", "-s", TMUX_SESSION, "--", "claude"]
        .map(str::to_owned)
        .into();
    arg.push_to(&mut cmd);
    cmd
}

/// `claude -p <prompt> --output-format json <session args>`.
pub fn headless_command(prompt: &str, arg: SessionArg) -> Vec<String> {
    let mut cmd: Vec<String> = ["claude", "-p", prompt, "--output-format", "json"]
        .map(str::to_owned)
        .into();
    arg.push_to(&mut cmd);
    cmd
}

/// `tmux has-session -t claude`; exit code 0 means alive.
pub fn tmux_has_session_command() -> Vec<String> {
    ["tmux", "has-session", "-t", TMUX_SESSION].map(str::to_owned).into()
}

/// `tmux detach-client -s claude`.
pub fn tmux_detach_command() -> Vec<String> {
    ["tmux", "detach-client", "-s", TMUX_SESSION].map(str::to_owned).into()
}

/// What a successful headless turn returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessResult {
    pub result: String,
    pub session_id: Option<Uuid>,
}

/// Shown when a headless turn fails because login has not happened.
pub const NOT_LOGGED_IN: &str = "Claude Code is not logged in: attach and log in first";

#[derive(serde::Deserialize)]
struct RawHeadless {
    #[serde(default)]
    result: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    is_error: bool,
}

fn mentions_login(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["/login", "not logged in", "authenticat", "invalid api key", "please log in", "oauth"]
        .iter()
        .any(|needle| t.contains(needle))
}

/// Parse `claude -p --output-format json` output. Pure.
///
/// A non-zero exit, `is_error: true`, or unparseable stdout is
/// [`SandboxError::Headless`] carrying the exit code and stderr. Text that
/// mentions login or authentication maps to [`NOT_LOGGED_IN`].
pub fn parse_headless(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i64>,
) -> Result<HeadlessResult, SandboxError> {
    let fail = |detail: String| SandboxError::Headless {
        exit_code,
        stderr: stderr.to_owned(),
        detail: if mentions_login(&detail) || mentions_login(stderr) {
            NOT_LOGGED_IN.to_owned()
        } else {
            detail
        },
    };
    let parsed = serde_json::from_str::<RawHeadless>(stdout.trim());
    if exit_code != Some(0) {
        let detail = match &parsed {
            Ok(raw) if !raw.result.is_empty() => raw.result.clone(),
            _ => format!("exited with code {}", exit_code.map_or("?".into(), |c| c.to_string())),
        };
        return Err(fail(detail));
    }
    match parsed {
        Ok(raw) if raw.is_error => Err(fail(raw.result)),
        Ok(raw) => Ok(HeadlessResult {
            result: raw.result,
            session_id: raw.session_id.and_then(|s| Uuid::parse_str(&s).ok()),
        }),
        Err(e) => Err(fail(format!("unparseable output: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Uuid = Uuid::from_u128(1);
    const B: Uuid = Uuid::from_u128(2);

    #[test]
    fn no_stored_id_uses_fresh_session_id() {
        assert_eq!(
            choose_session_arg(None, false, || B),
            SessionArg::New(B),
            "nothing stored: start a new conversation with a freshly generated id"
        );
    }

    #[test]
    fn stored_id_with_transcript_resumes() {
        assert_eq!(
            choose_session_arg(Some(A), true, || B),
            SessionArg::Resume(A),
            "stored id with a transcript on disk is resumed"
        );
    }

    #[test]
    fn stored_id_without_transcript_restarts_with_same_id() {
        assert_eq!(
            choose_session_arg(Some(A), false, || B),
            SessionArg::New(A),
            "stored id but no transcript reuses the id via --session-id"
        );
    }

    #[test]
    fn interactive_command_shape() {
        assert_eq!(
            interactive_command(SessionArg::Resume(A)).join(" "),
            format!("tmux new-session -A -s claude -- claude --resume {A}"),
            "interactive runs claude inside the persistent tmux session"
        );
        assert_eq!(
            interactive_command(SessionArg::New(A)).join(" "),
            format!("tmux new-session -A -s claude -- claude --session-id {A}"),
            "new conversations pass --session-id"
        );
    }

    #[test]
    fn headless_command_shape() {
        assert_eq!(
            headless_command("hi there", SessionArg::New(A)),
            ["claude", "-p", "hi there", "--output-format", "json", "--session-id", &A.to_string()],
            "prompt stays one argv element; new conversation uses --session-id"
        );
        assert_eq!(
            headless_command("x", SessionArg::Resume(A)).last().unwrap(),
            &A.to_string(),
            "resume passes the stored id last"
        );
        assert!(
            headless_command("x", SessionArg::Resume(A)).contains(&"--resume".to_owned()),
            "resume uses --resume"
        );
    }

    #[test]
    fn transcript_path_uses_workspace_project_dir() {
        assert_eq!(
            transcript_path("/home/u/.claude", A),
            format!("/home/u/.claude/projects/-workspace/{A}.jsonl"),
            "transcripts live under the cwd-named project directory"
        );
    }

    const SID: &str = "11111111-2222-4333-8444-555555555555";

    #[test]
    fn parse_success() {
        let out = format!(r#"{{"type":"result","is_error":false,"result":"hello","session_id":"{SID}"}}"#);
        let r = parse_headless(&out, "", Some(0)).unwrap();
        assert_eq!(r.result, "hello", "result text extracted");
        assert_eq!(r.session_id, Some(Uuid::parse_str(SID).unwrap()), "session id extracted");
    }

    #[test]
    fn parse_is_error_true_fails() {
        let out = r#"{"is_error":true,"result":"boom"}"#;
        let err = parse_headless(out, "warn", Some(0)).unwrap_err();
        assert!(
            matches!(&err, SandboxError::Headless { exit_code: Some(0), stderr, detail } if stderr == "warn" && detail == "boom"),
            "is_error surfaces the result text with stderr, got {err:?}"
        );
    }

    #[test]
    fn parse_nonzero_exit_fails() {
        let err = parse_headless("", "bad", Some(2)).unwrap_err();
        assert!(
            matches!(&err, SandboxError::Headless { exit_code: Some(2), stderr, .. } if stderr == "bad"),
            "non-zero exit carries code and stderr, got {err:?}"
        );
    }

    #[test]
    fn parse_malformed_json_carries_stderr() {
        let err = parse_headless("not json", "oops", Some(0)).unwrap_err();
        assert!(
            matches!(&err, SandboxError::Headless { stderr, detail, .. } if stderr == "oops" && detail.contains("unparseable")),
            "malformed output is an error carrying stderr, got {err:?}"
        );
    }

    #[test]
    fn parse_not_logged_in_maps_to_attach_message() {
        let out = r#"{"is_error":true,"result":"Invalid API key · Please run /login"}"#;
        let err = parse_headless(out, "", Some(1)).unwrap_err();
        assert!(
            matches!(&err, SandboxError::Headless { detail, .. } if detail == NOT_LOGGED_IN),
            "login failures tell the user to attach and log in, got {err:?}"
        );
    }
}
