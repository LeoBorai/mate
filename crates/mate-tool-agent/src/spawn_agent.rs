//! `spawn_agent` (§7.2, `M9-3`): delegates a narrow task to a subordinate agent inside the
//! same session. Every guardrail — depth, concurrency, per-turn fan-out cap, wall clock, turn
//! budget — is `mate-core`'s `SubagentSpawner` implementation's job (§7.4: "enforced in the
//! spawner, never the tool"); this tool only builds the request, awaits the report, and renders
//! it. It only ever appears on an agent whose `ToolCtx` carries a spawner —
//! `mate_core::toolset::build_toolset` gates attaching it on `ctx.spawner.is_some()` — so a
//! subagent (whose own `ToolCtx` has `spawner: None` past the configured delegation depth,
//! §7.4) never has this tool to call in the first place.

use mate_tool_api::{SubagentOutcome, SubagentReport, SubagentRequest, ToolCtx, ToolFailure};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnAgentArgs {
    /// Short label for this subagent, e.g. "deps" or "test-scan". Shown to the user.
    pub label: String,
    /// The complete task. The subagent does NOT see this conversation and starts with an
    /// empty context — restate every piece of information it needs. It cannot ask you a
    /// follow-up question.
    pub task: String,
    /// Tool profile: "read_only" (files) or "read_only_net" (files + http). Defaults to
    /// "read_only".
    #[serde(default)]
    pub tools: Option<String>,
    /// Maximum tool-use turns before the subagent is stopped and asked to report whatever it
    /// has. Defaults to 8, and is capped by the session's own configured limit regardless of
    /// what's asked for here.
    #[serde(default)]
    pub max_turns: Option<usize>,
}

/// Delegates one task to a subordinate agent with its own, empty context window (§7.1: context
/// firewalling — the subagent burns its own budget on the narrow task, the parent's context
/// grows by a short report instead of by every file it read).
pub struct SpawnAgent {
    ctx: ToolCtx,
}

impl SpawnAgent {
    pub fn new(ctx: ToolCtx) -> Self {
        Self { ctx }
    }
}

impl PortableTool for SpawnAgent {
    const NAME: &'static str = "spawn_agent";
    type Args = SpawnAgentArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "Delegate a narrow, self-contained task to a subordinate agent with its own, EMPTY \
         context window. It does not see this conversation and cannot ask follow-up \
         questions — restate every piece of context the task needs. It returns a short \
         report, not raw output; use it to investigate something without growing your own \
         context by the full detail of what you found."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(SpawnAgentArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: SpawnAgentArgs) -> Result<String, ToolFailure> {
        let spawner = self
            .ctx
            .spawner
            .clone()
            .ok_or_else(|| ToolFailure::Denied("delegation not enabled".to_string()))?;

        let tools = args.tools.as_deref().try_into()?;
        let report = spawner
            .run(SubagentRequest {
                parent: self.ctx.agent,
                label: args.label.clone(),
                task: args.task,
                tools,
                max_turns: args.max_turns,
                cancel: self.ctx.cancel.clone(),
            })
            .await?;

        // Cancellation is the one path that isn't feedback for the model to act on — the
        // whole parent turn is already unwinding (§7.3), so this becomes a hard tool error
        // like any other cancelled tool call rather than a report to read.
        if matches!(report.outcome, SubagentOutcome::Cancelled) {
            return Err(ToolFailure::Cancelled);
        }

        Ok(render_report(&args.label, &report))
    }
}

/// Renders a finished subagent's report (§7.5) for the parent model to read.
fn render_report(label: &str, report: &SubagentReport) -> String {
    let tokens = format_tokens(report.usage.total_tokens);
    let turn_word = if report.turns == 1 { "turn" } else { "turns" };
    match &report.outcome {
        SubagentOutcome::Completed { summary } => format!(
            "subagent `{label}` — completed in {} {turn_word}, {tokens} tokens\n\n{summary}",
            report.turns
        ),
        SubagentOutcome::Failed { reason } => format!(
            "subagent `{label}` — failed after {} {turn_word}, {tokens} tokens: {reason}",
            report.turns
        ),
        SubagentOutcome::TimedOut => format!(
            "subagent `{label}` — timed out after {} {turn_word}, {tokens} tokens",
            report.turns
        ),
        SubagentOutcome::Cancelled => format!("subagent `{label}` — cancelled"),
    }
}

/// Renders a token count the way §7.5's report format wants it: `"14.2k"` past 1000, the bare
/// number below.
fn format_tokens(total: u64) -> String {
    if total >= 1000 {
        format!("{:.1}k", total as f64 / 1000.0)
    } else {
        total.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use mate_tool_api::{AgentId, SubagentSpawner, ToolProfile};
    use rig::completion::Usage;
    use tokio_util::sync::CancellationToken;

    fn usage(total_tokens: u64) -> Usage {
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    struct FakeSpawner {
        result: Mutex<Option<Result<SubagentReport, ToolFailure>>>,
        seen: Mutex<Option<SubagentRequest>>,
    }

    impl FakeSpawner {
        fn returning(result: Result<SubagentReport, ToolFailure>) -> Self {
            Self {
                result: Mutex::new(Some(result)),
                seen: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl SubagentSpawner for FakeSpawner {
        async fn run(&self, request: SubagentRequest) -> Result<SubagentReport, ToolFailure> {
            *self.seen.lock().unwrap() = Some(request);
            self.result.lock().unwrap().take().expect("run called once")
        }
    }

    fn ctx(spawner: Option<Arc<dyn SubagentSpawner>>) -> ToolCtx {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        ToolCtx {
            agent: AgentId::ROOT,
            root: std::path::PathBuf::from("."),
            max_output_bytes: 1_000_000,
            spawner,
            activity,
            cancel: CancellationToken::new(),
            approvals: None,
            skills: Arc::from([]),
            agents_md: None,
        }
    }

    fn args(label: &str) -> SpawnAgentArgs {
        SpawnAgentArgs {
            label: label.to_string(),
            task: "investigate the thing".to_string(),
            tools: None,
            max_turns: None,
        }
    }

    #[tokio::test]
    async fn without_a_spawner_the_tool_is_denied() {
        let tool = SpawnAgent::new(ctx(None));
        let err = tool.call(args("deps")).await.unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[tokio::test]
    async fn a_completed_report_renders_the_label_turns_tokens_and_summary() {
        let spawner = Arc::new(FakeSpawner::returning(Ok(SubagentReport {
            id: AgentId(1),
            outcome: SubagentOutcome::Completed {
                summary: "found three call sites".to_string(),
            },
            usage: usage(14_200),
            turns: 6,
        }))) as Arc<dyn SubagentSpawner>;
        let tool = SpawnAgent::new(ctx(Some(spawner)));

        let out = tool.call(args("deps")).await.unwrap();
        assert_eq!(
            out, "subagent `deps` — completed in 6 turns, 14.2k tokens\n\nfound three call sites",
            "rendered report must follow §7.5's format exactly: label, turns, tokens, summary"
        );
    }

    #[tokio::test]
    async fn a_cancelled_outcome_is_a_hard_tool_error_not_a_report() {
        let spawner = Arc::new(FakeSpawner::returning(Ok(SubagentReport {
            id: AgentId(1),
            outcome: SubagentOutcome::Cancelled,
            usage: usage(0),
            turns: 0,
        }))) as Arc<dyn SubagentSpawner>;
        let tool = SpawnAgent::new(ctx(Some(spawner)));

        let err = tool.call(args("deps")).await.unwrap_err();
        assert!(matches!(err, ToolFailure::Cancelled));
    }

    #[tokio::test]
    async fn a_denied_spawn_propagates_the_spawner_s_error_verbatim() {
        let spawner = Arc::new(FakeSpawner::returning(Err(ToolFailure::Denied(
            "delegation depth limit reached (max_depth=1)".to_string(),
        )))) as Arc<dyn SubagentSpawner>;
        let tool = SpawnAgent::new(ctx(Some(spawner)));

        let err = tool.call(args("deps")).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "denied: delegation depth limit reached (max_depth=1)",
            "the tool must forward the spawner's ToolFailure text unmodified for the model to read"
        );
    }

    #[tokio::test]
    async fn the_task_and_label_reach_the_spawner_but_never_any_other_history() {
        let spawner = Arc::new(FakeSpawner::returning(Ok(SubagentReport {
            id: AgentId(1),
            outcome: SubagentOutcome::Completed {
                summary: "ok".to_string(),
            },
            usage: usage(1),
            turns: 1,
        })));
        let tool = SpawnAgent::new(ctx(Some(spawner.clone() as Arc<dyn SubagentSpawner>)));

        tool.call(args("deps")).await.unwrap();

        let seen = spawner.seen.lock().unwrap().take().unwrap();
        assert_eq!(seen.label, "deps", "label must reach the spawner unchanged");
        assert_eq!(
            seen.task, "investigate the thing",
            "task must reach the spawner unchanged — it's the subagent's entire input"
        );
        assert_eq!(
            seen.tools,
            ToolProfile::ReadOnly,
            "omitting `tools` in the args must default to ReadOnly"
        );
    }

    #[test]
    fn format_tokens_switches_to_k_notation_past_one_thousand() {
        assert_eq!(
            format_tokens(999),
            "999",
            "below 1000 renders as a bare integer"
        );
        assert_eq!(
            format_tokens(1000),
            "1.0k",
            "1000 is the first value to switch to k-notation"
        );
        assert_eq!(
            format_tokens(14_200),
            "14.2k",
            "one decimal place, matching §7.5's report example"
        );
    }

    /// `M9-3`'s acceptance criterion: both the tool description and the `task` argument's own
    /// schema description must be blunt that the subagent sees only `task`, nothing else.
    #[test]
    fn description_and_task_schema_are_blunt_about_the_context_boundary() {
        let tool = SpawnAgent::new(ctx(None));
        assert!(
            tool.description()
                .to_lowercase()
                .contains("does not see this conversation")
        );

        let schema = tool.parameters();
        let task_description = schema["properties"]["task"]["description"]
            .as_str()
            .unwrap();
        assert!(
            task_description
                .to_lowercase()
                .contains("does not see this conversation")
        );
    }
}
