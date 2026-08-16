//! `M10-9`'s wiremock suite: exercises the `http_request` tool end to end against a real
//! (loopback) HTTP server, with no live network reached. Every test talks to `wiremock`'s
//! server on `127.0.0.1`, so every tool instance here is built with `allow_localhost: true` —
//! that's the only way to reach the mock server at all; it has no bearing on what's being
//! tested (redirect validation, size caps, content-type refusal, header hygiene, status
//! surfacing), which all still exercise the real guard logic on whatever URL a response
//! points at next.

use std::sync::Arc;
use std::time::Duration;

use mate_tool_api::{AgentId, ToolCtx, ToolFailure};
use mate_tool_http::{HttpLimits, HttpRequest, HttpRequestArgs, HttpShared};
use rig::tool::PortableTool;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx() -> ToolCtx {
    let (activity, _rx) = tokio::sync::mpsc::channel(8);
    ToolCtx {
        agent: AgentId::ROOT,
        root: std::env::temp_dir(),
        max_output_bytes: 1_000_000,
        spawner: None,
        activity,
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

fn tool(shared: Arc<HttpShared>) -> HttpRequest {
    HttpRequest::new(ctx(), shared, true)
}

fn args(url: String) -> HttpRequestArgs {
    HttpRequestArgs {
        url,
        method: None,
        headers: None,
        render_text: None,
    }
}

#[tokio::test]
async fn a_redirect_to_a_private_ip_is_blocked_at_the_second_hop() {
    let server = MockServer::start().await;
    // The server's own reply doesn't matter — `Location` points somewhere the SSRF guard
    // must refuse before ever connecting to it.
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "http://169.254.169.254/secret"),
        )
        .mount(&server)
        .await;

    let shared = Arc::new(HttpShared::new(600).unwrap());
    let err = tool(shared)
        .call(args(format!("{}/start", server.uri())))
        .await
        .unwrap_err();

    assert!(
        matches!(err, ToolFailure::Denied(reason) if reason.contains("link-local")),
        "a redirect toward cloud-metadata-range space must be denied, not followed"
    );
}

#[tokio::test]
async fn a_redirect_loop_stops_at_the_cap_instead_of_hanging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/loop"))
        .mount(&server)
        .await;

    let shared = Arc::new(HttpShared::new(6000).unwrap());
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        tool(shared).call(args(format!("{}/loop", server.uri()))),
    )
    .await
    .expect("a redirect loop must stop at the hop cap, never hang");

    // Capped at max_redirects (5): the loop stops following and reports whatever it has —
    // still a redirect status, with the hop count at the cap.
    let text = outcome.expect("stopping at the cap is a normal result, not a tool failure");
    assert!(text.starts_with("302 "), "text was: {text}");
    assert!(text.contains("redirects: 5"), "text was: {text}");
}

#[tokio::test]
async fn an_oversized_body_aborts_mid_stream_without_buffering_the_whole_thing() {
    let server = MockServer::start().await;
    let big_body = "a".repeat(10_000);
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(big_body)
                .insert_header("Content-Type", "text/plain"),
        )
        .mount(&server)
        .await;

    let shared = Arc::new(
        HttpShared::with_limits(
            600,
            HttpLimits {
                max_response_bytes: 100,
                ..HttpLimits::default()
            },
        )
        .unwrap(),
    );

    let err = tool(shared)
        .call(args(format!("{}/big", server.uri())))
        .await
        .unwrap_err();

    assert!(matches!(err, ToolFailure::TooLarge { limit: 100 }));
}

#[tokio::test]
async fn an_octet_stream_response_is_refused_by_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0u8, 1, 2, 3])
                .insert_header("Content-Type", "application/octet-stream"),
        )
        .mount(&server)
        .await;

    let shared = Arc::new(HttpShared::new(600).unwrap());
    let err = tool(shared)
        .call(args(format!("{}/bin", server.uri())))
        .await
        .unwrap_err();

    assert!(matches!(err, ToolFailure::Denied(reason) if reason.contains("octet-stream")));
}

#[tokio::test]
async fn a_model_supplied_authorization_header_never_reaches_the_server() {
    let server = MockServer::start().await;
    // If the header ever made it through, this mock (which only matches requests carrying
    // an Authorization header) would be the one that responds; since the tool must refuse
    // before sending anything, no mock needs to match at all — verified below via
    // `received_requests`.
    Mock::given(method("GET"))
        .and(path("/secure"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let shared = Arc::new(HttpShared::new(600).unwrap());
    let mut request = args(format!("{}/secure", server.uri()));
    request.headers = Some(
        [("Authorization".to_string(), "Bearer stolen".to_string())]
            .into_iter()
            .collect(),
    );

    let err = tool(shared).call(request).await.unwrap_err();
    assert!(matches!(err, ToolFailure::Denied(_)));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "a denied header must stop the request before it ever reaches the network"
    );
}

#[tokio::test]
async fn a_429_response_surfaces_to_the_model_instead_of_erroring() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/limited"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_string("slow down")
                .insert_header("Content-Type", "text/plain"),
        )
        .mount(&server)
        .await;

    let shared = Arc::new(HttpShared::new(600).unwrap());
    let text = tool(shared)
        .call(args(format!("{}/limited", server.uri())))
        .await
        .expect("an upstream 429 is information for the model, not a tool failure");

    assert!(text.starts_with("429 "), "text was: {text}");
    assert!(text.contains("slow down"));
}
