use axum::body::Body;
use axum::http::{Uri, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

/// The built UI bundle (`mate-specviz-client`, built by Trunk), embedded at
/// compile time in release builds and read live from disk in debug builds.
/// `allow_missing` keeps host-only builds (CI's clippy/nextest, which never
/// run Trunk) compiling; the release workflow checks the bundle exists
/// before building `mate`.
#[derive(RustEmbed)]
#[folder = "../mate-specviz-client/dist/"]
#[allow_missing = true]
struct UiAssets;

/// Served in place of the SPA when no bundle was embedded.
const UI_NOT_BUILT: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>specviz</title></head>\
<body><h1>specviz UI not built</h1>\
<p>This build of <code>mate</code> has no embedded UI. Run <code>trunk build --release</code> \
in <code>crates/mate-specviz-client</code>, then rebuild. The JSON API under \
<code>/api/specs</code> works regardless.</p></body></html>\n";

/// Serves an embedded asset by exact path, or falls back to `index.html`
/// for client-side routing on any unknown non-API path.
pub async fn static_asset(uri: Uri) -> Response {
    serve(uri.path(), UiAssets::get)
}

/// `static_asset` with the asset lookup injected, so the missing-bundle path
/// is testable regardless of whether this build embedded one.
fn serve(path: &str, get: impl Fn(&str) -> Option<rust_embed::EmbeddedFile>) -> Response {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(file.data))
                .unwrap()
        }
        None => match get("index.html") {
            Some(file) => Response::builder()
                .header(header::CONTENT_TYPE, "text/html")
                .body(Body::from(file.data))
                .unwrap(),
            None => Html(UI_NOT_BUILT).into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;

    use super::*;

    #[tokio::test]
    async fn a_missing_bundle_serves_the_not_built_page() {
        let response = serve("/spec/foo.md", |_| None);

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the page itself loads; it just explains the UI is missing"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("specviz UI not built"),
            "the fallback page names the problem"
        );
    }
}
