//! Errors for the sandbox crate.

/// Everything that can go wrong driving a sandbox session.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The Docker daemon could not be reached (socket, connection, timeout).
    #[error("Docker daemon unreachable")]
    DaemonUnavailable,
    /// The container (or other Docker object) no longer exists.
    #[error("not found: {0}")]
    NotFound(String),
    /// The container lacks the `managed-by=mate` / `session-id` labels this
    /// session expects, so it must not be touched.
    #[error("container {0} is not managed by mate for this session")]
    NotOurs(String),
    /// The requested policy is refused before any container is created.
    #[error("invalid sandbox policy: {0}")]
    InvalidPolicy(String),
    /// The image does not meet the image contract (tmux, user-owned state dir).
    #[error("image contract violated: {0}")]
    ImageContract(String),
    /// A headless Claude Code turn failed or returned unusable output.
    #[error("headless turn failed (exit {exit_code:?}): {detail}")]
    Headless {
        exit_code: Option<i64>,
        stderr: String,
        detail: String,
    },
    /// Reading or writing the session store failed.
    #[error("sandbox store error: {0}")]
    Store(String),
    /// Any other Docker API failure.
    #[error("docker {op} failed: {source}")]
    Docker {
        op: &'static str,
        #[source]
        source: bollard::errors::Error,
    },
}
