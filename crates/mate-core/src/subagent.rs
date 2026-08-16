//! [`SubagentRunner`] (§6.2/§7, `M9-2`): `mate-core`'s implementation of
//! `mate_tool_api::SubagentSpawner`, the seam `mate-tool-agent`'s `spawn_agent` tool calls
//! through `ToolCtx::spawner` without depending on this crate (§8.1 note 1).
//!
//! One `SubagentRunner` is built per session, by [`crate::session::SessionManager::spawn`],
//! only when that session's root agent has `may_delegate: true`. It owns every guardrail
//! `M9-4` asks for — `max_depth`, `max_concurrent`, `max_total_per_turn`, per-subagent
//! `max_turns`, and a wall-clock timeout — and every subagent spawned during the session's
//! lifetime, at any delegation depth, shares its concurrency semaphore, per-turn counter, and
//! `AgentId` allocator: [`SubagentRunner::nested`] clones those `Arc`s rather than starting
//! fresh ones, so the limits stay session-wide regardless of which depth a spawn came from.
//!
//! [`acquire`](SubagentRunner::acquire) is the guardrail gate, split out from
//! [`SubagentRunner::run`] so `M9-4`'s depth/per-turn-cap checks and `M9-5`'s
//! concurrency-queues-rather-than-rejects behavior are unit-testable without ever building a
//! real agent (this module's tests exercise it directly). [`drive_subagent`] is the turn-driving
//! half, split out for the same reason in the other direction: it's generic over the completion
//! model, so it runs against Rig's `MockCompletionModel` in tests exactly like
//! `crate::session`'s own tests do, with no live network and no real `Backend` involved.
//!
//! **Cancellation (`M9-7`).** `SubagentRequest::cancel` — the calling tool's own `ToolCtx::cancel`
//! — is the parent of the subagent's token, which is in turn the parent of any grandchild
//! spawned from it. Today `ToolCtx::cancel` is set to a child of the *session's* cancellation
//! token (`crate::session::SessionManager::spawn`), so `SessionCmd::Shutdown` reaches every
//! subagent at any depth, at its very next await point, exactly like §5.4 describes. A per-turn
//! `SessionCmd::Cancel` does **not** yet reach this tree — only `Shutdown` does — because
//! `ToolCtx` (and the tools built from it) is constructed once per session, not once per turn;
//! narrowing cancellation to a single turn's tool calls needs `ToolCtx` to carry a
//! turn-swappable token, which nothing in `M9` produces. Noted here so it isn't mistaken for an
//! oversight if it's the first thing a future milestone needs to fix.
//!
//! **Activity (`M11-4`).** `self.activity` is a clone of the session's one shared
//! `ActivitySink`, handed down from [`crate::session::SessionManager::spawn`] the same way
//! `events_tx` already is. Every subagent's `ToolCtx` gets a clone of *that* sink — not a fresh
//! per-subagent channel — so a subagent's `ToolActivity` records reach the same forwarding task
//! the root agent's do, tagged with the subagent's own `AgentId` (already part of the sink's
//! `(AgentId, ToolActivity)` item type, so no extra wiring is needed here to keep that
//! attribution straight).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use mate_tool_api::{
    ActivitySink, AgentId, SubagentOutcome, SubagentReport, SubagentRequest, SubagentSpawner,
    ToolCtx, ToolFailure, ToolProfile, truncate_with_notice,
};
use mate_tool_http::HttpShared;
use rig::agent::Agent;
use rig::completion::{CompletionModel, GetTokenUsage};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::agent::{BuiltAgent, build_agent};
use crate::backend::Backend;
use crate::config::{AgentSpec, DelegationPolicy, HttpPolicy};
use crate::preamble::{PreambleRole, render_preamble};
use crate::session::{SessionEvent, SessionId, forward};
use crate::streaming::{self, AgentEvent, AgentEventEnvelope};
use crate::toolset::tool_descriptors;

/// Runs subordinate agents for one session (§6.2). See the module doc for how depth, the
/// concurrency semaphore, and the per-turn counter are shared across [`SubagentRunner::nested`]
/// instances.
pub struct SubagentRunner {
    backend: Arc<Backend>,
    http_shared: Arc<HttpShared>,
    session: SessionId,
    events_tx: mpsc::Sender<SessionEvent>,
    /// The session's one shared activity sink (`M11-4`): every subagent's `ToolCtx` gets a
    /// clone of this, rather than each building its own throwaway channel, so a subagent's
    /// `ToolActivity` records reach the same forwarding task the root agent's do.
    activity: ActivitySink,
    root: PathBuf,
    max_output_bytes: usize,
    model: String,
    sub_provider: Option<String>,
    base_url: Option<String>,
    temperature: f64,
    max_tokens: u64,
    http: HttpPolicy,
    policy: DelegationPolicy,
    /// Depth of the agent that *owns* this runner — `0` for the root's own runner. A subagent
    /// this runner spawns lives at `depth + 1`.
    depth: usize,
    concurrency: Arc<Semaphore>,
    /// Reset once per root prompt by `crate::session::session_task` (`M9-4`'s
    /// `max_total_per_turn`) — shared across every [`SubagentRunner::nested`] instance so the
    /// cap is session-wide, not per delegation level.
    turn_spawns: Arc<AtomicUsize>,
    /// `0` is `AgentId::ROOT`; every subagent in the session gets the next value, regardless of
    /// which depth spawned it.
    next_agent_id: Arc<AtomicU32>,
}

impl SubagentRunner {
    /// Builds the root runner for a session — depth `0`, fresh concurrency semaphore, counter,
    /// and id allocator. `root_agent` is the session's own (already-built) `AgentSpec`: model,
    /// provider routing, temperature, and http policy all carry forward into a subagent's spec
    /// as sensible defaults, narrowed per §7.4 rather than reopened.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: Arc<Backend>,
        http_shared: Arc<HttpShared>,
        session: SessionId,
        events_tx: mpsc::Sender<SessionEvent>,
        activity: ActivitySink,
        root: PathBuf,
        max_output_bytes: usize,
        root_agent: &AgentSpec,
    ) -> Self {
        let max_concurrent = root_agent.delegation.max_concurrent.max(1);
        Self {
            backend,
            http_shared,
            session,
            events_tx,
            activity,
            root,
            max_output_bytes,
            model: root_agent.model.clone(),
            sub_provider: root_agent.sub_provider.clone(),
            base_url: root_agent.base_url.clone(),
            temperature: root_agent.temperature,
            max_tokens: root_agent.max_tokens,
            http: root_agent.http.clone(),
            policy: root_agent.delegation.clone(),
            depth: 0,
            concurrency: Arc::new(Semaphore::new(max_concurrent)),
            turn_spawns: Arc::new(AtomicUsize::new(0)),
            next_agent_id: Arc::new(AtomicU32::new(1)),
        }
    }

    /// Resets the per-turn spawn counter. Called once at the start of each root prompt
    /// (`crate::session::session_task`), so `max_total_per_turn` bounds one turn's sequential
    /// tool rounds rather than the session's whole lifetime.
    pub fn reset_turn(&self) {
        self.turn_spawns.store(0, Ordering::SeqCst);
    }

    /// A spawner for one delegation level deeper (§7.4: depth is opt-in via config, never a
    /// tool argument). Shares this runner's concurrency semaphore, per-turn counter, and id
    /// allocator — only `depth` changes.
    fn nested(&self) -> Arc<SubagentRunner> {
        Arc::new(SubagentRunner {
            backend: self.backend.clone(),
            http_shared: self.http_shared.clone(),
            session: self.session,
            events_tx: self.events_tx.clone(),
            activity: self.activity.clone(),
            root: self.root.clone(),
            max_output_bytes: self.max_output_bytes,
            model: self.model.clone(),
            sub_provider: self.sub_provider.clone(),
            base_url: self.base_url.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            http: self.http.clone(),
            policy: self.policy.clone(),
            depth: self.depth + 1,
            concurrency: self.concurrency.clone(),
            turn_spawns: self.turn_spawns.clone(),
            next_agent_id: self.next_agent_id.clone(),
        })
    }

    /// The guardrail gate (`M9-4`), split out from [`Self::run`] so it's unit-testable without
    /// building a real agent: rejects a spawn that would exceed `max_depth` or
    /// `max_total_per_turn` outright, then *queues* — rather than rejects — once
    /// `max_concurrent` subagents are already running (`M9-5`), by blocking on the semaphore.
    /// Returns the id assigned to the new subagent, the held permit (drop it to free the slot),
    /// and whether that subagent itself is allowed to delegate one level further.
    async fn acquire(
        &self,
    ) -> Result<(AgentId, tokio::sync::OwnedSemaphorePermit, bool), ToolFailure> {
        let child_depth = self.depth + 1;
        if child_depth > self.policy.max_depth {
            return Err(ToolFailure::Denied(format!(
                "delegation depth limit reached (max_depth={})",
                self.policy.max_depth
            )));
        }

        let used = self.turn_spawns.fetch_add(1, Ordering::SeqCst) + 1;
        if used > self.policy.max_total_per_turn {
            return Err(ToolFailure::Denied(format!(
                "max_total_per_turn ({}) reached for this turn",
                self.policy.max_total_per_turn
            )));
        }

        let permit = self
            .concurrency
            .clone()
            .acquire_owned()
            .await
            .expect("concurrency semaphore is never closed");

        let agent_id = AgentId(self.next_agent_id.fetch_add(1, Ordering::SeqCst));
        let may_delegate = child_depth < self.policy.max_depth;
        Ok((agent_id, permit, may_delegate))
    }
}

#[async_trait]
impl SubagentSpawner for SubagentRunner {
    async fn run(&self, request: SubagentRequest) -> Result<SubagentReport, ToolFailure> {
        let (agent_id, _permit, may_delegate) = self.acquire().await?;

        let cancel = request.cancel.child_token();
        let ctx = ToolCtx {
            agent: agent_id,
            root: self.root.clone(),
            max_output_bytes: self.max_output_bytes,
            spawner: may_delegate.then(|| self.nested() as Arc<dyn SubagentSpawner>),
            activity: self.activity.clone(),
            cancel: cancel.clone(),
        };

        let max_turns = request
            .max_turns
            .unwrap_or(self.policy.subagent_max_turns)
            .clamp(1, self.policy.subagent_max_turns.max(1));

        let mut http = self.http.clone();
        http.enabled = self.http.enabled && matches!(request.tools, ToolProfile::ReadOnlyNet);

        let preamble = render_preamble(
            PreambleRole::Subagent,
            &self.root,
            std::env::consts::OS,
            &tool_descriptors(may_delegate, http.enabled),
        );

        let spec = AgentSpec {
            model: self
                .policy
                .subagent_model
                .clone()
                .unwrap_or_else(|| self.model.clone()),
            sub_provider: self.sub_provider.clone(),
            base_url: self.base_url.clone(),
            preamble,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            max_turns,
            http,
            may_delegate,
            delegation: self.policy.clone(),
        };

        let deadline = Duration::from_secs(self.policy.wall_clock_timeout_secs);
        let report_max_bytes = self.policy.report_max_bytes;
        let built = build_agent(&self.backend, &self.http_shared, &spec, ctx);

        let report = match built {
            BuiltAgent::HuggingFace(agent) => {
                drive_subagent(
                    &agent,
                    self.session,
                    agent_id,
                    &request,
                    &cancel,
                    deadline,
                    report_max_bytes,
                    &self.events_tx,
                )
                .await
            }
            BuiltAgent::OpenAiCompatible(agent) => {
                drive_subagent(
                    &agent,
                    self.session,
                    agent_id,
                    &request,
                    &cancel,
                    deadline,
                    report_max_bytes,
                    &self.events_tx,
                )
                .await
            }
        };

        Ok(report)
    }
}

/// Drives one subagent's turn to completion (`M9-2`/`M9-6`/`M9-7`/`M9-8`), generic over the
/// completion model so it's directly testable against `rig::test_utils::MockCompletionModel`
/// with no `Backend` and no network involved.
///
/// Emits `SubagentSpawned` before the turn starts and `SubagentFinished` after it ends, and
/// forwards every event the turn itself produces — tagged with `agent_id`, not
/// `AgentId::ROOT` — onto the session's shared channel, the same [`forward`] helper
/// `crate::session` uses for the root agent's own events.
///
/// `request.task` is the *only* prompt this agent ever receives (`M9-8`: no history is threaded
/// in, matching `streaming::stream_turn`'s own "no prior history" contract) — the mechanism
/// behind §7.6's non-addressability invariant: nothing after construction can add another
/// user-role message to this agent's conversation.
#[allow(clippy::too_many_arguments)]
async fn drive_subagent<M>(
    agent: &Agent<M>,
    session: SessionId,
    agent_id: AgentId,
    request: &SubagentRequest,
    cancel: &CancellationToken,
    deadline: Duration,
    report_max_bytes: usize,
    events_tx: &mpsc::Sender<SessionEvent>,
) -> SubagentReport
where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    forward(
        events_tx,
        session,
        AgentEventEnvelope {
            agent: agent_id,
            event: AgentEvent::SubagentSpawned {
                id: agent_id,
                label: request.label.clone(),
                task: request.task.clone(),
            },
        },
    );

    // An `Arc<AtomicUsize>` rather than a plain `&mut usize` captured by the closure below:
    // `turn_fut` (pinned, held across the `select!`) outlives the closure's last *apparent*
    // use of a captured exclusive reference, since the pinned future's own `Drop` could in
    // principle still touch it — the borrow checker keeps such a capture alive for the pin's
    // whole scope, which would make reading `turns` after the `select!` an error. A shared,
    // interior-mutable counter sidesteps that entirely.
    let turns = Arc::new(AtomicUsize::new(0));
    let turn_fut = {
        let turns = turns.clone();
        streaming::stream_turn(agent, request.task.clone(), cancel, move |envelope| {
            if matches!(envelope.event, AgentEvent::Usage(_)) {
                turns.fetch_add(1, Ordering::SeqCst);
            }
            forward(
                events_tx,
                session,
                AgentEventEnvelope {
                    agent: agent_id,
                    event: envelope.event,
                },
            );
        })
    };
    tokio::pin!(turn_fut);

    let mut timed_out = false;
    let outcome = tokio::select! {
        biased;
        outcome = &mut turn_fut => outcome,
        _ = tokio::time::sleep(deadline) => {
            timed_out = true;
            cancel.cancel();
            turn_fut.await
        }
    };

    let outcome_kind = if timed_out {
        SubagentOutcome::TimedOut
    } else if outcome.cancelled {
        SubagentOutcome::Cancelled
    } else if let Some(err) = outcome.error {
        SubagentOutcome::Failed { reason: err }
    } else {
        SubagentOutcome::Completed {
            summary: truncate_with_notice(&outcome.text, report_max_bytes),
        }
    };

    let report = SubagentReport {
        id: agent_id,
        outcome: outcome_kind,
        usage: outcome.usage,
        turns: turns.load(Ordering::SeqCst),
    };

    forward(
        events_tx,
        session,
        AgentEventEnvelope {
            agent: agent_id,
            event: AgentEvent::SubagentFinished {
                id: agent_id,
                outcome: report.outcome.clone(),
            },
        },
    );

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration as StdDuration;

    use rig::agent::AgentBuilder;
    use rig::test_utils::{MockAddTool, MockCompletionModel, MockControlledTool, MockStreamEvent};
    use slotmap::SlotMap;
    use tokio::sync::Notify;

    fn request(cancel: CancellationToken) -> SubagentRequest {
        SubagentRequest {
            parent: AgentId::ROOT,
            label: "test".to_string(),
            task: "do the thing".to_string(),
            tools: ToolProfile::ReadOnly,
            max_turns: None,
            cancel,
        }
    }

    fn session_id() -> SessionId {
        let mut sessions: SlotMap<SessionId, ()> = SlotMap::with_key();
        sessions.insert(())
    }

    fn runner(policy: DelegationPolicy) -> (SubagentRunner, mpsc::Receiver<SessionEvent>) {
        let backend = Arc::new(
            Backend::huggingface("dummy-key", None, None).expect("offline client construction"),
        );
        let http_shared = Arc::new(HttpShared::new(60).unwrap());
        let (events_tx, events_rx) = mpsc::channel(64);
        let spec = AgentSpec {
            model: "org/root-model".to_string(),
            sub_provider: None,
            base_url: None,
            preamble: String::new(),
            temperature: 0.2,
            max_tokens: 512,
            max_turns: 4,
            http: HttpPolicy::default(),
            may_delegate: true,
            delegation: policy,
        };
        let (activity, _activity_rx) = mpsc::channel(64);
        (
            SubagentRunner::new(
                backend,
                http_shared,
                session_id(),
                events_tx,
                activity,
                PathBuf::from("."),
                1_000_000,
                &spec,
            ),
            events_rx,
        )
    }

    // --- `acquire` (`M9-4`/`M9-5`): guardrails, without ever building an agent -------------

    #[tokio::test]
    async fn acquire_denies_a_spawn_once_max_depth_is_reached() {
        let policy = DelegationPolicy {
            max_depth: 0,
            ..DelegationPolicy::default()
        };
        let (runner, _events) = runner(policy);

        let err = runner.acquire().await.unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[tokio::test]
    async fn acquire_denies_a_spawn_once_max_total_per_turn_is_reached() {
        let policy = DelegationPolicy {
            max_total_per_turn: 2,
            ..DelegationPolicy::default()
        };
        let (runner, _events) = runner(policy);

        assert!(runner.acquire().await.is_ok());
        assert!(runner.acquire().await.is_ok());
        let err = runner.acquire().await.unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[tokio::test]
    async fn reset_turn_clears_the_per_turn_counter_for_the_next_prompt() {
        let policy = DelegationPolicy {
            max_total_per_turn: 1,
            ..DelegationPolicy::default()
        };
        let (runner, _events) = runner(policy);

        assert!(runner.acquire().await.is_ok());
        assert!(runner.acquire().await.is_err());

        runner.reset_turn();
        assert!(runner.acquire().await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn acquire_queues_rather_than_rejects_once_max_concurrent_is_running() {
        let policy = DelegationPolicy {
            max_concurrent: 4,
            max_total_per_turn: 6,
            ..DelegationPolicy::default()
        };
        let (runner, _events) = runner(policy);
        let runner = Arc::new(runner);

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..6 {
            let runner = runner.clone();
            let in_flight = in_flight.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                let (_id, permit, _may_delegate) =
                    runner.acquire().await.expect("should queue, not reject");
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(StdDuration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                drop(permit);
            }));
        }

        for handle in handles {
            handle.await.expect("no spawn should be rejected");
        }

        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "concurrency cap was exceeded"
        );
    }

    // --- `drive_subagent` (`M9-2`/`M9-6`/`M9-7`/`M9-8`): generic, mock-model driven --------

    #[tokio::test]
    async fn a_completed_subagent_reports_its_summary_turns_and_usage() {
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("call_1", "add", serde_json::json!({"x": 1, "y": 2})),
                MockStreamEvent::final_response_with_total_tokens(5),
            ],
            vec![
                MockStreamEvent::text("found it"),
                MockStreamEvent::final_response_with_total_tokens(7),
            ],
        ]);
        let agent = AgentBuilder::new(model)
            .tool(MockAddTool)
            .default_max_turns(2)
            .build();
        let session = session_id();
        let (events_tx, mut events) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let req = request(cancel.clone());

        let report = drive_subagent(
            &agent,
            session,
            AgentId(1),
            &req,
            &cancel,
            StdDuration::from_secs(30),
            2048,
            &events_tx,
        )
        .await;

        assert_eq!(
            report.outcome,
            SubagentOutcome::Completed {
                summary: "found it".to_string()
            },
            "the model's final text becomes the report's summary verbatim, untruncated"
        );
        assert_eq!(
            report.turns, 2,
            "one completion call per mock stream turn — tool_call round, then text round"
        );
        // `FinalResponse`'s usage is the whole multi-turn interaction's total, not just the
        // last round's — 5 (first completion call) + 7 (second) = 12.
        assert_eq!(
            report.usage.total_tokens, 12,
            "reported usage must be the FinalResponse total, summed across both rounds"
        );

        let first = events.try_recv().expect("spawned event");
        assert_eq!(
            first.agent,
            AgentId(1),
            "the spawned event must be tagged with the subagent's own id, not AgentId::ROOT"
        );
        assert!(matches!(first.event, AgentEvent::SubagentSpawned { .. }));
    }

    #[tokio::test]
    async fn a_200kb_summary_is_truncated_with_a_notice_intact() {
        let big = "a".repeat(200 * 1024);
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text(big),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = AgentBuilder::new(model).build();
        let cancel = CancellationToken::new();
        let req = request(cancel.clone());
        let (events_tx, _events) = mpsc::channel(64);

        let report = drive_subagent(
            &agent,
            session_id(),
            AgentId(1),
            &req,
            &cancel,
            StdDuration::from_secs(30),
            2048,
            &events_tx,
        )
        .await;

        match report.outcome {
            SubagentOutcome::Completed { summary } => {
                assert!(
                    summary.len() <= 2048 + 64,
                    "summary should stay near the cap"
                );
                assert!(summary.contains("truncated"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wall_clock_timeout_reports_timed_out_without_hanging() {
        let started = Arc::new(Notify::new());
        let never_finishes = Arc::new(Notify::new());
        let model = MockCompletionModel::from_stream_turns([vec![MockStreamEvent::tool_call(
            "call_1",
            "controlled",
            serde_json::json!({}),
        )]]);
        let agent = AgentBuilder::new(model)
            .tool(MockControlledTool::new(started, never_finishes))
            .build();
        let cancel = CancellationToken::new();
        let req = request(cancel.clone());
        let (events_tx, _events) = mpsc::channel(64);

        // The tool call never returns on its own — only the 20ms deadline inside
        // `drive_subagent` stops this, so a bounded `timeout` around the whole call proves
        // it actually does.
        let report = tokio::time::timeout(
            StdDuration::from_secs(2),
            drive_subagent(
                &agent,
                session_id(),
                AgentId(1),
                &req,
                &cancel,
                StdDuration::from_millis(20),
                2048,
                &events_tx,
            ),
        )
        .await
        .expect("the wall-clock timeout should stop the turn promptly, not hang on it");

        assert_eq!(
            report.outcome,
            SubagentOutcome::TimedOut,
            "a subagent still running past its wall-clock deadline must report TimedOut"
        );
    }

    #[tokio::test]
    async fn a_parent_cancellation_reports_cancelled_within_one_await_point() {
        let started = Arc::new(Notify::new());
        let never_finishes = Arc::new(Notify::new());
        let model = MockCompletionModel::from_stream_turns([vec![MockStreamEvent::tool_call(
            "call_1",
            "controlled",
            serde_json::json!({}),
        )]]);
        let agent = AgentBuilder::new(model)
            .tool(MockControlledTool::new(started.clone(), never_finishes))
            .build();
        let cancel = CancellationToken::new();
        let req = request(cancel.clone());
        let (events_tx, _events) = mpsc::channel(64);

        let drive = drive_subagent(
            &agent,
            session_id(),
            AgentId(1),
            &req,
            &cancel,
            StdDuration::from_secs(30),
            2048,
            &events_tx,
        );
        tokio::pin!(drive);

        let canceller = cancel.clone();
        let report = tokio::select! {
            report = &mut drive => report,
            _ = started.notified() => {
                canceller.cancel();
                drive.await
            }
        };

        assert_eq!(
            report.outcome,
            SubagentOutcome::Cancelled,
            "cancelling the parent's token must propagate to the subagent's child token \
             (§7.3's token tree) and report Cancelled, not hang or time out"
        );
    }

    // --- `M9-8`: non-addressability ---------------------------------------------------------

    #[tokio::test]
    async fn the_subagent_sees_only_the_task_no_other_history() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("done"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let agent = AgentBuilder::new(model.clone()).build();
        let cancel = CancellationToken::new();
        let req = request(cancel.clone());
        let (events_tx, _events) = mpsc::channel(64);

        drive_subagent(
            &agent,
            session_id(),
            AgentId(1),
            &req,
            &cancel,
            StdDuration::from_secs(30),
            2048,
            &events_tx,
        )
        .await;

        let requests = model.requests();
        assert_eq!(
            requests.len(),
            1,
            "a subagent's single turn must be exactly one model request, never a retry loop"
        );
        // Exactly one message went to the model: the task itself, nothing threaded in from
        // any parent conversation.
        assert_eq!(
            requests[0].chat_history.len(),
            1,
            "§7.6: the subagent's whole input is preamble + task — no parent history leaked in"
        );
    }
}
