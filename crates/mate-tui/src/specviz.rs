//! `/specviz`: background spec viewers, at most one per workspace root. [`Viewers::ensure`]
//! binds inline (fast — the URL is known before anything slow happens) and leaves the index
//! walk and serving to a background task, so the event loop never waits on a large tree.
//! A viewer that fails after that point reports back through [`Viewers::exits`], which
//! `app::run_loop` `select!`s on alongside terminal and session events. Every viewer stops
//! when `App` (and so [`Viewers`]) is dropped on quit.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use mate_core::session::SessionId;
use mate_specviz_server::{Viewer, ViewerError};
use tokio::sync::mpsc;

/// A background viewer stopped with an error.
#[derive(Debug)]
pub(crate) struct ViewerExit {
    pub(crate) root: PathBuf,
    /// Identifies which viewer for `root` this was, so a stale exit can't evict a newer
    /// viewer started for the same root after it.
    pub(crate) url: String,
    /// The tab that issued the `/specviz` that started it — the one to tell.
    pub(crate) tab: SessionId,
    pub(crate) error: String,
}

pub(crate) struct Viewers {
    running: HashMap<PathBuf, Viewer>,
    exits_tx: mpsc::UnboundedSender<ViewerExit>,
    pub(crate) exits: mpsc::UnboundedReceiver<ViewerExit>,
}

impl Viewers {
    pub(crate) fn new() -> Self {
        let (exits_tx, exits) = mpsc::unbounded_channel();
        Self {
            running: HashMap::new(),
            exits_tx,
            exits,
        }
    }

    /// The URL serving `root` — an already-running viewer's if there is one, otherwise a new
    /// viewer's on an OS-assigned loopback port. Returns the canonical root alongside it.
    pub(crate) async fn ensure(
        &mut self,
        root: &Path,
        tab: SessionId,
    ) -> Result<(PathBuf, String), ViewerError> {
        let root = dunce::canonicalize(root).map_err(|source| ViewerError::Root {
            path: root.to_path_buf(),
            source,
        })?;
        if let Some(viewer) = self.running.get(&root) {
            return Ok((root, viewer.url().to_owned()));
        }

        let listener =
            mate_specviz_server::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
        let (viewer, task) = mate_specviz_server::spawn(listener, root.clone())?;
        let url = viewer.url().to_owned();

        let exits = self.exits_tx.clone();
        let exit_root = root.clone();
        let exit_url = url.clone();
        tokio::spawn(async move {
            let error = match task.await {
                Ok(Ok(())) => return,
                Ok(Err(err)) => err.to_string(),
                Err(join) => format!("viewer task failed: {join}"),
            };
            tracing::warn!(root = %exit_root.display(), %error, "specviz stopped");
            let _ = exits.send(ViewerExit {
                root: exit_root,
                url: exit_url,
                tab,
                error,
            });
        });

        tracing::info!(root = %root.display(), %url, "specviz started");
        self.running.insert(root.clone(), viewer);
        Ok((root, url))
    }

    /// Forgets the viewer `exit` reports on, so the next `/specviz` for that root starts a
    /// fresh one.
    pub(crate) fn forget(&mut self, exit: &ViewerExit) {
        if self
            .running
            .get(&exit.root)
            .is_some_and(|viewer| viewer.url() == exit.url)
        {
            self.running.remove(&exit.root);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self, root: &Path) -> bool {
        self.running.contains_key(root)
    }

    #[cfg(test)]
    pub(crate) fn exit_sender(&self) -> mpsc::UnboundedSender<ViewerExit> {
        self.exits_tx.clone()
    }
}

/// The transcript line for a serving viewer. The URL ends the line with nothing after it, so
/// terminals that detect links pick it up cleanly.
pub(crate) fn serving_line(root: &Path, url: &str) -> String {
    format!("specviz serving {} at {url}", root.display())
}
