use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// A raw filesystem change under one of the watched source dirs. Deliberately not
/// a domain/application type — infra stays below application in the
/// dependency direction, so it hands back plain paths + the `notify` event
/// kind and lets the composition root (`bootstrap`) decide which command to
/// dispatch (a content-only `Modify` -> `InvalidateSpec`, anything that can
/// reshape the tree -> `RefreshSpecIndex`).
#[derive(Debug, Clone)]
pub struct SpecFsEvent {
    pub kind: EventKind,
    pub paths: Vec<PathBuf>,
}

/// Holds the live `notify` watcher so it isn't dropped (and stops firing).
pub struct SpecWatcher {
    _inner: RecommendedWatcher,
}

/// Starts one watcher over every directory in `dirs`, forwarding raw
/// filesystem events over the returned channel. A directory that doesn't
/// exist is skipped rather than failing — sources are only detected at
/// startup, so a source created later is picked up on the next start.
pub fn watch(
    dirs: &[&Path],
) -> notify::Result<(SpecWatcher, mpsc::UnboundedReceiver<SpecFsEvent>)> {
    let (tx, rx) = mpsc::unbounded_channel();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            if event.kind.is_access() {
                return;
            }
            let _ = tx.send(SpecFsEvent {
                kind: event.kind,
                paths: event.paths,
            });
        }
    })?;

    for dir in dirs {
        if dir.is_dir() {
            watcher.watch(dir, RecursiveMode::Recursive)?;
        }
    }

    Ok((SpecWatcher { _inner: watcher }, rx))
}
