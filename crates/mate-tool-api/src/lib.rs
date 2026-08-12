//! Shared contracts every `mate-tool-*` crate builds on: `ToolCtx` (workspace root,
//! output caps, approval channel), the `ToolFailure` error type, and the
//! `SubagentSpawner` trait — the seam that lets `mate-tool-agent` spawn agents
//! without depending on `mate-core`, keeping the dependency graph one-directional.
