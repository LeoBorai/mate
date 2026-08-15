//! The session manager (§5, `M6`): one Tokio task per session, holding its root agent,
//! conversation history, and cancellation token; a `SessionCmd` queue for control; and a
//! single shared, bounded event channel every session forwards into.
//!
//! `M6-1` lands [`SessionId`] and [`SessionEvent`] — the `session: SessionId` field
//! `mate_core::streaming`'s module doc promised on top of the `{ agent, event }` envelope
//! `M2` already emits. `M6-2` is [`session_task`]: generic over the completion model `M` the
//! same way [`crate::streaming::stream_turn`] is, so [`SessionManager::spawn`] can match on
//! [`BuiltAgent`]'s two provider variants once and instantiate one task per variant, and so
//! this file's own tests can drive it against Rig's mock model without a real backend or
//! network access.
//!
//! `M6-3`'s shared channel backpressure lives in [`forward`]: `try_send` first, and only on a
//! full channel falls back to a blocking send via `tokio::task::block_in_place` — this is
//! safe because every session task runs under the process's multi-thread Tokio runtime, and
//! it's what makes a full channel throttle the producing agent instead of dropping events or
//! panicking (§5.2).
//!
//! `M6-4`'s cancellation tree is two tokens per session: `session_cancel`, cancelled once by
//! `SessionCmd::Shutdown`, and a fresh child `turn_cancel` per prompt, cancelled by
//! `SessionCmd::Cancel` without touching the parent — cancelling a turn must not prevent the
//! next prompt from running. Cancelling the parent cancels whichever child is live, so
//! `Shutdown` mid-turn aborts that turn for free.
//!
//! `M6-5`'s crash isolation is [`spawn_supervised`]: it wraps the session task in its own
//! `tokio::spawn`, so a panic inside becomes an `Err` on the outer `JoinHandle` the supervisor
//! awaits, rather than unwinding into whatever called `spawn`. The supervisor turns that into
//! `SessionStatus::Errored` on the same watch channel the task itself uses for every other
//! transition — the rest of the process never sees a panic, only a status change.

use std::sync::Arc;

use mate_tool_api::{AgentId, ToolCtx};
use rig::agent::Agent;
use rig::completion::{CompletionModel, GetTokenUsage, Message};
use slotmap::SlotMap;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use crate::agent::{BuiltAgent, build_agent};
use crate::backend::Backend;
use crate::config::SessionSpec;
use crate::streaming::{self, AgentEvent, AgentEventEnvelope};

slotmap::new_key_type! {
    /// One tab's worth of state (§5.1): a root agent, its history, and (from `M9` on) every
    /// subagent it spawns during a turn. Stable across other sessions closing and reopening —
    /// a `slotmap` key, not an index into a `Vec`.
    pub struct SessionId;
}

/// One event produced by a session's root agent or a subagent, tagged by both keys so the
/// single shared channel (§5.2) can route to the right tab and, from `M9` on, the right
/// roster row within it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub session: SessionId,
    pub agent: AgentId,
    pub event: AgentEvent,
}

/// Coarse state of a session (§5.4), watched by whoever holds a [`SessionHandle`] — the
/// crash-isolation test in this file today, the TUI's tab bar from `M7` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Running,
    Errored,
    Closed,
}

/// One command a session's task consumes (§5.1). `Approve` has no producer until `M13`'s
/// approval flow exists — the session task accepts and no-ops it now for the same reason
/// `AgentEvent::ApprovalRequired` already exists ahead of its producer.
#[derive(Debug, Clone)]
pub enum SessionCmd {
    Prompt(String),
    Approve { id: Ulid, granted: bool },
    Cancel,
    Shutdown,
}

/// A live session's remote control (§5.1): send it commands, watch its status. Cloning is
/// cheap — a channel sender and a watch receiver are the only state.
#[derive(Clone)]
pub struct SessionHandle {
    pub id: SessionId,
    cmds: mpsc::Sender<SessionCmd>,
    pub status: watch::Receiver<SessionStatus>,
}

impl SessionHandle {
    /// Enqueues one command. Errs only once the session's task has exited and dropped its
    /// receiver — sending into a live-but-busy session still succeeds; the command just waits
    /// its turn in the (small, user-paced) command queue.
    pub async fn send(&self, cmd: SessionCmd) -> Result<(), SessionError> {
        self.cmds.send(cmd).await.map_err(|_| SessionError::Closed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session limit reached ({0} sessions already running)")]
    TooManySessions(usize),
    #[error("session is closed")]
    Closed,
}

/// Capacity of the one shared [`SessionEvent`] channel every session task forwards into
/// (§5.2): sized for several tabs' worth of subagent fan-out. Bounded on purpose — see
/// [`forward`].
const EVENT_CHANNEL_CAPACITY: usize = 512;

/// Command channel capacity per session: commands are user-paced (one prompt, one cancel),
/// never a hot path, so a small bound is plenty and keeps a stuck session's queue from
/// growing unbounded.
const CMD_CHANNEL_CAPACITY: usize = 8;

/// Owns every live session in the process (§5.4): builds each session's root agent, spawns
/// its task under crash isolation, and is the one place `max_sessions` is enforced.
pub struct SessionManager {
    backend: Arc<Backend>,
    events_tx: mpsc::Sender<SessionEvent>,
    sessions: SlotMap<SessionId, SessionHandle>,
    max_sessions: usize,
}

impl SessionManager {
    /// Builds a manager sharing `backend` across every session it spawns, and the single
    /// bounded event channel (§5.2) every session forwards into. Returns the receiving end
    /// for the caller (the TUI, from `M7` on) to poll.
    pub fn new(backend: Arc<Backend>, max_sessions: usize) -> (Self, mpsc::Receiver<SessionEvent>) {
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        (
            Self {
                backend,
                events_tx,
                sessions: SlotMap::with_key(),
                max_sessions,
            },
            events_rx,
        )
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Builds `spec`'s root agent against the shared backend, spawns its task under crash
    /// isolation, and returns a handle to it. Errs without spawning anything once
    /// `max_sessions` live sessions already exist (`M6-4`'s cap).
    pub fn spawn(
        &mut self,
        spec: &SessionSpec,
        ctx: ToolCtx,
    ) -> Result<SessionHandle, SessionError> {
        if self.sessions.len() >= self.max_sessions {
            return Err(SessionError::TooManySessions(self.max_sessions));
        }

        let built = build_agent(&self.backend, &spec.agent, ctx);
        let (cmds_tx, cmds_rx) = mpsc::channel(CMD_CHANNEL_CAPACITY);
        let (status_tx, status_rx) = watch::channel(SessionStatus::Idle);
        let events_tx = self.events_tx.clone();

        let id = self.sessions.insert_with_key(|id| SessionHandle {
            id,
            cmds: cmds_tx,
            status: status_rx,
        });

        match built {
            BuiltAgent::HuggingFace(agent) => {
                spawn_supervised(id, agent, cmds_rx, events_tx, status_tx)
            }
            BuiltAgent::OpenAiCompatible(agent) => {
                spawn_supervised(id, agent, cmds_rx, events_tx, status_tx)
            }
        }

        Ok(self.sessions[id].clone())
    }
}

/// Spawns `session_task` under a supervising task (`M6-5`): the supervisor's own `JoinHandle`
/// can never itself carry a panic (it only awaits and matches), so a panic inside the session
/// task surfaces as `Err` here rather than propagating anywhere else, and becomes
/// `SessionStatus::Errored` on the same channel the task uses for every other transition.
/// Every other live session's task is entirely unaffected — it isn't even aware this one
/// exists.
fn spawn_supervised<M>(
    id: SessionId,
    agent: Agent<M>,
    cmds_rx: mpsc::Receiver<SessionCmd>,
    events_tx: mpsc::Sender<SessionEvent>,
    status_tx: watch::Sender<SessionStatus>,
) where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    let supervisor_status = status_tx.clone();
    tokio::spawn(async move {
        let result = tokio::spawn(session_task(id, agent, cmds_rx, events_tx, status_tx)).await;
        if result.is_err() {
            let _ = supervisor_status.send(SessionStatus::Errored);
        }
    });
}

/// The session task itself (`M6-2`): one per session, owning `agent`'s conversation history
/// across turns and consuming `cmds` until `Shutdown` (or every [`SessionHandle`] is dropped).
async fn session_task<M>(
    id: SessionId,
    agent: Agent<M>,
    mut cmds: mpsc::Receiver<SessionCmd>,
    events_tx: mpsc::Sender<SessionEvent>,
    status_tx: watch::Sender<SessionStatus>,
) where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    let session_cancel = CancellationToken::new();
    let mut history: Vec<Message> = Vec::new();

    while let Some(cmd) = cmds.recv().await {
        match cmd {
            SessionCmd::Prompt(prompt) => {
                let shutdown = run_prompt(
                    id,
                    &agent,
                    prompt,
                    &mut history,
                    &session_cancel,
                    &mut cmds,
                    &events_tx,
                    &status_tx,
                )
                .await;
                if shutdown {
                    break;
                }
            }
            // No turn in flight outside `run_prompt`, so a `Cancel` here has nothing to do.
            SessionCmd::Cancel => {}
            // No approval flow exists until `M13`.
            SessionCmd::Approve { .. } => {}
            SessionCmd::Shutdown => {
                session_cancel.cancel();
                break;
            }
        }
    }

    let _ = status_tx.send(SessionStatus::Closed);
}

/// Runs one prompt to completion (`M6-2`/`M6-4`), racing the turn's future against `cmds` so a
/// `Cancel` or `Shutdown` sent mid-turn takes effect at the turn's very next await point
/// instead of waiting for it to finish on its own. Returns `true` if the caller should stop
/// the session entirely (a `Shutdown` was observed, or every command sender was dropped).
#[allow(clippy::too_many_arguments)]
async fn run_prompt<M>(
    id: SessionId,
    agent: &Agent<M>,
    prompt: String,
    history: &mut Vec<Message>,
    session_cancel: &CancellationToken,
    cmds: &mut mpsc::Receiver<SessionCmd>,
    events_tx: &mpsc::Sender<SessionEvent>,
    status_tx: &watch::Sender<SessionStatus>,
) -> bool
where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
{
    let _ = status_tx.send(SessionStatus::Running);

    let turn_cancel = session_cancel.child_token();
    let snapshot = history.clone();
    let turn = streaming::stream_turn_with_history(
        agent,
        prompt.clone(),
        &snapshot,
        &turn_cancel,
        |envelope| forward(events_tx, id, envelope),
    );
    tokio::pin!(turn);

    let mut shutdown = false;
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = &mut turn => break outcome,
            cmd = cmds.recv() => match cmd {
                Some(SessionCmd::Cancel) => turn_cancel.cancel(),
                Some(SessionCmd::Shutdown) => {
                    shutdown = true;
                    session_cancel.cancel();
                }
                Some(SessionCmd::Approve { .. }) => {}
                // One turn at a time (§5.2's model): a prompt sent while another is still
                // running is dropped rather than queued or interleaved.
                Some(SessionCmd::Prompt(_)) => {}
                None => {
                    shutdown = true;
                    session_cancel.cancel();
                }
            },
        }
    };

    if !outcome.cancelled && outcome.error.is_none() {
        history.push(Message::user(prompt));
        history.push(Message::assistant(outcome.text));
    }
    let _ = status_tx.send(SessionStatus::Idle);
    shutdown
}

/// Forwards one turn's event onto the shared channel (`M6-3`), tagging it with `session`.
/// `try_send` covers the common case for free; only once the channel is genuinely full does
/// this fall back to a blocking send, via `tokio::task::block_in_place` (sound only because
/// every session task runs under the process's multi-thread runtime) — the mechanism that
/// makes a full channel throttle the producing agent's turn instead of dropping the event or
/// panicking (§5.2).
fn forward(
    events_tx: &mpsc::Sender<SessionEvent>,
    session: SessionId,
    envelope: AgentEventEnvelope,
) {
    let event = SessionEvent {
        session,
        agent: envelope.agent,
        event: envelope.event,
    };
    if let Err(TrySendError::Full(event)) = events_tx.try_send(event) {
        tokio::task::block_in_place(|| {
            let _ = events_tx.blocking_send(event);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use rig::agent::AgentBuilder;
    use rig::test_utils::{MockCompletionModel, MockControlledTool, MockStreamEvent};
    use rig::tool::{Tool, ToolContext};
    use serde::{Deserialize, Serialize};
    use tokio::sync::Notify;

    /// A tool that panics on every call — the only reliable way to prove crash isolation
    /// (`M6-5`) without depending on some other crate's internals happening to panic.
    #[derive(Deserialize, Serialize)]
    struct PanicTool;

    #[derive(Deserialize)]
    struct NoArgs {}

    impl Tool for PanicTool {
        const NAME: &'static str = "panic_tool";
        type Error = rig::test_utils::MockToolError;
        type Args = NoArgs;
        type Output = String;

        fn description(&self) -> String {
            "deliberately panics".to_string()
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn call(
            &self,
            _context: &mut ToolContext,
            _args: Self::Args,
        ) -> Result<Self::Output, Self::Error> {
            panic!("deliberate test panic — crash isolation must survive this")
        }
    }

    fn spawn_session<M>(agent: Agent<M>) -> (SessionId, SessionHandle, mpsc::Receiver<SessionEvent>)
    where
        M: CompletionModel + 'static,
        M::StreamingResponse: GetTokenUsage,
    {
        let (cmds_tx, cmds_rx) = mpsc::channel(CMD_CHANNEL_CAPACITY);
        let (status_tx, status_rx) = watch::channel(SessionStatus::Idle);
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let mut sessions: SlotMap<SessionId, ()> = SlotMap::with_key();
        let id = sessions.insert(());
        tokio::spawn(session_task(id, agent, cmds_rx, events_tx, status_tx));
        (
            id,
            SessionHandle {
                id,
                cmds: cmds_tx,
                status: status_rx,
            },
            events_rx,
        )
    }

    #[tokio::test]
    async fn a_prompt_round_trips_through_the_command_channel() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("hi there"),
            MockStreamEvent::final_response_with_total_tokens(3),
        ]]);
        let agent = AgentBuilder::new(model).build();
        let (id, handle, mut events) = spawn_session(agent);

        handle
            .send(SessionCmd::Prompt("hello".into()))
            .await
            .unwrap();

        let mut text = String::new();
        loop {
            let event = events.recv().await.expect("session should emit events");
            assert_eq!(event.session, id);
            assert_eq!(event.agent, AgentId::ROOT);
            match event.event {
                AgentEvent::Token(chunk) => text.push_str(&chunk),
                AgentEvent::TurnComplete => break,
                _ => {}
            }
        }
        assert_eq!(text, "hi there");

        handle.send(SessionCmd::Shutdown).await.unwrap();
        let mut status = handle.status.clone();
        status
            .wait_for(|s| *s == SessionStatus::Closed)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_event_channel_throttles_the_producer_instead_of_dropping_events() {
        let events: Vec<MockStreamEvent> = (0..20)
            .map(|i| MockStreamEvent::text(format!("{i} ")))
            .chain(std::iter::once(
                MockStreamEvent::final_response_with_total_tokens(20),
            ))
            .collect();
        let model = MockCompletionModel::from_stream_turns([events]);
        let agent = AgentBuilder::new(model).build();

        let (cmds_tx, cmds_rx) = mpsc::channel(CMD_CHANNEL_CAPACITY);
        let (status_tx, _status_rx) = watch::channel(SessionStatus::Idle);
        // Deliberately tiny — smaller than the 21 events this turn emits — so `forward` is
        // guaranteed to hit its blocking fallback rather than never exercising it.
        let (events_tx, mut events_rx) = mpsc::channel::<SessionEvent>(2);
        let mut sessions: SlotMap<SessionId, ()> = SlotMap::with_key();
        let id = sessions.insert(());
        let task = tokio::spawn(session_task(id, agent, cmds_rx, events_tx, status_tx));

        cmds_tx.send(SessionCmd::Prompt("go".into())).await.unwrap();

        // Drain slowly at first so the channel actually fills while the task is still
        // producing, then drain the rest — never dropping the sender, so the task cannot
        // mistake a closed channel for anything but a slow consumer.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut token_count = 0;
        while let Some(event) = tokio::time::timeout(Duration::from_secs(2), events_rx.recv())
            .await
            .expect("draining the event channel should not hang")
        {
            if matches!(event.event, AgentEvent::Token(_)) {
                token_count += 1;
            }
            if matches!(event.event, AgentEvent::TurnComplete) {
                break;
            }
        }

        assert_eq!(token_count, 20, "no token events were dropped");
        cmds_tx.send(SessionCmd::Shutdown).await.unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_aborts_an_in_flight_turn_within_one_await_point() {
        let started = Arc::new(Notify::new());
        let allow_finish = Arc::new(Notify::new());
        let model = MockCompletionModel::from_stream_turns([vec![MockStreamEvent::tool_call(
            "call_1",
            "controlled",
            serde_json::json!({}),
        )]]);
        let agent = AgentBuilder::new(model)
            .tool(MockControlledTool::new(started.clone(), allow_finish))
            .build();
        let (_id, handle, events) = spawn_session(agent);

        handle.send(SessionCmd::Prompt("go".into())).await.unwrap();
        // Waits for the tool call to genuinely be in flight — proves the turn is stuck on a
        // real await point, not merely "not yet scheduled".
        started.notified().await;

        handle.send(SessionCmd::Shutdown).await.unwrap();

        let mut status = handle.status.clone();
        tokio::time::timeout(
            Duration::from_millis(500),
            status.wait_for(|s| *s == SessionStatus::Closed),
        )
        .await
        .expect("shutdown should abort the stuck turn promptly, not hang on it")
        .unwrap();

        // The channel is still open on the sender side; nothing was forced to close it, the
        // task just stopped producing once cancelled.
        drop(events);
    }

    #[tokio::test]
    async fn a_panicking_session_leaves_other_sessions_running() {
        let panicking_model =
            MockCompletionModel::from_stream_turns([vec![MockStreamEvent::tool_call(
                "call_1",
                "panic_tool",
                serde_json::json!({}),
            )]]);
        let panicking_agent = AgentBuilder::new(panicking_model).tool(PanicTool).build();

        let (cmds_tx_a, cmds_rx_a) = mpsc::channel(CMD_CHANNEL_CAPACITY);
        let (status_tx_a, status_rx_a) = watch::channel(SessionStatus::Idle);
        let (events_tx_a, _events_rx_a) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let mut sessions: SlotMap<SessionId, ()> = SlotMap::with_key();
        let id_a = sessions.insert(());
        spawn_supervised(id_a, panicking_agent, cmds_rx_a, events_tx_a, status_tx_a);
        cmds_tx_a
            .send(SessionCmd::Prompt("break".into()))
            .await
            .unwrap();

        let ok_model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("fine"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let ok_agent = AgentBuilder::new(ok_model).build();
        let (id_b, handle_b, mut events_b) = spawn_session(ok_agent);
        handle_b
            .send(SessionCmd::Prompt("hi".into()))
            .await
            .unwrap();
        loop {
            let event = events_b
                .recv()
                .await
                .expect("healthy session should emit events");
            assert_eq!(event.session, id_b);
            if matches!(event.event, AgentEvent::TurnComplete) {
                break;
            }
        }
        let mut status_b = handle_b.status.clone();
        status_b
            .wait_for(|s| *s == SessionStatus::Idle)
            .await
            .unwrap();

        let mut status_a = status_rx_a;
        tokio::time::timeout(
            Duration::from_millis(500),
            status_a.wait_for(|s| *s == SessionStatus::Errored),
        )
        .await
        .expect("the panicking session should be reported errored, not hang")
        .unwrap();
    }

    #[tokio::test]
    async fn spawning_past_max_sessions_errs_without_touching_the_network() {
        let backend = Arc::new(
            Backend::huggingface("dummy-key", None, None).expect("offline client construction"),
        );
        let (mut manager, _events) = SessionManager::new(backend, 1);

        let spec = crate::config::SessionSpec {
            title: "t".to_string(),
            root: std::path::PathBuf::from("."),
            agent: crate::config::AgentSpec {
                model: "org/model".to_string(),
                sub_provider: None,
                base_url: None,
                preamble: String::new(),
                temperature: 0.2,
                max_tokens: 512,
                max_turns: 4,
                http: crate::config::HttpPolicy::default(),
                may_delegate: false,
                delegation: crate::config::DelegationPolicy::default(),
            },
            delegation: crate::config::DelegationPolicy::default(),
            max_turns: 4,
        };
        let ctx = ToolCtx {
            agent: AgentId::ROOT,
            root: std::path::PathBuf::from("."),
            max_output_bytes: 1_000_000,
            spawner: None,
            activity: tokio::sync::mpsc::channel(1).0,
            cancel: CancellationToken::new(),
        };

        assert!(manager.spawn(&spec, ctx.clone()).is_ok());
        assert_eq!(manager.len(), 1);
        assert!(matches!(
            manager.spawn(&spec, ctx),
            Err(SessionError::TooManySessions(1))
        ));
    }
}
