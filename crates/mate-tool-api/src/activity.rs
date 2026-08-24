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
    /// `mate-tool-skills`'s `skill` tool loaded a skill's full instructions. A structured
    /// variant rather than folding into `Note` — the panel's SKILLS widget needs the exact
    /// `name` to flip that skill's row from an empty circle to a green dot, and string-matching
    /// a `Note`'s free text back into a name would be fragile.
    SkillLoaded { name: String },
}

/// `mate-tool-fs`'s `read_file`/`list_dir`/`find_files` (`M3`) emit `Read`; `write_file` emits
/// `Write` (overwrite) or `Create` (new file). `Delete` has no producer yet — it was defined
/// ahead of one the same way `Write`/`Create` were before `write_file` landed, the same
/// reasoning as `mate-core::streaming::SubagentOutcome`.
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
