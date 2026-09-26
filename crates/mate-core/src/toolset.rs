//! `build_toolset` (`M4-1`, §8.1): assembles the toolset for one agent — root or subagent —
//! from its [`ToolCtx`]. Called once per agent, by [`crate::agent::build_agent`].
//!
//! Returns a `ToolServerHandle` rather than a bare `ToolSet`: `AgentBuilder::tool_server_handle`
//! is generic over the provider model (unlike `.tool()`, which pins the builder's typestate),
//! so the same handle attaches to either of [`crate::backend::Backend`]'s provider paths without
//! rebuilding the toolset per variant.
//!
//! `mate-tool-fs`'s tools (`M3`) are attached unconditionally — nothing in `AgentSpec` disables
//! filesystem access. `M10`'s `http_request` attaches whenever `http_policy.enabled`, built
//! against the process-wide `HttpShared` every agent in the process shares (§5.3) and narrowed
//! to `http_policy.policy == HttpAccessPolicy::AllowLocalhost` the same way every other
//! per-agent narrowing in this function works. `M9-3`'s `spawn_agent` attaches
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

use std::sync::Arc;

use mate_tool_api::ToolCtx;
use mate_tool_http::HttpShared;
use mate_tool_mcp::McpServers;
use rig::tool::server::{ToolServer, ToolServerHandle};

use crate::config::{HttpAccessPolicy, HttpPolicy};
use crate::preamble::ToolDescriptor;

/// `mcp` is `Option<&Arc<McpServers>>`, not a plain `Arc<McpServers>`, so a subagent's toolset
/// can never attach `mcp` even by accident: `crate::subagent::SubagentRunner` never holds an
/// `Arc<McpServers>` at all, so its own `build_agent`/`build_toolset` call sites structurally
/// only ever have `None` to pass — the same "type system enforces it" shape `M9-4`'s
/// non-addressability guardrail already uses, rather than a boolean flag someone could set
/// wrong (spec: "No MCP tools for subagents").
pub fn build_toolset(
    ctx: ToolCtx,
    http_policy: &HttpPolicy,
    http_shared: Arc<HttpShared>,
    mcp: Option<&Arc<McpServers>>,
) -> ToolServerHandle {
    let mut builder = ToolServer::new()
        .tool(mate_tool_fs::ReadFile::new(ctx.clone()))
        .tool(mate_tool_fs::ListDir::new(ctx.clone()))
        .tool(mate_tool_fs::FindFiles::new(ctx.clone()))
        .tool(mate_tool_fs::WriteFile::new(ctx.clone()));
    if http_policy.enabled {
        let allow_localhost = http_policy.policy == HttpAccessPolicy::AllowLocalhost;
        builder = builder.tool(mate_tool_http::HttpRequest::new(
            ctx.clone(),
            http_shared,
            allow_localhost,
        ));
    }
    if !ctx.skills.is_empty() {
        builder = builder.tool(mate_tool_skills::Skill::new(ctx.clone()));
    }
    if let Some(servers) = mcp.filter(|servers| servers.has_active_servers()) {
        builder = builder.tool(mate_tool_mcp::McpProxy::new(ctx.clone(), servers.clone()));
    }
    if ctx.spawner.is_some() {
        builder = builder.tool(mate_tool_agent::SpawnAgent::new(ctx));
    }
    builder.run()
}

/// Descriptors for the tools [`build_toolset`] attaches, for preamble rendering (§4, `M1-4`)
/// until the promised "derive from the real `ToolSet`" wiring lands — kept next to
/// `build_toolset` so the two lists can't drift apart. `may_delegate`/`http_enabled` must match
/// whatever `build_toolset` was (or, for a not-yet-built agent, will be) called with, the same
/// way a caller already threads one `may_delegate` value into both `AgentSpec::may_delegate` and
/// this function (`crate::subagent` does the same for a subagent's own preamble and its own
/// narrowed `http.enabled`).
pub fn tool_descriptors(
    may_delegate: bool,
    http_enabled: bool,
    skills_enabled: bool,
    mcp_enabled: bool,
) -> Vec<ToolDescriptor> {
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
        ToolDescriptor::new(
            "write_file",
            "Create or overwrite a file inside the workspace with the given full contents. \
             The containing directory must already exist. Every write requires human \
             approval before it happens.",
        ),
    ];
    if http_enabled {
        descriptors.push(ToolDescriptor::new(
            "http_request",
            "Fetch a URL over HTTP or HTTPS. Only GET and HEAD are supported; mutating \
             methods are refused. Requests to private, loopback, link-local, and other \
             non-public addresses are blocked. HTML responses are converted to readable text \
             and JSON is pretty-printed by default — set render_text to false for the raw \
             body. Output leads with the status, final URL (after any redirects), content \
             type, and redirect count.",
        ));
    }
    if skills_enabled {
        descriptors.push(ToolDescriptor::new(
            "skill",
            "Load the full instructions for a skill named in the \"Available skills\" list. \
             Returns the skill's own directory (read any file it bundles with \
             read_file/find_files) followed by its complete instructions.",
        ));
    }
    if mcp_enabled {
        descriptors.push(ToolDescriptor::new(
            "mcp",
            "Call a tool exposed by a configured MCP (Model Context Protocol) server. Pass \
             the server name, the tool name, and arguments matching that tool's schema — see \
             the tool's own description for which servers and tools are currently available.",
        ));
    }
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
    use mate_tool_api::{
        SkillMetadata, SubagentReport, SubagentRequest, SubagentSpawner, ToolFailure,
    };
    use mate_tool_mcp::{McpServers, ServerSpec};
    use tokio_util::sync::CancellationToken;

    fn ctx(root: std::path::PathBuf) -> ToolCtx {
        ctx_with_skills(root, Vec::new())
    }

    /// A registry with zero configured servers — `has_active_servers()` is always `false`, the
    /// same "no servers configured" starting point every process without an `[[mcp.servers]]`
    /// entry has.
    async fn empty_mcp() -> Arc<McpServers> {
        Arc::new(McpServers::connect(Vec::new()).await)
    }

    /// A registry with one server whose command never speaks MCP (`false`, a real but silent
    /// process) — it fails to initialize, so `has_active_servers()` stays `false` the same way
    /// `empty_mcp` is, but exercises the "configured yet still absent" path rather than the
    /// "nothing configured at all" one. Good enough for `build_toolset`'s attachment tests,
    /// which only care whether the `mcp` tool is present or absent, not what it can route to.
    #[cfg(unix)]
    async fn failing_mcp() -> Arc<McpServers> {
        Arc::new(
            McpServers::connect(vec![ServerSpec {
                name: "silent".to_string(),
                command: "false".to_string(),
                args: Vec::new(),
                env: std::collections::HashMap::new(),
                allow: Vec::new(),
            }])
            .await,
        )
    }

    fn ctx_with_skills(root: std::path::PathBuf, skills: Vec<SkillMetadata>) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: mate_tool_api::AgentId::ROOT,
            root,
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: CancellationToken::new(),
            approvals: None,
            skills: Arc::from(skills),
            agents_md: None,
        }
    }

    fn a_skill() -> SkillMetadata {
        SkillMetadata {
            name: "demo".to_string(),
            description: "A demo skill.".to_string(),
            dir: std::path::PathBuf::from(".claude/skills/demo"),
        }
    }

    fn http_shared() -> Arc<HttpShared> {
        Arc::new(HttpShared::new(60).unwrap())
    }

    fn http_policy(enabled: bool) -> HttpPolicy {
        HttpPolicy {
            enabled,
            ..HttpPolicy::default()
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
    async fn attaches_every_fs_tool_and_http_but_not_spawn_agent_without_a_spawner() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = build_toolset(
            ctx(tmp.path().to_path_buf()),
            &http_policy(true),
            http_shared(),
            None,
        );

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
            vec![
                "find_files",
                "http_request",
                "list_dir",
                "read_file",
                "write_file"
            ],
            "spawn_agent must be absent from the toolset when ctx.spawner is None"
        );
    }

    #[tokio::test]
    async fn does_not_attach_http_request_when_the_policy_disables_it() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = build_toolset(
            ctx(tmp.path().to_path_buf()),
            &http_policy(false),
            http_shared(),
            None,
        );

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
            vec!["find_files", "list_dir", "read_file", "write_file"]
        );
    }

    #[tokio::test]
    async fn attaches_spawn_agent_when_the_context_carries_a_spawner() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.spawner = Some(Arc::new(StubSpawner) as Arc<dyn SubagentSpawner>);
        let handle = build_toolset(c, &http_policy(true), http_shared(), None);

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
            vec![
                "find_files",
                "http_request",
                "list_dir",
                "read_file",
                "spawn_agent",
                "write_file"
            ],
            "spawn_agent must attach whenever ctx.spawner is Some, regardless of caller"
        );
    }

    #[tokio::test]
    async fn attaches_skill_when_the_context_carries_discovered_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = build_toolset(
            ctx_with_skills(tmp.path().to_path_buf(), vec![a_skill()]),
            &http_policy(false),
            http_shared(),
            None,
        );

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
            vec!["find_files", "list_dir", "read_file", "skill", "write_file"],
            "skill must attach whenever ctx.skills is non-empty"
        );
    }

    #[test]
    fn tool_descriptors_match_the_attached_toolset_without_delegation_http_or_skills() {
        let descriptors = tool_descriptors(false, false, false, false);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["find_files", "list_dir", "read_file", "write_file"],
            "descriptors must match build_toolset's own attachment set for may_delegate: false, \
             http_enabled: false, skills_enabled: false"
        );
    }

    #[test]
    fn tool_descriptors_include_http_request_when_enabled() {
        let descriptors = tool_descriptors(false, true, false, false);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "find_files",
                "http_request",
                "list_dir",
                "read_file",
                "write_file"
            ]
        );
    }

    #[test]
    fn tool_descriptors_include_skill_when_skills_are_enabled() {
        let descriptors = tool_descriptors(false, false, true, false);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["find_files", "list_dir", "read_file", "skill", "write_file"]
        );
    }

    #[test]
    fn tool_descriptors_include_spawn_agent_when_delegation_is_enabled() {
        let descriptors = tool_descriptors(true, true, false, false);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "find_files",
                "http_request",
                "list_dir",
                "read_file",
                "spawn_agent",
                "write_file"
            ],
            "descriptors must match build_toolset's own attachment set for may_delegate: true, \
             http_enabled: true, skills_enabled: false"
        );
    }

    #[test]
    fn tool_descriptors_include_mcp_when_enabled() {
        let descriptors = tool_descriptors(false, false, false, true);
        let mut names: Vec<&str> = descriptors.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["find_files", "list_dir", "mcp", "read_file", "write_file"]
        );
    }

    // --- `mcp` attachment (`M`-shaped, `add-mcp-support` §5.2): zero servers, servers present \
    // but none ready, and a subagent's structural exclusion ------------------------------------

    #[tokio::test]
    async fn does_not_attach_mcp_when_no_registry_is_passed() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = build_toolset(
            ctx(tmp.path().to_path_buf()),
            &http_policy(false),
            http_shared(),
            None,
        );

        let names: Vec<String> = handle
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();

        assert!(
            !names.contains(&"mcp".to_string()),
            "mcp must be absent when build_toolset is called with mcp: None — the only way a \
             subagent's own build_toolset call site is structurally guaranteed to behave, since \
             SubagentRunner never holds an Arc<McpServers> to pass Some with"
        );
    }

    #[tokio::test]
    async fn does_not_attach_mcp_when_the_registry_has_no_configured_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let mcp = empty_mcp().await;
        let handle = build_toolset(
            ctx(tmp.path().to_path_buf()),
            &http_policy(false),
            http_shared(),
            Some(&mcp),
        );

        let names: Vec<String> = handle
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();

        assert!(
            !names.contains(&"mcp".to_string()),
            "mcp must be absent when the registry has zero configured servers (spec: \"No \
             servers configured\")"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn does_not_attach_mcp_when_every_configured_server_failed_to_initialize() {
        let tmp = tempfile::tempdir().unwrap();
        let mcp = failing_mcp().await;
        let handle = build_toolset(
            ctx(tmp.path().to_path_buf()),
            &http_policy(false),
            http_shared(),
            Some(&mcp),
        );

        let names: Vec<String> = handle
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();

        assert!(
            !names.contains(&"mcp".to_string()),
            "mcp must be absent when every configured server failed to initialize (spec: \"No \
             servers configured or initialized\")"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attaches_mcp_when_the_registry_has_a_ready_server() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = mate_tool_mcp::ServerHandle::test_ready("demo", Vec::new(), Vec::new());
        let mcp = Arc::new(McpServers::test_with_ready(handle));
        let toolset = build_toolset(
            ctx(tmp.path().to_path_buf()),
            &http_policy(false),
            http_shared(),
            Some(&mcp),
        );

        let names: Vec<String> = toolset
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();

        assert!(
            names.contains(&"mcp".to_string()),
            "mcp must attach for a root agent once the registry has at least one ready server"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_subagent_style_ctx_still_gets_no_mcp_tool_even_with_a_ready_registry() {
        // `SubagentRunner::run` (`crate::subagent`) never holds an `Arc<McpServers>` at all, so
        // its own `build_agent`/`build_toolset` call site can only ever pass `None` here — this
        // test proves the `None` side of that guarantee does what the spec requires, standing
        // in for a real subagent ToolCtx (which needs a whole SubagentRunner to construct).
        let tmp = tempfile::tempdir().unwrap();
        let handle = mate_tool_mcp::ServerHandle::test_ready("demo", Vec::new(), Vec::new());
        let mcp = Arc::new(McpServers::test_with_ready(handle));
        let mut subagent_ctx = ctx(tmp.path().to_path_buf());
        subagent_ctx.agent = mate_tool_api::AgentId(1);

        let toolset = build_toolset(subagent_ctx, &http_policy(false), http_shared(), None);

        let names: Vec<String> = toolset
            .get_tool_defs(None)
            .await
            .unwrap()
            .into_iter()
            .map(|def| def.name)
            .collect();

        assert!(
            !names.contains(&"mcp".to_string()),
            "no mcp tool must reach a subagent's toolset, spec's \"No MCP tools for subagents\", \
             regardless of the registry a caller might otherwise have on hand"
        );
    }
}
