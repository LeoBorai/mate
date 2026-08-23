//! [`AgentsMdSource`] — one discovered project-instructions file's name and content, carried on
//! [`crate::ToolCtx`] so `mate_core::session::SessionManager::spawn` can hand it to
//! `SubagentRunner` without that runner ever re-reading the filesystem. Defined here, not in
//! `mate-core`, for the same reason [`crate::SkillMetadata`] is: [`crate::ToolCtx`] needs the
//! type, and this crate can never depend on `mate-core`.

/// One discovered project-instructions file — `AGENTS.md`, `CLAUDE.md`, or another agent
/// framework's filename for the same concept — as found by
/// `mate_core::agents_md::discover_agents_md`. `content` is already size-capped by the time it
/// reaches here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsMdSource {
    pub filename: &'static str,
    pub content: String,
}
