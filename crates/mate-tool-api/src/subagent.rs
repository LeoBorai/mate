//! The seam `mate-tool-agent` (`M9`) uses to run subordinate agents without depending on
//! `mate-core` (§6.2): the capability is declared here as a trait, implemented in
//! `mate-core`, and injected into [`crate::ToolCtx`] at construction. Nothing in `M3`
//! produces a [`SubagentSpawner`] yet — it's defined now because it's part of `ToolCtx`'s
//! own field list, the same reasoning already applied to `FileOp::Write` (§9.3).

use async_trait::async_trait;
use rig::completion::Usage;
use tokio_util::sync::CancellationToken;

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

/// Parses the `spawn_agent` tool's `tools` argument (`M9-3`): `None`/`"read_only"` is the
/// default, `"read_only_net"` additionally allows the http tool once `M10` ships one. Any
/// other string is a model mistake, not a crash — reported back as feedback the model can
/// read and correct, the same way every other `ToolFailure` is (§6.1).
impl TryFrom<Option<&str>> for ToolProfile {
    type Error = ToolFailure;

    fn try_from(value: Option<&str>) -> Result<Self, Self::Error> {
        match value {
            None | Some("read_only") => Ok(ToolProfile::ReadOnly),
            Some("read_only_net") => Ok(ToolProfile::ReadOnlyNet),
            Some(other) => Err(ToolFailure::InvalidArgs(format!(
                "unknown tool profile: {other:?} (expected \"read_only\" or \"read_only_net\")"
            ))),
        }
    }
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
    /// The calling agent's own cancellation token (§7.3/§7.6): the spawner derives the
    /// subagent's token as a child of this one, so a parent cancel (or session shutdown)
    /// propagates down without the subagent ever becoming addressable — cancellation is the
    /// only user-initiated signal that reaches it (§7.6's `SubagentControl`).
    pub cancel: CancellationToken,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_value_and_read_only_both_parse_to_the_default_profile() {
        assert_eq!(
            ToolProfile::try_from(None).unwrap(),
            ToolProfile::ReadOnly,
            "omitting `tools` must default to the same profile as an explicit \"read_only\""
        );
        assert_eq!(
            ToolProfile::try_from(Some("read_only")).unwrap(),
            ToolProfile::ReadOnly,
            "\"read_only\" is the documented default profile string"
        );
    }

    #[test]
    fn read_only_net_parses() {
        assert_eq!(
            ToolProfile::try_from(Some("read_only_net")).unwrap(),
            ToolProfile::ReadOnlyNet,
            "\"read_only_net\" must parse to the net-enabled profile"
        );
    }

    #[test]
    fn an_unknown_profile_is_reported_for_the_model_to_read() {
        let err = ToolProfile::try_from(Some("sudo")).unwrap_err();
        assert!(matches!(err, ToolFailure::InvalidArgs(_)));
        assert!(err.to_string().contains("sudo"));
    }
}
