use crate::domain::SpecId;

/// Published on the command bus's broadcast channel whenever the in-memory
/// index changes — consumed by the SSE endpoint for live reload.
#[derive(Debug, Clone)]
pub enum SpecChanged {
    /// The whole tree was re-walked (startup, or a create/rename/remove).
    IndexRefreshed,
    /// One spec's cached render was invalidated (content edit).
    Spec(SpecId),
}
