//! Claude Code command construction (and, later, headless output parsing).

use uuid::Uuid;

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
}
