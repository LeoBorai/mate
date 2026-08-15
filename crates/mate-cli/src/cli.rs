//! CLI surface used to override config, and to select a frontend (§10, `M5-1`). The tabbed
//! TUI is the eventual default (`M7`); until then `--plain` and `--print` are the only
//! frontends `main` can actually run.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "mate", version, about = "A terminal coding agent")]
pub struct Cli {
    /// One-shot prompt; omit to start an interactive session.
    pub prompt: Option<String>,

    /// Root agent model.
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Subagent model; defaults to the root model if unset.
    #[arg(long)]
    pub subagent_model: Option<String>,

    /// HuggingFace sub-provider.
    #[arg(long)]
    pub provider: Option<String>,

    /// Workspace root; repeat for one tab per path.
    #[arg(short = 'C', long = "dir", value_name = "PATH")]
    pub dir: Vec<PathBuf>,

    /// Line-based stdout, single session.
    #[arg(long)]
    pub plain: bool,

    /// One-shot turn, print, exit.
    #[arg(short = 'p', long, requires = "prompt")]
    pub print: bool,

    /// Disable the http tool.
    #[arg(long)]
    pub no_http: bool,

    /// Disable the spawn_agent tool.
    #[arg(long)]
    pub no_delegate: bool,

    /// Permit the http tool to reach loopback addresses.
    #[arg(long)]
    pub http_allow_localhost: bool,

    /// Maximum concurrent sessions.
    #[arg(long)]
    pub max_sessions: Option<usize>,

    /// Maximum concurrent subagents per session.
    #[arg(long)]
    pub max_subagents: Option<usize>,

    /// Rig `multi_turn` cap.
    #[arg(long)]
    pub max_turns: Option<usize>,

    /// Explicit config file, replacing the `./.mate.toml` lookup.
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_without_a_prompt_is_a_usage_error() {
        let err = Cli::try_parse_from(["mate", "--print"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn print_with_a_prompt_parses() {
        let cli = Cli::parse_from(["mate", "--print", "hello"]);
        assert!(cli.print);
        assert_eq!(cli.prompt.as_deref(), Some("hello"));
    }
}
