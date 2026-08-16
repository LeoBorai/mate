//! Config-shaped domain types shared between `mate-cli`'s loader and the agent/session
//! builders that will consume them (§4, §5.1, §10). `DelegationPolicy` and
//! `HttpPolicy` are deserialized straight out of TOML tables; `AgentSpec` and `SessionSpec`
//! are assembled from a loaded `Config` plus per-invocation data (workspace root, title).

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
            subagent_model: Some("Qwen/Qwen3-Coder-30B-A3B-Instruct".to_string()),
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
