//! `mate specviz`: the spec viewer in the foreground. Runs before config loading, the first-run
//! notice, and any backend or MCP setup — viewing specs needs none of them, so it works with
//! no API token and never spawns an MCP server.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use mate_specviz_server::ViewerError;

/// Serves `dir` on `127.0.0.1:<port>` until Ctrl+C. The one startup line goes to stdout (the
/// library itself never prints); the URL ends the line so terminals can detect it as a link.
pub async fn run(dir: &Path, port: u16) -> Result<(), ViewerError> {
    let root = dunce::canonicalize(dir).map_err(|source| ViewerError::Root {
        path: dir.to_path_buf(),
        source,
    })?;
    let listener =
        mate_specviz_server::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
    let url = mate_specviz_server::url_of(&listener)?;

    println!("specviz serving {} at {url}", root.display());
    tracing::info!(root = %root.display(), %url, "specviz started");

    mate_specviz_server::run(listener, &root, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_root_names_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");

        let err = run(&missing, 0).await.unwrap_err();
        assert!(
            err.to_string().contains(&missing.display().to_string()),
            "the error names the directory that doesn't exist: {err}"
        );
    }

    #[tokio::test]
    async fn an_occupied_port_names_the_port() {
        let tmp = tempfile::tempdir().unwrap();
        let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = taken.local_addr().unwrap().port();

        let err = run(tmp.path(), port).await.unwrap_err();
        assert!(
            err.to_string().contains(&port.to_string()),
            "the error names the port already in use: {err}"
        );
    }
}
