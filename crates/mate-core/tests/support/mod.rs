//! Shared scaffolding for `mate-core`'s integration tests (`M4-5`): a `ToolCtx` builder every
//! fs-tool-driven test needs, reused rather than reinvented in each test file. Rig's own
//! `MockCompletionModel`/`MockStreamEvent` (behind the `test-utils` dev-dependency feature)
//! cover the model side; this covers the `mate`-specific context side.

#![allow(dead_code)] // not every test file in this crate uses every helper.

use std::path::PathBuf;
use std::sync::Arc;

use mate_tool_api::{AgentId, ToolCtx};
use mate_tool_http::HttpShared;
use tokio_util::sync::CancellationToken;

/// A `ToolCtx` rooted at `root`, with a generously-sized activity channel and no delegation
/// spawner — the shape every test in this crate that builds a real toolset needs.
pub fn tool_ctx(root: PathBuf) -> ToolCtx {
    let (activity, _rx) = tokio::sync::mpsc::channel(64);
    ToolCtx {
        agent: AgentId::ROOT,
        root,
        max_output_bytes: 1_000_000,
        spawner: None,
        activity,
        cancel: CancellationToken::new(),
        approvals: None,
    }
}

/// The process-wide HTTP state `build_agent`/`build_toolset` now require (`M10`) — a fresh,
/// offline-safe instance per test, since nothing here is actually shared across a real
/// process.
pub fn http_shared() -> Arc<HttpShared> {
    Arc::new(HttpShared::new(60).expect("offline construction never touches the network"))
}
