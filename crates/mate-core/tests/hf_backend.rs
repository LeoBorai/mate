//! Live smoke test (§12): confirms `Backend` actually authenticates against HuggingFace —
//! `Backend::verify`'s default-router path checks the token against the Hub directly (see
//! `verify_huggingface_hub_token` in `backend.rs`), since the router itself doesn't serve the
//! whoami route Rig's own `verify()` would otherwise call. Requires a real `API_TOKEN` and
//! network access, so it's `#[ignore]`d — run manually, never in CI.

use mate_core::backend::Backend;

#[tokio::test]
#[ignore = "hits the live HuggingFace Hub; run manually with API_TOKEN set"]
async fn reaches_the_router() {
    let token = std::env::var("API_TOKEN").expect("set API_TOKEN to run this test");
    let backend =
        Backend::huggingface(token, None, None).expect("failed to build HuggingFace client");

    backend
        .verify()
        .await
        .expect("HuggingFace rejected the token");
}
