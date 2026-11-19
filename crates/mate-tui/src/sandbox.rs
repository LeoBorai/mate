//! Sandbox tab plumbing: the events background Docker tasks send back to the TUI, and the
//! wiring `mate-cli` hands in at startup.
//!
//! Every Docker call runs in a spawned task that owns a clone of [`SandboxCtx::tx`] and reports
//! back as a [`SandboxEvent`], so the event loop never awaits the daemon. Events are tagged
//! with the sandbox's id; one for a sandbox with no open tab is ignored.

use std::sync::Arc;

use mate_sandbox::attach::AttachOutcome;
use mate_sandbox::claude::HeadlessResult;
use mate_sandbox::docker::DockerApi;
use mate_sandbox::error::SandboxError;
use mate_sandbox::report::SessionReport;
use mate_sandbox::session::{SandboxSession, SandboxStatus};
use mate_sandbox::store::SandboxStore;
use tokio::sync::mpsc;
use uuid::Uuid;

const EVENT_BUFFER: usize = 64;

/// One result reported by a background sandbox task.
pub enum SandboxEvent {
    /// Startup reconciliation of one stored session finished.
    Reconciled {
        id: Uuid,
        result: Result<SandboxStatus, SandboxError>,
    },
    /// A line of image pull or build progress.
    PullProgress { id: Uuid, line: String },
    /// A freshly created sandbox: fills in its tab (opened as `creating…` when the form was
    /// submitted), or opens one.
    Created { session: SandboxSession },
    /// Creating the sandbox failed before a container existed: closes its placeholder tab and
    /// reopens the form with the reason.
    CreateFailed { id: Uuid, message: String },
    /// Start or stop changed the container's status.
    StatusChanged { id: Uuid, status: SandboxStatus },
    /// A headless turn finished. `claude_session_id` is the session's id after the turn (set
    /// even when the turn failed, since it is stored before the turn runs).
    HeadlessDone {
        id: Uuid,
        claude_session_id: Option<Uuid>,
        result: Result<HeadlessResult, SandboxError>,
    },
    /// An interactive attach ended.
    AttachFinished {
        id: Uuid,
        outcome: Result<AttachOutcome, SandboxError>,
    },
    /// The inspection report for `/inspect`.
    Report { id: Uuid, report: Box<SessionReport> },
    /// The sandbox was removed: closes its tab.
    Removed { id: Uuid },
    /// Any other action failed.
    Error { id: Uuid, message: String },
}

impl SandboxEvent {
    /// The sandbox this event is about.
    pub(crate) fn id(&self) -> Uuid {
        match self {
            Self::Reconciled { id, .. }
            | Self::PullProgress { id, .. }
            | Self::StatusChanged { id, .. }
            | Self::HeadlessDone { id, .. }
            | Self::AttachFinished { id, .. }
            | Self::Report { id, .. }
            | Self::Removed { id }
            | Self::CreateFailed { id, .. }
            | Self::Error { id, .. } => *id,
            Self::Created { session } => session.id,
        }
    }
}

/// What the TUI needs to run sandbox tabs: the Docker client (absent when it could not be
/// built, which is not fatal), the session store, the stored sessions to open as tabs, and the
/// event channel background tasks report on.
pub struct SandboxWiring {
    pub(crate) settings: SandboxSettings,
    pub(crate) docker: Option<Arc<dyn DockerApi>>,
    pub(crate) store: Option<SandboxStore>,
    pub(crate) sessions: Vec<SandboxSession>,
    pub(crate) events: mpsc::Receiver<SandboxEvent>,
    pub(crate) tx: mpsc::Sender<SandboxEvent>,
}

impl SandboxWiring {
    pub fn new(
        docker: Option<Arc<dyn DockerApi>>,
        store: Option<SandboxStore>,
        sessions: Vec<SandboxSession>,
    ) -> Self {
        let (tx, events) = mpsc::channel(EVENT_BUFFER);
        Self {
            settings: SandboxSettings::default(),
            docker,
            store,
            sessions,
            events,
            tx,
        }
    }

    /// Defaults for sandboxes created from the spawn form.
    pub fn with_settings(mut self, settings: SandboxSettings) -> Self {
        self.settings = settings;
        self
    }

    /// No sandboxes: no client, no store, no tabs.
    pub fn disabled() -> Self {
        Self::new(None, None, Vec::new())
    }
}

/// Defaults for new sandboxes: an optional image override (unset means the embedded image) and
/// the resource limits.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxSettings {
    pub image: Option<String>,
    /// Bytes.
    pub memory: i64,
    pub pids: i64,
    pub cpus: f64,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            image: None,
            memory: mate_sandbox::policy::SandboxPolicy::DEFAULT_MEMORY,
            pids: mate_sandbox::policy::SandboxPolicy::DEFAULT_PIDS,
            cpus: mate_sandbox::policy::SandboxPolicy::DEFAULT_CPUS,
        }
    }
}

/// The handles an `App` keeps to start sandbox actions. Holding `tx` here also keeps the event
/// channel open for the life of the app.
pub(crate) struct SandboxCtx {
    pub(crate) settings: SandboxSettings,
    pub(crate) docker: Option<Arc<dyn DockerApi>>,
    // Read once sandbox actions persist session changes.
    #[allow(dead_code)]
    pub(crate) store: Option<SandboxStore>,
    pub(crate) tx: mpsc::Sender<SandboxEvent>,
}

/// A sandbox's status as the tab shows it. `Checking` until reconciliation answers;
/// `Unreachable` when the daemon could not be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxState {
    Checking,
    /// The form was submitted and the image, volume and container are being made.
    Creating,
    Known(SandboxStatus),
    Unreachable,
    /// Asking Docker failed for some reason other than the daemon being unreachable.
    Failed,
}

impl SandboxState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Checking => "checking…",
            Self::Creating => "creating…",
            Self::Known(SandboxStatus::Running) => "running",
            Self::Known(SandboxStatus::Exited) => "exited",
            Self::Known(SandboxStatus::Created) => "created",
            Self::Known(SandboxStatus::Missing) => "missing",
            Self::Unreachable => "Docker daemon unreachable",
            Self::Failed => "status unknown",
        }
    }
}
