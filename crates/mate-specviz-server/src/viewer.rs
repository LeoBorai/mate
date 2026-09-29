//! The public entry points `mate` uses: bind a loopback listener, then either
//! serve in the foreground until a shutdown future resolves ([`run`], for
//! `mate specviz`) or serve in a background task owned by a [`Viewer`]
//! handle ([`spawn`], for the TUI's `/specviz`).
//!
//! Binding is separate from serving so a caller learns the URL immediately —
//! before the initial index walk, which can be slow on a large root. A
//! browser that connects while indexing is still running just waits in the
//! listener's accept backlog.

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::bootstrap;

/// The port `mate specviz` binds when none is given.
pub const DEFAULT_PORT: u16 = 7732;

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    #[error("cannot bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot open {}: {source}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot watch spec sources: {0}")]
    Watch(#[from] notify::Error),
    #[error("server error: {0}")]
    Serve(#[source] std::io::Error),
}

/// Binds `addr`. Only loopback addresses are meaningful here — callers
/// always pass `127.0.0.1`; the viewer is never exposed off-host.
pub async fn bind(addr: SocketAddr) -> Result<TcpListener, ViewerError> {
    TcpListener::bind(addr)
        .await
        .map_err(|source| ViewerError::Bind { addr, source })
}

/// `http://<addr>` for a bound listener.
pub fn url_of(listener: &TcpListener) -> Result<String, ViewerError> {
    let addr = listener.local_addr().map_err(ViewerError::Serve)?;
    Ok(format!("http://{addr}"))
}

/// Builds the app for `root` and serves it on `listener` until `shutdown`
/// resolves. Not a graceful shutdown: open live-reload streams never end on
/// their own, so waiting for them would never return.
pub async fn run(
    listener: TcpListener,
    root: &Path,
    shutdown: impl Future<Output = ()>,
) -> Result<(), ViewerError> {
    let app = bootstrap::build(root).await?;
    tokio::select! {
        result = axum::serve(listener, app.router.clone()).into_future() => {
            result.map_err(ViewerError::Serve)
        }
        () = shutdown => Ok(()),
    }
}

/// A viewer serving in the background. Dropping it stops the server: the
/// listener closes and new connections are refused.
pub struct Viewer {
    url: String,
    root: PathBuf,
    _stop: DropGuard,
}

impl Viewer {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Starts serving `root` on `listener` in a background task. The returned
/// handle resolves when the server stops — with an error if it failed to
/// start or crashed, `Ok` if the [`Viewer`] was dropped.
pub fn spawn(
    listener: TcpListener,
    root: PathBuf,
) -> Result<(Viewer, JoinHandle<Result<(), ViewerError>>), ViewerError> {
    let url = url_of(&listener)?;
    let stop = CancellationToken::new();
    let task = {
        let stop = stop.clone();
        let root = root.clone();
        tokio::spawn(async move { run(listener, &root, async move { stop.cancelled().await }).await })
    };
    let viewer = Viewer {
        url,
        root,
        _stop: stop.drop_guard(),
    };
    Ok((viewer, task))
}
