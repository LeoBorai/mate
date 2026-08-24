//! [`SessionApprovalHub`] (§7.4, `M13-1`): `mate-core`'s implementation of
//! `mate_tool_api::Approvals`, the seam a tool calls through `ToolCtx::approvals` without this
//! crate ever depending on `mate-core` (§8.1 note 1) — the same shape `crate::subagent`'s
//! `SubagentSpawner` implementation already takes.
//!
//! One hub is built per session, by `crate::session::SessionManager::spawn`, and shared by the
//! root agent and every subagent's `ToolCtx` (`crate::subagent::SubagentRunner` clones the same
//! `Arc` into each one it builds) — **one approval channel per session, never per agent**, so two
//! subagents asking at once queue as two independent pending entries rather than deadlocking
//! behind whichever modal renders first (§7.4).
//!
//! A request is a row in `pending`, keyed by a fresh [`Ulid`], plus a forwarded
//! [`AgentEvent::ApprovalRequired`] tagged with whichever agent asked — the same channel and the
//! same `forward` helper every other session/subagent event already rides. [`SessionApprovalHub::resolve`]
//! is what [`crate::session::session_task`] calls on a [`crate::session::SessionCmd::Approve`],
//! and what a request's own timeout calls on itself — both paths converge on the same
//! remove-and-send, so whichever fires first wins and the other is a harmless no-op (the entry
//! is simply gone by the time it runs).
//!
//! `M13-5`'s "always allow" scope lives in `always_allowed`: a granted [`Self::resolve`] can
//! carry a directory to remember, and a later [`Self::request`] whose `path` falls under any
//! remembered directory is granted immediately — no event, no queue entry, nobody asked twice.
//! The scope is a plain directory prefix, never free text (§7.4's binary-only answer still
//! holds; this only ever short-circuits toward the same `true` a human already gave once), and
//! it lives only as long as the hub itself — one session, never persisted to disk.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use mate_tool_api::{ApprovalRequest as ToolApprovalRequest, Approvals};
use tokio::sync::{mpsc, oneshot};
use ulid::Ulid;

use crate::session::{SessionEvent, SessionId, forward};
use crate::streaming::{AgentEvent, AgentEventEnvelope};

/// Auto-deny timeout (§7.4's "default 5 min"): a request nobody answers resolves to `false`
/// rather than blocking the calling tool — and so the calling agent's turn — forever.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub struct SessionApprovalHub {
    session: SessionId,
    events_tx: mpsc::Sender<SessionEvent>,
    pending: Mutex<HashMap<Ulid, oneshot::Sender<bool>>>,
    timeout: Duration,
    /// `M13-5`: directory prefixes granted "always allow" — see the module doc.
    always_allowed: Mutex<HashSet<PathBuf>>,
}

impl SessionApprovalHub {
    pub fn new(session: SessionId, events_tx: mpsc::Sender<SessionEvent>) -> Self {
        Self::with_timeout(session, events_tx, APPROVAL_TIMEOUT)
    }

    /// Same as [`Self::new`] with an explicit timeout — real callers always get
    /// [`APPROVAL_TIMEOUT`] via `new`; this exists so the auto-deny test below doesn't have to
    /// wait five real minutes for one assertion.
    fn with_timeout(
        session: SessionId,
        events_tx: mpsc::Sender<SessionEvent>,
        timeout: Duration,
    ) -> Self {
        Self {
            session,
            events_tx,
            pending: Mutex::new(HashMap::new()),
            timeout,
            always_allowed: Mutex::new(HashSet::new()),
        }
    }

    /// Resolves one pending request (`SessionCmd::Approve`, or this request's own timeout —
    /// see the module doc for why both converge here safely). `false` if `id` wasn't pending —
    /// already resolved, or never existed — matching `SubagentRunner::cancel`'s own "report
    /// whether anything happened" contract. `remember`, when `Some` alongside `granted: true`,
    /// also adds that directory to `always_allowed` (`M13-5`) so every future request whose
    /// path falls under it skips the prompt entirely — a `granted: false` with `remember: Some`
    /// never reaches this, since the TUI only ever sends the two paired.
    pub fn resolve(&self, id: Ulid, granted: bool, remember: Option<PathBuf>) -> bool {
        if granted && let Some(dir) = remember {
            self.always_allowed
                .lock()
                .expect("not poisoned")
                .insert(dir);
        }
        match self.pending.lock().expect("not poisoned").remove(&id) {
            Some(tx) => {
                let _ = tx.send(granted);
                true
            }
            None => false,
        }
    }
}

#[async_trait]
impl Approvals for SessionApprovalHub {
    async fn request(&self, request: ToolApprovalRequest) -> bool {
        // `M13-5`: a path already covered by a remembered "always allow" directory is granted
        // immediately — no pending entry, no event, nobody asked twice for the same scope.
        if let Some(path) = &request.path {
            let always_allowed = self.always_allowed.lock().expect("not poisoned");
            if always_allowed.iter().any(|dir| path.starts_with(dir)) {
                return true;
            }
        }

        let id = Ulid::generate();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("not poisoned").insert(id, tx);

        forward(
            &self.events_tx,
            self.session,
            AgentEventEnvelope {
                agent: request.agent,
                event: AgentEvent::ApprovalRequired {
                    id,
                    name: request.name,
                    detail: request.detail,
                    path: request.path,
                    diff: request.diff,
                },
            },
        );

        let granted = tokio::time::timeout(self.timeout, rx)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false);
        // Idempotent with `resolve` above: on the happy path the entry is already gone (whoever
        // answered removed it), so this is a no-op; on a timeout it's what actually cleans the
        // entry up.
        self.pending.lock().expect("not poisoned").remove(&id);
        granted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mate_tool_api::AgentId;
    use slotmap::SlotMap;

    fn session_id() -> SessionId {
        let mut sessions: SlotMap<SessionId, ()> = SlotMap::with_key();
        sessions.insert(())
    }

    fn request(agent: AgentId) -> ToolApprovalRequest {
        ToolApprovalRequest {
            agent,
            name: "http_request".to_string(),
            detail: "POST https://example.com".to_string(),
            path: None,
            diff: None,
        }
    }

    fn write_request(agent: AgentId, path: std::path::PathBuf) -> ToolApprovalRequest {
        ToolApprovalRequest {
            agent,
            name: "write_file".to_string(),
            detail: "create a.txt".to_string(),
            path: Some(path),
            diff: None,
        }
    }

    #[tokio::test]
    async fn a_request_emits_an_event_tagged_with_the_asking_agent() {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let hub = std::sync::Arc::new(SessionApprovalHub::new(session_id(), events_tx));
        let waiter = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.request(request(AgentId(3))).await })
        };

        let event = events_rx.recv().await.expect("request must emit an event");
        assert_eq!(event.agent, AgentId(3));
        let AgentEvent::ApprovalRequired { id, .. } = event.event else {
            panic!("expected ApprovalRequired, got {:?}", event.event);
        };

        hub.resolve(id, true, None);
        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn resolves_a_pending_request_and_unblocks_the_waiter() {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let hub = std::sync::Arc::new(SessionApprovalHub::new(session_id(), events_tx));

        let waiter = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.request(request(AgentId::ROOT)).await })
        };

        let event = events_rx.recv().await.expect("request must emit an event");
        let AgentEvent::ApprovalRequired { id, .. } = event.event else {
            panic!("expected ApprovalRequired");
        };

        assert!(
            hub.resolve(id, true, None),
            "resolving a genuinely pending id must report true"
        );
        assert!(
            waiter.await.unwrap(),
            "the waiter must observe the granted decision"
        );

        assert!(
            !hub.resolve(id, true, None),
            "resolving the same id twice must be a harmless no-op, not a panic"
        );
    }

    #[tokio::test]
    async fn two_concurrent_requests_from_different_subagents_do_not_deadlock() {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let hub = std::sync::Arc::new(SessionApprovalHub::new(session_id(), events_tx));

        let a = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.request(request(AgentId(1))).await })
        };
        let b = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.request(request(AgentId(2))).await })
        };

        let first = events_rx.recv().await.unwrap();
        let second = events_rx.recv().await.unwrap();
        let id_of = |e: AgentEvent| match e {
            AgentEvent::ApprovalRequired { id, .. } => id,
            other => panic!("expected ApprovalRequired, got {other:?}"),
        };

        assert!(hub.resolve(id_of(first.event), true, None));
        assert!(hub.resolve(id_of(second.event), false, None));

        assert!(a.await.unwrap());
        assert!(!b.await.unwrap());
    }

    #[tokio::test]
    async fn remembering_a_directory_grants_a_later_request_under_it_with_no_prompt() {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let hub = std::sync::Arc::new(SessionApprovalHub::new(session_id(), events_tx));
        let dir = std::path::PathBuf::from("/workspace/src");

        let first = {
            let hub = hub.clone();
            let dir = dir.clone();
            tokio::spawn(async move {
                hub.request(write_request(AgentId::ROOT, dir.join("a.txt")))
                    .await
            })
        };
        let event = events_rx
            .recv()
            .await
            .expect("first request must emit an event");
        let AgentEvent::ApprovalRequired { id, path, .. } = event.event else {
            panic!("expected ApprovalRequired");
        };
        assert_eq!(
            path,
            Some(dir.join("a.txt")),
            "the event must carry the request's resolved path"
        );
        hub.resolve(id, true, Some(dir.clone()));
        assert!(first.await.unwrap());

        // A second request for a different file under the same remembered directory must be
        // granted immediately — no event on the channel, no pending entry to answer.
        let granted = hub
            .request(write_request(AgentId::ROOT, dir.join("b.txt")))
            .await;
        assert!(
            granted,
            "a path under a remembered directory must auto-grant"
        );
        assert!(
            events_rx.try_recv().is_err(),
            "an auto-granted request must not emit a second ApprovalRequired event"
        );
    }

    #[tokio::test]
    async fn remembering_a_directory_does_not_cover_a_sibling_directory() {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let hub = std::sync::Arc::new(SessionApprovalHub::new(session_id(), events_tx));
        let allowed = std::path::PathBuf::from("/workspace/src");
        let sibling = std::path::PathBuf::from("/workspace/docs");

        let first = {
            let hub = hub.clone();
            let allowed = allowed.clone();
            tokio::spawn(async move {
                hub.request(write_request(AgentId::ROOT, allowed.join("a.txt")))
                    .await
            })
        };
        let event = events_rx
            .recv()
            .await
            .expect("first request must emit an event");
        let AgentEvent::ApprovalRequired { id, .. } = event.event else {
            panic!("expected ApprovalRequired");
        };
        hub.resolve(id, true, Some(allowed));
        assert!(first.await.unwrap());

        let second = {
            let hub = hub.clone();
            tokio::spawn(async move {
                hub.request(write_request(AgentId::ROOT, sibling.join("b.txt")))
                    .await
            })
        };
        let event = events_rx
            .recv()
            .await
            .expect("a sibling directory must still prompt");
        let AgentEvent::ApprovalRequired { id, .. } = event.event else {
            panic!("expected ApprovalRequired");
        };
        hub.resolve(id, true, None);
        assert!(second.await.unwrap());
    }

    #[tokio::test]
    async fn an_unanswered_request_auto_denies_after_the_timeout_instead_of_hanging_forever() {
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let hub =
            SessionApprovalHub::with_timeout(session_id(), events_tx, Duration::from_millis(20));

        let run = tokio::spawn(async move { hub.request(request(AgentId::ROOT)).await });
        let _event = events_rx.recv().await.unwrap();

        assert!(
            !tokio::time::timeout(Duration::from_secs(2), run)
                .await
                .expect("the auto-deny timeout must fire on its own, not hang the test")
                .unwrap(),
            "a request nobody answers must auto-deny, not hang the calling tool forever"
        );
    }
}
