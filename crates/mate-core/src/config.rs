//! Config-shaped domain types shared between `mate-cli`'s loader and the agent/session
//! builders that will consume them (§4, §5.1, §10). `DelegationPolicy` and
//! `HttpPolicy` are deserialized straight out of TOML tables; `AgentSpec` and `SessionSpec`
//! are assembled from a loaded `Config` plus per-invocation data (workspace root, title).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Per-agent HTTP tool policy. Same shape backs both the process-wide `[http]` config table
/// and a narrowed copy handed to a subagent (§7.4: narrowing-only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpPolicy {
    pub enabled: bool,
    pub policy: HttpAccessPolicy,
    pub rate_limit_per_host_per_min: u32,
}

impl Default for HttpPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            policy: HttpAccessPolicy::Public,
            rate_limit_per_host_per_min: 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpAccessPolicy {
    /// Public hosts only — private/loopback/link-local ranges rejected (§8.2).
    Public,
    /// `--http-allow-localhost`: also permits loopback. Never the default.
    AllowLocalhost,
}

/// MCP (Model Context Protocol) server configuration (§8.3-shaped, `add-mcp-support`): zero or
/// more named servers, each spawned over stdio and proxied through `mate-tool-mcp`'s single
/// `mcp` tool. Empty by default — no server configured means no `mcp` tool attached at all
/// (spec: "No servers configured").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub servers: Vec<McpServerConfig>,
}

/// One configured MCP server. `transport` only ever deserializes to [`McpTransport::Stdio`]
/// today — any other value fails at config-load time (serde's own unknown-variant error),
/// satisfying "reject any transport other than stdio" without a separate validation pass for
/// that specific check. [`validate_mcp_servers`] covers what serde's enum matching can't:
/// duplicate names across servers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// Tool names this server's calls may run unattended. Empty means every call to this
    /// server is refused, though its proxy entry still attaches (spec: "Empty allow-list").
    pub allow: Vec<String>,
}

/// This change supports stdio only (`proposal.md`'s non-goals: remote transports are a
/// follow-on). A single-variant enum rather than a `bool`/`&str` so a future remote transport is
/// an additive variant, not a breaking field-type change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    #[default]
    Stdio,
}

/// Rejects a configuration with two servers sharing a name, before any server is spawned (spec:
/// "Duplicate server name"). Pure and process-free so it's called from `mate-cli`'s config
/// loader right after deserializing, ahead of ever touching `mate-tool-mcp`.
pub fn validate_mcp_servers(servers: &[McpServerConfig]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for server in servers {
        if !seen.insert(server.name.as_str()) {
            return Err(format!("duplicate mcp server name: '{}'", server.name));
        }
    }
    Ok(())
}

/// The subagent model absent any explicit override — the HuggingFace path's own default, used
/// by [`DelegationPolicy::default`]. Exposed so `mate-cli` (which knows about backend selection,
/// a concept `mate-core` doesn't have) can detect "still at the HuggingFace default" and swap in
/// a backend-appropriate default of its own instead, without duplicating this literal.
pub const DEFAULT_SUBAGENT_MODEL: &str = "Qwen/Qwen3-Coder-30B-A3B-Instruct";

/// Delegation guardrails (§7.4). Depth 1 by default; deeper trees are opt-in via config only,
/// never via a tool argument the model controls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationPolicy {
    pub enabled: bool,
    pub subagent_model: Option<String>,
    pub max_depth: usize,
    pub max_concurrent: usize,
    pub max_total_per_turn: usize,
    pub subagent_max_turns: usize,
    pub wall_clock_timeout_secs: u64,
    pub report_max_bytes: usize,
}

impl Default for DelegationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            subagent_model: Some(DEFAULT_SUBAGENT_MODEL.to_string()),
            max_depth: 1,
            max_concurrent: 4,
            max_total_per_turn: 8,
            subagent_max_turns: 8,
            wall_clock_timeout_secs: 120,
            report_max_bytes: 2048,
        }
    }
}

/// Root or subordinate agent configuration (§4). The same builder produces both — a
/// subagent is an `AgentSpec` with a narrowed toolset and `may_delegate: false`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSpec {
    pub model: String,
    pub sub_provider: Option<String>,
    pub base_url: Option<String>,
    /// System preamble handed to the agent as-is. `M1-4` layers templating
    /// (workspace root, OS, rendered tool list) on top by producing this string —
    /// `build_agent` itself just forwards whatever preamble the spec carries.
    pub preamble: String,
    pub temperature: f64,
    pub max_tokens: u64,
    /// Total model-call budget for a turn from this agent, including the initial call and
    /// every tool-driven continuation (`M4-2`'s `default_max_turns`). A tool call followed by
    /// a model-authored final answer needs at least two.
    pub max_turns: usize,
    pub http: HttpPolicy,
    pub may_delegate: bool,
    pub delegation: DelegationPolicy,
}

/// One session's worth of config: workspace root, root agent, delegation policy, turn cap
/// (§5.1). `title` starts from the root dir name and is renamable in the TUI.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSpec {
    pub title: String,
    pub root: PathBuf,
    pub agent: AgentSpec,
    pub delegation: DelegationPolicy,
    pub max_turns: usize,
}

#[cfg(test)]
mod mcp_config_tests {
    use super::*;

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "true".to_string(),
            ..McpServerConfig::default()
        }
    }

    #[test]
    fn transport_defaults_to_stdio() {
        assert_eq!(McpServerConfig::default().transport, McpTransport::Stdio);
    }

    #[test]
    fn empty_server_list_validates() {
        assert!(validate_mcp_servers(&[]).is_ok());
    }

    #[test]
    fn distinct_names_validate() {
        assert!(validate_mcp_servers(&[server("a"), server("b")]).is_ok());
    }

    #[test]
    fn duplicate_names_are_rejected_before_any_server_would_be_spawned() {
        let err = validate_mcp_servers(&[server("dup"), server("dup")]).unwrap_err();
        assert!(
            err.contains("dup"),
            "the error must name the offending duplicate server: {err}"
        );
    }

    #[test]
    fn an_unsupported_transport_is_rejected_at_deserialize_time() {
        let toml = r#"
            name = "x"
            transport = "http"
            command = "whatever"
        "#;
        let err = toml::from_str::<McpServerConfig>(toml).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("http")
                || err.to_string().to_lowercase().contains("variant"),
            "an unsupported transport must fail to deserialize, not silently fall back to \
             stdio: {err}"
        );
    }
}
