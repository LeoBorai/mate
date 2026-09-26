//! [`McpProxy`]: the single tool the model sees for every configured MCP server combined.
//!
//! `PortableTool::NAME` is a compile-time `const &'static str` with no per-instance override
//! (verified against every existing tool in this workspace — `read_file`, `write_file`,
//! `list_dir`, `find_files`, `skill`). N runtime-configured MCP servers can't each get a
//! distinctly-`const`-named Rust type, so this tool mirrors `mate-tool-skills::Skill` exactly:
//! one tool, dispatching over a runtime-discovered set by an argument, rather than one
//! statically-named tool per server or per underlying MCP tool.

use std::sync::Arc;
use std::time::Instant;

use mate_tool_api::{ToolActivity, ToolCtx, ToolFailure};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::servers::McpServers;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct McpArgs {
    /// Name of a configured MCP server, as listed in this tool's own description.
    pub server: String,
    /// Name of a tool that server advertises and allow-lists, as listed in this tool's own
    /// description.
    pub tool: String,
    /// Arguments for that tool, matching its advertised input schema.
    #[serde(default)]
    pub arguments: Value,
}

/// Routes a call to one configured MCP server's tool. One instance is built per agent (root
/// only — never a subagent, structurally: `mate_core::subagent::SubagentRunner` never holds an
/// `Arc<McpServers>` to construct one from) in `mate-core::toolset::build_toolset`, the same way
/// `HttpRequest`/`Skill` are.
pub struct McpProxy {
    ctx: ToolCtx,
    servers: Arc<McpServers>,
}

impl McpProxy {
    pub fn new(ctx: ToolCtx, servers: Arc<McpServers>) -> Self {
        Self { ctx, servers }
    }
}

impl PortableTool for McpProxy {
    const NAME: &'static str = "mcp";
    type Args = McpArgs;
    // `String`, not `serde_json::Value`: every existing tool in this workspace (`read_file`,
    // `write_file`, `list_dir`, `find_files`, `http_request`, `skill`) uses `Output = String` —
    // none use `Value`. Matching that proven shape avoids depending on an unverified assumption
    // about what `PortableTool::Output` requires (`IntoToolOutput`) that this repo's hard rule
    // against running `cargo` means nothing here can actually check.
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        format!(
            "Call a tool exposed by a configured MCP (Model Context Protocol) server. Pass \
             the server name, the tool name, and arguments matching that tool's schema. Only \
             tools explicitly allow-listed for their server can be called.\n\nConfigured \
             servers and their allowed tools:\n{}",
            self.servers.describe()
        )
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(McpArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: McpArgs) -> Result<String, ToolFailure> {
        // Stage (a): resolve the server. No activity emitted for this stage — an unresolvable
        // server name never reached any real server, so there's nothing worth logging as a
        // network-shaped event (`design.md`'s "three-stage refusal order").
        let handle = self.servers.resolve(&args.server)?;

        let started = Instant::now();
        let outcome = self.route(&handle, &args).await;

        let _ = self.ctx.activity.try_send((
            self.ctx.agent,
            ToolActivity::McpCall {
                server: args.server.clone(),
                tool: args.tool.clone(),
                ok: outcome.is_ok(),
                ms: started.elapsed().as_millis() as u64,
            },
        ));

        // Pretty-printed JSON, matching `http_request`'s own "pretty-print structured output
        // for the model" default — the raw `Value` a server returns is rarely meant to be read
        // as a single unbroken line.
        outcome.map(|value| {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        })
    }
}

impl McpProxy {
    /// Stages (b)/(c)/the forward itself, split out from [`PortableTool::call`] so every path
    /// through it — success or refusal — flows through the one activity-emitting call site
    /// above exactly once.
    async fn route(
        &self,
        handle: &crate::servers::ServerHandle,
        args: &McpArgs,
    ) -> Result<Value, ToolFailure> {
        // Stage (b): the server must have actually advertised this tool.
        if handle.advertised(&args.tool).is_none() {
            return Err(ToolFailure::NotFound(format!(
                "server '{}' does not advertise a tool named '{}'",
                args.server, args.tool
            )));
        }

        // Stage (c): the tool must be on that server's configured allow-list.
        if !handle.is_allowed(&args.tool) {
            return Err(ToolFailure::Denied(format!(
                "tool '{}' on server '{}' is not on its configured allow-list",
                args.tool, args.server
            )));
        }

        handle.call_tool(&args.tool, args.arguments.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use tokio_util::sync::CancellationToken;

    fn ctx() -> (ToolCtx, tokio::sync::mpsc::Receiver<(mate_tool_api::AgentId, ToolActivity)>) {
        let (activity, rx) = tokio::sync::mpsc::channel(8);
        (
            ToolCtx {
                agent: mate_tool_api::AgentId::ROOT,
                root: std::env::temp_dir(),
                max_output_bytes: 1_000_000,
                spawner: None,
                activity,
                cancel: CancellationToken::new(),
                approvals: None,
                skills: Arc::from([]),
                agents_md: None,
            },
            rx,
        )
    }

    #[tokio::test]
    async fn schema_carries_field_descriptions() {
        let (ctx, _rx) = ctx();
        let tool = McpProxy::new(ctx, Arc::new(McpServers::empty()));
        let schema = tool.parameters();
        for field in ["server", "tool", "arguments"] {
            let description = schema["properties"][field]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                !description.is_empty(),
                "{field} must carry a non-empty schema description"
            );
        }
    }

    #[tokio::test]
    async fn calling_an_unconfigured_server_is_refused_without_emitting_activity() {
        let (ctx, mut rx) = ctx();
        let tool = McpProxy::new(ctx, Arc::new(McpServers::empty()));

        let err = tool
            .call(McpArgs {
                server: "nope".to_string(),
                tool: "anything".to_string(),
                arguments: Value::Null,
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err, ToolFailure::NotFound(_)),
            "an unconfigured server must refuse with NotFound, not attempt any call"
        );
        assert!(
            rx.try_recv().is_err(),
            "stage (a) refusals must not emit ToolActivity::McpCall (design.md)"
        );
    }

    fn spec_with_allow(allow: Vec<String>) -> crate::servers::ServerSpec {
        crate::servers::ServerSpec {
            name: "demo".to_string(),
            command: "true".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            allow,
        }
    }

    #[tokio::test]
    async fn empty_registry_describe_says_nothing_is_available() {
        let servers = McpServers::empty();
        assert_eq!(servers.describe(), "No MCP servers are currently available.");
    }

    #[test]
    fn server_spec_carries_its_allow_list() {
        // Smoke test that the plain config-shape struct this crate exposes (no mate-core
        // dependency, per §8.1 note 1) round-trips its fields without any transport involved.
        let spec = spec_with_allow(vec!["read".to_string()]);
        assert_eq!(spec.allow, vec!["read".to_string()]);
        assert_eq!(spec.name, "demo");
    }

    #[cfg(unix)]
    fn tool_info(name: &str) -> crate::protocol::McpToolInfo {
        crate::protocol::McpToolInfo {
            name: name.to_string(),
            description: String::new(),
            input_schema: Value::Null,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn calling_a_tool_the_server_never_advertised_is_refused() {
        let (ctx, mut rx) = ctx();
        let handle = crate::servers::ServerHandle::test_ready(
            "demo",
            vec![tool_info("read")],
            vec!["read".to_string()],
        );
        let tool = McpProxy::new(ctx, Arc::new(McpServers::test_with_ready(handle)));

        let err = tool
            .call(McpArgs {
                server: "demo".to_string(),
                tool: "never_advertised".to_string(),
                arguments: Value::Null,
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err, ToolFailure::NotFound(_)),
            "an unadvertised tool must refuse with NotFound"
        );
        let (_, activity) = rx.try_recv().expect("stage (b)+ refusals do emit McpCall");
        assert!(matches!(
            activity,
            ToolActivity::McpCall { ok: false, .. }
        ));
    }

    /// `ServerHandle::test_ready` wraps a real `cat` process (see its own doc comment), which
    /// echoes whatever this crate's `tools/call` request writes straight back with no `result`
    /// field — enough to prove the real success path (write → child echoes → read_loop parses →
    /// `ServerHandle::call_tool` → `McpProxy::route` → JSON-formatted `Output`) round-trips
    /// end to end through a real subprocess, without needing genuine MCP tool semantics.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_allow_listed_call_to_an_advertised_tool_round_trips_and_emits_activity() {
        let (ctx, mut rx) = ctx();
        let handle = crate::servers::ServerHandle::test_ready(
            "demo",
            vec![tool_info("read")],
            vec!["read".to_string()],
        );
        let tool = McpProxy::new(ctx, Arc::new(McpServers::test_with_ready(handle)));

        let output = tool
            .call(McpArgs {
                server: "demo".to_string(),
                tool: "read".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
            })
            .await
            .expect("an allow-listed, advertised tool call must succeed");

        assert_eq!(
            output, "null",
            "cat's echo has no result field, so the resolved Value::Null must round-trip as \
             pretty-printed JSON \"null\" — proving the whole success path actually ran, not \
             just that it didn't error"
        );

        let (_, activity) = rx.try_recv().expect("a successful call must still emit McpCall");
        assert!(
            matches!(
                activity,
                ToolActivity::McpCall {
                    ok: true,
                    ref server,
                    ref tool,
                    ..
                } if server == "demo" && tool == "read"
            ),
            "activity must report ok: true with the actual server/tool names: {activity:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn calling_an_advertised_but_disallowed_tool_is_refused() {
        let (ctx, mut rx) = ctx();
        // Advertised by the server, but the allow-list only permits "read".
        let handle = crate::servers::ServerHandle::test_ready(
            "demo",
            vec![tool_info("read"), tool_info("delete_everything")],
            vec!["read".to_string()],
        );
        let tool = McpProxy::new(ctx, Arc::new(McpServers::test_with_ready(handle)));

        let err = tool
            .call(McpArgs {
                server: "demo".to_string(),
                tool: "delete_everything".to_string(),
                arguments: Value::Null,
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err, ToolFailure::Denied(_)),
            "an advertised-but-not-allow-listed tool must refuse with Denied, not NotFound"
        );
        let (_, activity) = rx.try_recv().expect("stage (b)+ refusals do emit McpCall");
        assert!(matches!(
            activity,
            ToolActivity::McpCall { ok: false, .. }
        ));
    }
}
