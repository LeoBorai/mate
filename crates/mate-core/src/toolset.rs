//! `build_toolset` (`M4-1`, §8.1): assembles the toolset for one agent — root or subagent —
//! from its [`ToolCtx`]. Called once per agent, by [`crate::agent::build_agent`].
//!
//! Returns a `ToolServerHandle` rather than a bare `ToolSet`: `AgentBuilder::tool_server_handle`
//! is generic over the provider model (unlike `.tool()`, which pins the builder's typestate),
//! so the same handle attaches to either of [`crate::backend::Backend`]'s provider paths without
//! rebuilding the toolset per variant.
//!
//! Only `mate-tool-fs`'s tools exist today (`M3`) and are attached unconditionally — nothing in
//! `AgentSpec` disables filesystem access. `M9`'s `spawn_agent` and `M10`'s `http_request` attach
//! here too once those crates ship a real `Tool` impl, gated on `spec.may_delegate` and
//! `spec.http.enabled` respectively. `ToolServer` makes "disabled tools are absent from the
//! agent's definitions" (`M4-1`'s acceptance criterion) a structural guarantee rather than
//! something to test per tool: a tool never `.tool()`-ed onto the server cannot appear in
//! [`rig::tool::server::ToolServerHandle::get_tool_defs`], so a conditional `if spec.http.enabled
//! { builder = builder.tool(..) }` is sufficient on its own, with no separate call-time check
//! needed.

use mate_tool_api::ToolCtx;
use rig::tool::server::{ToolServer, ToolServerHandle};

pub fn build_toolset(ctx: ToolCtx) -> ToolServerHandle {
    ToolServer::new()
        .tool(mate_tool_fs::ReadFile::new(ctx.clone()))
        .tool(mate_tool_fs::ListDir::new(ctx.clone()))
        .tool(mate_tool_fs::FindFiles::new(ctx))
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx(root: std::path::PathBuf) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: mate_tool_api::AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn attaches_every_fs_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = build_toolset(ctx(tmp.path().to_path_buf()));

        let mut names: Vec<String> = handle
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();
        names.sort();

        assert_eq!(names, vec!["find_files", "list_dir", "read_file"]);
    }
}
