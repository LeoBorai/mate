//! Typed telemetry a tool emits alongside its return value (§9.3), so the TUI's status
//! panel (`M12`) can show "`read_file` touched `src/build.rs` at 210 lines" without
//! parsing a tool's rendered output string back into structure.

use std::path::PathBuf;

use http::Method;
use tokio::sync::mpsc;

use crate::AgentId;

/// One typed record of a tool doing something worth showing in the status panel.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolActivity {
    FileTouched {
        path: PathBuf,
        op: FileOp,
        lines: usize,
        bytes: usize,
    },
    NetRequest {
        method: Method,
        host: String,
        path: String,
        status: Option<u16>,
        ms: u64,
        bytes: usize,
        redirects: u8,
        /// Why the request never went out — an SSRF guard tripping (§8.2), for instance.
        /// `None` on a request that actually reached a server, `status` included.
        reason: Option<String>,
    },
    /// Free-form note, kept to ~60 chars for the subagent activity line.
    Note { text: String },
}

/// `Write`/`Create`/`Delete` exist now, ahead of any producer, so `mate-tool-edit` (§14)
/// doesn't have to reopen this enum later — the same reasoning as
/// `mate-core::streaming::SubagentOutcome`. `mate-tool-fs`'s `M3` tools emit `Read` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    Read,
    Write,
    Create,
    Delete,
}

/// Fire-and-forget sink for [`ToolActivity`], tagged by the agent that produced it so
/// the panel can show a whole session's activity, root and subagents together. Senders
/// use `try_send`: dropping a telemetry record under backpressure is strictly better
/// than stalling a tool call to report on itself.
pub type ActivitySink = mpsc::Sender<(AgentId, ToolActivity)>;
