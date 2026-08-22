//! Agent Skills (issue #59): filesystem-discovered `SKILL.md` packages under
//! `.claude/skills/`, `.opencode/skills/`, `.copilot/skills/`, and `.agents/skills/`, loaded
//! on demand by the `skill` tool this crate exports.
//!
//! `mate` has no bash/exec tool for a model to `cat SKILL.md` itself the way Claude Code does,
//! so — closer to how OpenCode does it — level 1 (name + description) is discovered once at
//! session-build time by [`discover_skills`] and rendered into the preamble
//! (`mate_core::preamble::render_preamble`'s "Available skills" section), and level 2 (the full
//! body) loads through [`Skill`], a dedicated [`rig::tool::PortableTool`]. Level 3 (bundled
//! resource files a skill's body references) rides the `read_file`/`find_files` tools `mate`
//! already has — this crate's tool output tells the model the skill's own directory so it can
//! build a correct path. Bundled *scripts* are out of scope: nothing in `mate` can execute one.
//!
//! `mate-tool-skills → mate-tool-api` only, never `mate-core` — the same one-directional
//! dependency graph every other `mate-tool-*` crate keeps.

mod discovery;
mod frontmatter;
mod skill;

pub use discovery::discover_skills;
pub use skill::{Skill, SkillArgs};
