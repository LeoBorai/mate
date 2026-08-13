//! CLI surface used to override config (§10 of `plan.md`). The full frontend-selection
//! behavior (`--plain`, `--print`, signal handling, …) lands with `M5-1`; this subset exists
//! so config loading has real flags to layer on top of env/files/defaults.

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
    #[arg(short = 'p', long)]
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
