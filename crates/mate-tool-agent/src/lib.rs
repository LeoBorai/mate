//! The `spawn_agent` tool: lets a root agent delegate a narrow task to a
//! subordinate agent inside the same session. The subagent gets its own context
//! window and returns a short report, never a raw dump — the whole point is
//! keeping the parent's context small. Subagents are not addressable by the user
//! and cannot spawn further subagents by default.
