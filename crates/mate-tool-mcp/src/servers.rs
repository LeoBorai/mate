//! [`McpServers`]: the process-wide registry every [`crate::McpProxy`] instance shares (built
//! once, handed down as an `Arc`, the same shape as `mate_tool_http::HttpShared` — see
//! `design.md`'s "Server registry" decision). Owns each configured server's live stdio session
//! or its failure reason, built once at startup by [`McpServers::connect`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mate_tool_api::ToolFailure;
use serde_json::Value;

use crate::protocol::{McpToolInfo, ToolsListResult, call_tool_params, initialize_params};
use crate::transport::StdioTransport;

/// How long [`McpServers::connect`] waits for one server's `initialize` handshake or
/// `tools/list` call before treating it as failed-to-initialize (`design.md`'s mitigation for a
/// hung server delaying agent construction). Not configurable — every other fixed network/process
/// timeout in this workspace (`mate_tool_http::HttpLimits`) is a constant too, for the same
/// per-agent reason.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a routed `tools/call` waits for a response once the server is already initialized.
/// Generous relative to the handshake timeout — a tool call can legitimately do real work
/// (a query, a long-running command), unlike the handshake, which is just protocol chatter.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// One configured MCP server, transport-agnostic: no `mate-core` config type reaches this crate
/// (§8.1 note 1 applies to every `mate-tool-*` crate, not just the ones that predate this
/// change) — whichever caller builds the registry (`mate-cli`) maps its own config type into
/// this plain struct first.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// Tool names this server's proxy calls may reach unattended. Empty means every call to
    /// this server is refused (spec: "Empty allow-list") — the server's tool still attaches, so
    /// its description can say so.
    pub allow: Vec<String>,
}

/// One successfully initialized server: its live transport, what it actually advertised, and
/// what it's configured to allow.
pub struct ServerHandle {
    pub name: String,
    transport: StdioTransport,
    pub tools: Vec<McpToolInfo>,
    pub allow: Vec<String>,
}

impl ServerHandle {
    pub fn advertised(&self, tool: &str) -> Option<&McpToolInfo> {
        self.tools.iter().find(|t| t.name == tool)
    }

    pub fn is_allowed(&self, tool: &str) -> bool {
        self.allow.iter().any(|allowed| allowed == tool)
    }

    /// Whether the underlying process is still alive — a server that initialized successfully
    /// can still die mid-session (spec: "Server process exits mid-session").
    pub fn is_alive(&self) -> bool {
        !self.transport.is_dead()
    }

    /// Forwards an already-validated call (advertised and allow-listed — the caller checks
    /// both before reaching here) to the server's `tools/call`.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, ToolFailure> {
        if !self.is_alive() {
            return Err(ToolFailure::Other(anyhow::anyhow!(
                "server '{}' is no longer running",
                self.name
            )));
        }
        self.transport
            .request(
                "tools/call",
                Some(call_tool_params(tool, arguments)),
                CALL_TIMEOUT,
            )
            .await
    }
}

enum ServerEntry {
    Ready(Arc<ServerHandle>),
    Failed { reason: String },
}

/// The registry every [`crate::McpProxy`] instance shares. Built once by [`Self::connect`];
/// never mutated after that except through each entry's own `ServerHandle` (whose `is_alive`
/// reflects a process dying after a successful start — the entry itself stays `Ready`, since
/// "initialized" and "currently alive" are different questions, per the spec's separate
/// "server fails to spawn or initialize" and "server process exits mid-session" scenarios).
pub struct McpServers {
    entries: HashMap<String, ServerEntry>,
}

impl McpServers {
    /// Spawns and initializes every `spec` in order. A spawn or handshake failure is recorded
    /// as that server's own failure and logged — it never aborts the rest of the list (spec:
    /// "Server fails to spawn or initialize" -> continue initializing every other server).
    /// `specs` is assumed already validated (no duplicate names, `stdio` transport only) by the
    /// caller's config-loading step — this constructor doesn't re-check either.
    pub async fn connect(specs: Vec<ServerSpec>) -> Self {
        let mut entries = HashMap::with_capacity(specs.len());
        for spec in specs {
            let name = spec.name.clone();
            let entry = match connect_one(&spec).await {
                Ok(handle) => ServerEntry::Ready(Arc::new(handle)),
                Err(reason) => {
                    tracing::warn!(
                        server = %spec.name,
                        reason = %reason,
                        "mcp server failed to initialize; excluded from this session's toolset"
                    );
                    ServerEntry::Failed { reason }
                }
            };
            entries.insert(name, entry);
        }
        Self { entries }
    }

    /// An empty registry — every call refused, no process ever spawned. The state a process
    /// with zero configured servers starts from (spec: "No servers configured").
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Whether at least one configured server is ready to route calls to — `build_toolset`
    /// attaches `McpProxy` only when this is true (spec: "No servers configured or
    /// initialized" -> proxy absent).
    pub fn has_active_servers(&self) -> bool {
        self.entries
            .values()
            .any(|e| matches!(e, ServerEntry::Ready(_)))
    }

    /// Resolves `name` to its live handle, or a `NotFound`-backed refusal describing why not:
    /// never configured, or configured but failed to initialize. Both collapse to `NotFound`
    /// (not `Denied`) — an unknown or unavailable server is a naming/config problem for the
    /// model to report back, not a policy refusal.
    pub fn resolve(&self, name: &str) -> Result<Arc<ServerHandle>, ToolFailure> {
        match self.entries.get(name) {
            Some(ServerEntry::Ready(handle)) => Ok(handle.clone()),
            Some(ServerEntry::Failed { reason }) => Err(ToolFailure::NotFound(format!(
                "mcp server '{name}' failed to initialize: {reason}"
            ))),
            None => Err(ToolFailure::NotFound(format!(
                "no mcp server named '{name}' is configured"
            ))),
        }
    }

    /// Enumerates every ready server and its allow-listed tools, for [`crate::McpProxy`]'s
    /// `description()` — the mitigation `design.md` names for a single global tool otherwise
    /// hiding what's actually reachable (mirrors `mate-tool-skills`'s preamble-list precedent,
    /// but folded into this tool's own description since the server/tool list isn't known until
    /// after this registry's own async construction, unlike skills' discovery-at-session-build).
    pub fn describe(&self) -> String {
        let mut names: Vec<&String> = self.entries.keys().collect();
        names.sort();

        let mut sections = Vec::new();
        for name in names {
            let Some(ServerEntry::Ready(handle)) = self.entries.get(name) else {
                continue;
            };
            if handle.allow.is_empty() {
                sections.push(format!("- {name}: no tools currently allowed"));
                continue;
            }
            let tools: Vec<String> = handle
                .allow
                .iter()
                .filter_map(|allowed| handle.advertised(allowed))
                .map(|t| {
                    if t.description.is_empty() {
                        t.name.clone()
                    } else {
                        format!("{} - {}", t.name, t.description)
                    }
                })
                .collect();
            sections.push(format!("- {name}: {}", tools.join("; ")));
        }

        if sections.is_empty() {
            "No MCP servers are currently available.".to_string()
        } else {
            sections.join("\n")
        }
    }
}

impl McpServers {
    /// Test-support constructor: wires one already-"initialized" `ServerHandle` straight into
    /// the registry without going through [`Self::connect`]'s real handshake — lets tests in
    /// this crate and downstream crates (`mate-core::toolset`'s attachment tests,
    /// `crate::proxy`'s refusal-stage tests) exercise `resolve`/`advertised`/`is_allowed`/
    /// `has_active_servers` against a live (but MCP-silent) child process, without needing a
    /// real MCP-speaking fixture server. Not behind `#[cfg(test)]`: it's used from other crates'
    /// test code, which only ever links this crate as an ordinary dependency, the same reason
    /// `mate_tool_http::HttpShared::with_limits` is a plain public constructor "for tests" too.
    pub fn test_with_ready(handle: ServerHandle) -> Self {
        let mut entries = HashMap::new();
        entries.insert(handle.name.clone(), ServerEntry::Ready(Arc::new(handle)));
        Self { entries }
    }
}

impl ServerHandle {
    /// Builds a `ServerHandle` around a real (but MCP-silent) child process — `cat` on unix,
    /// which just echoes stdin to stdout and never sends anything unprompted, so it's alive
    /// (`is_alive() == true`) without ever answering a `tools/list`/`tools/call` this test
    /// helper never issues. Only usable for tests that stop before actually forwarding a call.
    /// See [`McpServers::test_with_ready`]'s doc comment for why this isn't `#[cfg(test)]`.
    #[cfg(unix)]
    pub fn test_ready(name: &str, tools: Vec<McpToolInfo>, allow: Vec<String>) -> Self {
        let transport = StdioTransport::spawn("cat", &[], &HashMap::new())
            .expect("`cat` must be spawnable in the test environment");
        Self {
            name: name.to_string(),
            transport,
            tools,
            allow,
        }
    }
}

async fn connect_one(spec: &ServerSpec) -> Result<ServerHandle, String> {
    let transport = StdioTransport::spawn(&spec.command, &spec.args, &spec.env)
        .map_err(|err| err.to_string())?;

    tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        transport.request("initialize", Some(initialize_params()), HANDSHAKE_TIMEOUT),
    )
    .await
    .map_err(|_| "timed out waiting for initialize response".to_string())?
    .map_err(|err| err.to_string())?;

    transport
        .notify("notifications/initialized", None)
        .await
        .map_err(|err| err.to_string())?;

    let list = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        transport.request("tools/list", None, HANDSHAKE_TIMEOUT),
    )
    .await
    .map_err(|_| "timed out waiting for tools/list response".to_string())?
    .map_err(|err| err.to_string())?;

    let parsed: ToolsListResult =
        serde_json::from_value(list).map_err(|err| format!("invalid tools/list response: {err}"))?;

    Ok(ServerHandle {
        name: spec.name.clone(),
        transport,
        tools: parsed.tools,
        allow: spec.allow.clone(),
    })
}
