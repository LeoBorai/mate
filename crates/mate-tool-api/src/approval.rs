//! [`Approvals`] (§6.2-shaped, §7.4, `M13-1`): the seam a tool uses to ask a human for a binary
//! yes/no on a risky action, without depending on `mate-core` (§8.1 note 1) — same pattern as
//! [`crate::SubagentSpawner`]: the capability is a trait here, implemented in `mate-core` against
//! the session's own event/command channels, and injected into [`crate::ToolCtx`] at
//! construction.
//!
//! `mate-tool-fs`'s `write_file` is the first caller: every write is gated through `request`
//! before the file ever touches disk. The seam otherwise predates any producer for the same
//! reason `SubagentSpawner` did before `M9` had a real implementation: a field added to
//! `ToolCtx` later is a breaking change to every call site that constructs one, a field added
//! now and left `None` in every existing construction is not.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::AgentId;

/// One approval request (§7.4): `agent` is who's asking (root or a subagent, so the UI can
/// label a subagent's request distinctly — "subagent `deps` wants to POST to …"), `name` is the
/// tool/action being gated, and `detail` is the human-readable specifics shown alongside it.
/// `path` carries the resolved, in-jail target of a filesystem action, when there is one — an
/// `Approvals` implementation can use it to remember a scope (e.g. "always allow writes under
/// this directory") without that scope ever being free text a human typed, which would be the
/// back door `detail` alone is deliberately not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub agent: AgentId,
    pub name: String,
    pub detail: String,
    pub path: Option<PathBuf>,
}

/// Requests a binary decision on one risky action and blocks until it's made. The answer is
/// always a plain `bool` — never a typed reason — because a free-text denial would be a back
/// door around §7.6: whatever a human typed would otherwise need to land somewhere in the
/// calling agent's context, and a subagent's context is exactly what §7.6 says nothing but
/// cancellation may reach into. `false` covers both an explicit "no" and an auto-deny timeout;
/// a tool has no way to tell the two apart, and doesn't need to.
#[async_trait]
pub trait Approvals: Send + Sync {
    async fn request(&self, request: ApprovalRequest) -> bool;
}
