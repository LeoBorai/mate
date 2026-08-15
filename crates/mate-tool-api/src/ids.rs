//! Identifiers shared between `mate-core`'s event/session model and tool contracts.
//!
//! [`AgentId`] lives here rather than in `mate-core` because [`crate::ToolCtx`] and
//! [`crate::ToolActivity`] must carry it too, and `mate-tool-api` can never depend on
//! `mate-core` (§8.1 note 1) — `mate-core` imports this definition instead of owning
//! a second one. `SessionId` stays out until `M6`'s session manager gives it a
//! producer (the same reasoning `mate-core::streaming` already applied to it).

/// Index of an agent within a session (§5.1). `0` is always the root agent; every
/// other value is a subordinate spawned via `spawn_agent` (`M9`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct AgentId(pub u32);

impl AgentId {
    pub const ROOT: AgentId = AgentId(0);
}
