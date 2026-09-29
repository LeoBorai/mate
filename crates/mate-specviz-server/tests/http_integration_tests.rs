//! End-to-end coverage of the real axum `Router` — route wiring, the
//! static-asset SPA fallback, and the SSE live-reload stream — none of
//! which is exercised by the unit tests living alongside each module.

use std::path::Path;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use futures::StreamExt;
use mate_specviz_server::bootstrap::{self, App};
use tempfile::TempDir;
use tower::ServiceExt;

/// Builds an app rooted at a fresh temp directory (with an empty `specs/`
/// already created), letting `setup` populate fixture files before the
/// initial index is built.
async fn fixture_app(setup: impl FnOnce(&Path)) -> (App, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("specs")).unwrap();
    setup(tmp.path());

    let app = bootstrap::build(tmp.path()).await.unwrap();
    (app, tmp)
}

#[tokio::test]
async fn list_specs_returns_the_fixture_tree() {
    let (app, _tmp) = fixture_app(|dir| {
        std::fs::write(dir.join("specs/root.md"), "# Root\n").unwrap();
    })
    .await;

    let response = app
        .router
        .clone()
        .oneshot(Request::get("/api/specs").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK, "the tree endpoint always answers");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let file = &json["nodes"][0]["children"][0];
    assert_eq!(json["nodes"][0]["kind"], "plain", "specs/ is the plain section");
    assert_eq!(file["type"], "file", "a top-level spec is a file node");
    assert_eq!(file["id"], "specs/root.md", "ids are relative to the served root");
}

#[tokio::test]
async fn get_spec_returns_rendered_html_for_an_existing_spec() {
    let (app, _tmp) = fixture_app(|dir| {
        std::fs::write(dir.join("specs/root.md"), "# Root\n\nhello").unwrap();
    })
    .await;

    let response = app
        .router
        .clone()
        .oneshot(
            Request::get("/api/specs/specs/root.md")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK, "an existing spec renders");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["title"], "Root", "the title is the first H1");
    assert!(
        json["html"].as_str().unwrap().contains("<h1>Root</h1>"),
        "the body is rendered HTML"
    );
}

#[tokio::test]
async fn get_spec_returns_404_for_a_missing_spec() {
    let (app, _tmp) = fixture_app(|_| {}).await;

    let response = app
        .router
        .clone()
        .oneshot(
            Request::get("/api/specs/specs/missing.md")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND, "no such document");
}

#[tokio::test]
async fn unknown_non_api_paths_fall_back_to_index_html() {
    let (app, _tmp) = fixture_app(|_| {}).await;

    let response = app
        .router
        .clone()
        .oneshot(
            Request::get("/some/client-side/route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "client-side routes survive a reload"
    );
    let content_type = response.headers().get(header::CONTENT_TYPE).unwrap();
    assert!(
        content_type.to_str().unwrap().starts_with("text/html"),
        "the fallback is the entry page (or the not-built page), both HTML"
    );
}

#[tokio::test]
async fn events_emits_spec_changed_on_file_write() {
    let (app, tmp) = fixture_app(|_| {}).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/events"))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();

    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(tmp.path().join("specs/new.md"), "# New\n").unwrap();

    let event_text = tokio::time::timeout(Duration::from_secs(5), async {
        let mut buf = Vec::new();
        loop {
            let chunk = stream.next().await.unwrap().unwrap();
            buf.extend_from_slice(&chunk);
            let text = String::from_utf8_lossy(&buf).into_owned();
            if text.contains("spec-changed") {
                return text;
            }
        }
    })
    .await
    .expect("no spec-changed SSE event within timeout");

    assert!(event_text.contains("spec-changed"), "a new file emits a live-reload event");
}

#[tokio::test]
async fn parent_traversal_is_a_404() {
    let (app, _tmp) = fixture_app(|dir| {
        std::fs::write(dir.join("secret.md"), "nope").unwrap();
    })
    .await;

    let response = app
        .router
        .clone()
        .oneshot(
            Request::get("/api/specs/specs/../secret.md")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a path outside every source is indistinguishable from a missing one"
    );
}

#[tokio::test]
async fn events_fire_for_edits_under_openspec() {
    let (app, tmp) = fixture_app(|dir| {
        std::fs::create_dir_all(dir.join("openspec/changes/add-x")).unwrap();
        std::fs::write(dir.join("openspec/changes/add-x/tasks.md"), "- [ ] a\n").unwrap();
    })
    .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let response = reqwest::Client::new()
        .get(format!("http://{addr}/events"))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();

    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::write(
        tmp.path().join("openspec/changes/add-x/tasks.md"),
        "- [x] a\n",
    )
    .unwrap();

    let event_text = tokio::time::timeout(Duration::from_secs(5), async {
        let mut buf = Vec::new();
        loop {
            let chunk = stream.next().await.unwrap().unwrap();
            buf.extend_from_slice(&chunk);
            let text = String::from_utf8_lossy(&buf).into_owned();
            if text.contains("spec-changed") {
                return text;
            }
        }
    })
    .await
    .expect("no spec-changed SSE event within timeout");

    assert!(
        event_text.contains(r#""kind":"index""#),
        "checking a task re-walks the tree so the progress badge updates: {event_text}"
    );
}

#[tokio::test]
async fn a_spawned_viewer_serves_on_loopback_until_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("specs")).unwrap();

    let listener = mate_specviz_server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (viewer, task) = mate_specviz_server::spawn(listener, tmp.path().to_path_buf()).unwrap();
    let url = viewer.url().to_owned();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the viewer only listens on loopback: {url}"
    );

    let response = reqwest::get(format!("{url}/api/specs")).await.unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "the spawned viewer answers the tree endpoint"
    );

    drop(viewer);
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the server task stops after its Viewer is dropped")
        .unwrap();
    assert!(result.is_ok(), "a dropped viewer stops cleanly: {result:?}");
    assert!(
        reqwest::get(format!("{url}/api/specs")).await.is_err(),
        "after the Viewer is dropped its port refuses new connections"
    );
}

#[tokio::test]
async fn run_returns_ok_when_shutdown_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = mate_specviz_server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        mate_specviz_server::run(listener, tmp.path(), async {}),
    )
    .await
    .expect("run returns promptly once shutdown resolves");
    assert!(result.is_ok(), "an immediate shutdown is a clean exit: {result:?}");
}

#[tokio::test]
async fn binding_an_occupied_port_names_the_address() {
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = taken.local_addr().unwrap();

    let err = mate_specviz_server::bind(addr).await.unwrap_err();
    assert!(
        err.to_string().contains(&addr.port().to_string()),
        "the bind error names the port: {err}"
    );
}

#[tokio::test]
async fn a_missing_root_names_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope");

    let err = bootstrap::build(&missing).await.err().unwrap();
    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "the root error names the directory: {err}"
    );
}
