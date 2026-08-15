//! Ties `provider_error::ProviderError::classify` to a real `VerifyError` produced by
//! `Backend::verify` against scripted HTTP responses (`M1-5`), rather than only asserting
//! against hand-built `VerifyError` values — confirms the mapping matches what rig's
//! HuggingFace client actually hands back for each status class.

use mate_core::backend::Backend;
use mate_core::provider_error::ProviderError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn verify_against(status: u16, body: &str) -> ProviderError {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/whoami-v2"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let backend = Backend::huggingface("dummy-key", None, Some(base_url.as_str()))
        .expect("failed to build the HuggingFace client");

    let error = backend
        .verify()
        .await
        .expect_err("scripted non-200 response should fail verify");

    ProviderError::classify(&error)
}

#[tokio::test]
async fn a_429_response_maps_to_rate_limited() {
    let error = verify_against(429, "rate limited").await;
    assert_eq!(error, ProviderError::RateLimited);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_503_response_maps_to_model_warming() {
    let error = verify_against(503, r#"{"error":"Model is currently loading"}"#).await;
    assert_eq!(error, ProviderError::ModelWarming);
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_401_response_maps_to_auth_failed_and_never_retries() {
    let error = verify_against(401, "invalid credentials").await;
    assert_eq!(error, ProviderError::AuthFailed);
    assert!(!error.is_retryable());
}
