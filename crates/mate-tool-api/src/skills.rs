//! [`SkillMetadata`] — one discovered Skill's level-1 metadata (name, description, and its
//! workspace-root-relative directory), carried on [`crate::ToolCtx`] so `mate-tool-skills`'s
//! `skill` tool can look a name up without re-walking the filesystem on every call, and so
//! `mate-core`'s preamble rendering can list what's available without depending on the tool
//! crate that discovers them. Defined here, not in `mate-tool-skills`, for the same reason
//! [`crate::AgentId`] and [`crate::SubagentSpawner`] are: [`crate::ToolCtx`] needs the type,
//! and this crate can never depend on a tool crate.

use std::path::PathBuf;

/// One Skill's discovery-time metadata — Anthropic's "level 1": name and description are all
/// that ever load into the preamble unconditionally. The full `SKILL.md` body loads on demand,
/// by name, through the `skill` tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    /// The skill's own directory, relative to the workspace root — e.g.
    /// `.claude/skills/pdf-processing`. Joined with the workspace root to locate `SKILL.md`
    /// and any bundled resource file the skill's body references.
    pub dir: PathBuf,
}
