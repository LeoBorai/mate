//! `build_toolset` (`M4-1`, §8.1): assembles the toolset for one agent — root or subagent —
//! from its [`ToolCtx`]. Called once per agent, by [`crate::agent::build_agent`].
//!
//! Returns a `ToolServerHandle` rather than a bare `ToolSet`: `AgentBuilder::tool_server_handle`
//! is generic over the provider model (unlike `.tool()`, which pins the builder's typestate),
//! so the same handle attaches to either of [`crate::backend::Backend`]'s provider paths without
//! rebuilding the toolset per variant.
//!
//! Only `mate-tool-fs`'s tools exist today (`M3`) and are attached unconditionally — nothing in
//! `AgentSpec` disables filesystem access. `M10`'s `http_request` attaches here too once that
//! crate ships a real `Tool` impl, gated on `spec.http.enabled`. `M9-3`'s `spawn_agent` attaches
//! whenever `ctx.spawner.is_some()` — `SessionManager::spawn` (`M9-2`) only ever sets a spawner
//! when the agent's spec has `may_delegate: true`, so this one check is equivalent to gating on
//! that flag directly, and it's also what makes a subagent's own `ToolCtx` (always built with
//! `spawner: None` past the configured delegation depth, §7.4) end up with no spawn tool at all
//! — the `M9-4` guardrail the type system enforces rather than a runtime check. `ToolServer`
//! makes "disabled tools are absent from the agent's definitions" (`M4-1`'s acceptance
//! criterion) a structural guarantee rather than something to test per tool: a tool never
//! `.tool()`-ed onto the server cannot appear in
//! [`rig::tool::server::ToolServerHandle::get_tool_defs`], so a conditional
//! `if condition { builder = builder.tool(..) }` is sufficient on its own, with no separate
//! call-time check needed.

use mate_tool_api::ToolCtx;
use rig::tool::server::{ToolServer, ToolServerHandle};

use crate::preamble::ToolDescriptor;

pub fn build_toolset(ctx: ToolCtx) -> ToolServerHandle {
    let mut builder = ToolServer::new()
        .tool(mate_tool_fs::ReadFile::new(ctx.clone()))
        .tool(mate_tool_fs::ListDir::new(ctx.clone()))
        .tool(mate_tool_fs::FindFiles::new(ctx.clone()));
    if ctx.spawner.is_some() {
        builder = builder.tool(mate_tool_agent::SpawnAgent::new(ctx));
    }
    builder.run()
}

/// Descriptors for the tools [`build_toolset`] attaches, for preamble rendering (§4, `M1-4`)
/// until the promised "derive from the real `ToolSet`" wiring lands — kept next to
/// `build_toolset` so the two lists can't drift apart. `may_delegate` must match whatever
/// `build_toolset` was (or, for a not-yet-built agent, will be) called with, the same way a
/// caller already threads one `may_delegate` value into both `AgentSpec::may_delegate` and this
/// function (`crate::subagent` does the same for a subagent's own preamble).
pub fn tool_descriptors(may_delegate: bool) -> Vec<ToolDescriptor> {
    let mut descriptors = vec![
        ToolDescriptor::new(
            "read_file",
            "Read a file inside the workspace. Output is line-numbered. Use start_line/end_line \
             to read a slice of a large file instead of the whole thing.",
        ),
        ToolDescriptor::new(
            "list_dir",
            "List one level of a directory inside the workspace. Respects .gitignore. \
             Directory entries are suffixed with '/'.",
        ),
        ToolDescriptor::new(
            "find_files",
            "Find files under the workspace root matching a glob pattern, e.g. \"**/*.rs\". \
             Respects .gitignore.",
        ),
    ];
    if may_delegate {
        descriptors.push(ToolDescriptor::new(
            "spawn_agent",
            "Delegate a narrow, self-contained task to a subordinate agent with its own, \
             EMPTY context window. It does not see this conversation and cannot ask \
             follow-up questions — restate every piece of context the task needs. It \
             returns a short report, not raw output.",
        ));
    }
    descriptors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use mate_tool_api::{SubagentReport, SubagentRequest, SubagentSpawner, ToolFailure};
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

    /// Never actually called in this module's tests — only its presence in `ctx.spawner`
    /// matters, to prove `spawn_agent` attaches whenever a spawner is set.
    struct StubSpawner;

    #[async_trait]
    impl SubagentSpawner for StubSpawner {
        async fn run(&self, _request: SubagentRequest) -> Result<SubagentReport, ToolFailure> {
            unimplemented!("not called by this module's tests")
        }
    }

    #[tokio::test]
    async fn attaches_every_fs_tool_but_not_spawn_agent_without_a_spawner() {
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

        assert_eq!(
            names,
            vec!["find_files", "list_dir", "read_file"],
            "spawn_agent must be absent from the toolset when ctx.spawner is None"
        );
    }

    #[tokio::test]
    async fn attaches_spawn_agent_when_the_context_carries_a_spawner() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.spawner = Some(Arc::new(StubSpawner) as Arc<dyn SubagentSpawner>);
        let handle = build_toolset(c);

        let mut names: Vec<String> = handle
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();
        names.sort();

        assert_eq!(
            names,
            vec!["find_files", "list_dir", "read_file", "spawn_agent"],
            "spawn_agent must attach whenever ctx.spawner is Some, regardless of caller"
        );
    }

    #[test]
    fn tool_descriptors_match_the_attached_toolset_without_delegation() {
        let descriptors = tool_descriptors(false);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["find_files", "list_dir", "read_file"],
            "descriptors must match build_toolset's own attachment set for may_delegate: false"
        );
    }

    #[test]
    fn tool_descriptors_include_spawn_agent_when_delegation_is_enabled() {
        let descriptors = tool_descriptors(true);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["find_files", "list_dir", "read_file", "spawn_agent"],
            "descriptors must match build_toolset's own attachment set for may_delegate: true"
        );
    }
}
