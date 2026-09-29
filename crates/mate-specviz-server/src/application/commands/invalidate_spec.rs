use crate::domain::SpecId;

/// Signals that a single spec's content changed on disk. `RenderSpec`
/// always re-renders from disk (no persisted state), so today this command
/// has no index to mutate — it exists as the command-bus entry point that
/// the watcher drives and that publishes `SpecChanged::Spec(id)` for SSE. A
/// per-spec render cache would hook into `handle` here without changing
/// the command's shape.
pub struct InvalidateSpec(pub SpecId);

pub fn handle(_cmd: &InvalidateSpec) {}
