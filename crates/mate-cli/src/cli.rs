//! CLI surface used to override config, and to select a frontend (§10, `M5-1`). The tabbed
//! TUI (`M7`/`M8`) is the default; `--plain` and `--print` select the line-based frontend
//! instead. The one subcommand, `mate specviz`, runs the spec viewer in the foreground and
//! skips every agent-facing flag (clap rejects them alongside it).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::BackendKind;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "mate",
    version,
    about = "A terminal coding agent",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// One-shot prompt; omit to start an interactive session.
    pub prompt: Option<String>,

    /// Root agent model.
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Subagent model; defaults to the root model if unset.
    #[arg(long)]
    pub subagent_model: Option<String>,

    /// Provider backend to talk to.
    #[arg(long)]
    pub backend: Option<BackendKind>,

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

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Serve this workspace's specs/ and openspec/ as a live-reloading web UI.
    Specviz {
        /// Directory to serve.
        #[arg(short = 'C', long = "dir", value_name = "PATH", default_value = ".")]
        dir: PathBuf,

        /// Loopback port to listen on.
        #[arg(long, default_value_t = mate_specviz_server::DEFAULT_PORT)]
        port: u16,
    },
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

    #[test]
    fn bare_specviz_selects_the_subcommand_with_defaults() {
        let cli = Cli::parse_from(["mate", "specviz"]);
        match cli.command {
            Some(Command::Specviz { dir, port }) => {
                assert_eq!(dir, PathBuf::from("."), "serves the current directory by default");
                assert_eq!(port, 7732, "the default port is 7732");
            }
            other => panic!("expected the specviz subcommand, got {other:?}"),
        }
        assert_eq!(cli.prompt, None, "the subcommand is not a one-shot prompt");
    }

    #[test]
    fn specviz_takes_a_dir_and_port() {
        let cli = Cli::parse_from(["mate", "specviz", "-C", "../other", "--port", "9000"]);
        match cli.command {
            Some(Command::Specviz { dir, port }) => {
                assert_eq!(dir, PathBuf::from("../other"), "-C overrides the served root");
                assert_eq!(port, 9000, "--port overrides the default");
            }
            other => panic!("expected the specviz subcommand, got {other:?}"),
        }
    }

    #[test]
    fn a_quoted_prompt_mentioning_specviz_is_still_a_prompt() {
        let cli = Cli::parse_from(["mate", "specviz is not loading"]);
        assert!(cli.command.is_none(), "only the bare word selects the subcommand");
        assert_eq!(
            cli.prompt.as_deref(),
            Some("specviz is not loading"),
            "the whole argument is the prompt"
        );
    }

    #[test]
    fn agent_flags_are_not_specviz_flags() {
        assert!(
            Cli::try_parse_from(["mate", "specviz", "--model", "m"]).is_err(),
            "--model after the subcommand is not one of its flags"
        );
    }
}
