//! The seam `mate-tool-agent` (`M9`) uses to run subordinate agents without depending on
//! `mate-core` (§6.2): the capability is declared here as a trait, implemented in
//! `mate-core`, and injected into [`crate::ToolCtx`] at construction. Nothing in `M3`
//! produces a [`SubagentSpawner`] yet — it's defined now because it's part of `ToolCtx`'s
//! own field list, the same reasoning already applied to `FileOp::Write` (§9.3).

use async_trait::async_trait;
use rig::completion::Usage;

use crate::{AgentId, ToolFailure};

/// How a subagent's run ended. Defined alongside [`SubagentReport`] even though nothing
/// produces one until `M9` — an unused variant costs nothing, a rewritten public enum
/// costs every downstream match arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentOutcome {
    Completed { summary: String },
    Failed { reason: String },
    Cancelled,
    TimedOut,
}

/// Which tools a subagent is allowed to call, narrowed relative to its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolProfile {
    ReadOnly,
    ReadOnlyNet,
    Custom(Vec<String>),
}

/// One request to run a subordinate agent to completion.
#[derive(Debug, Clone)]
pub struct SubagentRequest {
    pub parent: AgentId,
    /// Short label for this subagent, e.g. "deps" or "test-scan" — shown in the roster.
    pub label: String,
    /// The subagent's whole instruction. It does not inherit the parent's conversation.
    pub task: String,
    pub tools: ToolProfile,
    pub max_turns: Option<usize>,
}

/// What a finished (or aborted) subagent run leaves behind.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentReport {
    pub id: AgentId,
    pub outcome: SubagentOutcome,
    pub usage: Usage,
    pub turns: usize,
}

/// Runs a subordinate agent to completion and returns its report. Implementations
/// enforce depth, fan-out, and budget limits — `mate-tool-agent`'s `spawn_agent` tool
/// just calls `run` and doesn't police any of that itself.
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    async fn run(&self, request: SubagentRequest) -> Result<SubagentReport, ToolFailure>;
}
