//! Live-router smoke test (§12): confirms `Backend` actually authenticates against
//! HuggingFace Inference Providers. Requires a real `API_TOKEN` and network access, so it's
//! `#[ignore]`d — run manually, never in CI.

use mate_core::backend::Backend;
use rig::client::VerifyClient;

#[tokio::test]
#[ignore = "hits the live HuggingFace router; run manually with API_TOKEN set"]
async fn reaches_the_router() {
    let token = std::env::var("API_TOKEN").expect("set API_TOKEN to run this test");
    let backend = Backend::new(token, None).expect("failed to build HuggingFace client");

    backend
        .client()
        .verify()
        .await
        .expect("HuggingFace router rejected the token");
}
