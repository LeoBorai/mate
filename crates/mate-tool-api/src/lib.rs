//! Shared contracts every `mate-tool-*` crate builds on: [`ToolCtx`] (workspace root,
//! output caps, approval channel), the [`ToolFailure`] error type, and the
//! [`SubagentSpawner`] trait — the seam that lets `mate-tool-agent` spawn agents without
//! depending on `mate-core`, keeping the dependency graph one-directional.
//!
//! [`AgentId`] lives here rather than in `mate-core` for the same reason: [`ToolCtx`] and
//! [`ToolActivity`] both need to carry it, and this crate can never depend on `mate-core`.

mod activity;
mod agents_md;
mod approval;
mod ctx;
mod error;
mod ids;
mod skills;
mod subagent;

pub use activity::{ActivitySink, DiffLine, DiffTag, FileOp, ToolActivity};
pub use agents_md::AgentsMdSource;
pub use approval::{ApprovalRequest, Approvals};
pub use ctx::ToolCtx;
pub use error::{ToolFailure, enforce_max_size, number_lines, refuse_binary, truncate_with_notice};
pub use ids::AgentId;
pub use skills::SkillMetadata;
pub use subagent::{
    SubagentOutcome, SubagentReport, SubagentRequest, SubagentSpawner, ToolProfile,
};
