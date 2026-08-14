//! `Backend` (§4): the process-wide, provider-backed client that every session's and
//! subagent's `Agent` is built from — one auth setup, one pooled HTTP connection underneath.
//!
//! Two provider paths, config-selected (`M1-3`):
//!
//! - [`Backend::huggingface`] — the default. Talks to HuggingFace Inference Providers
//!   natively, optionally against a `base_url` override (a dedicated Inference Endpoint, or
//!   a local server speaking HF's wire format).
//! - [`Backend::openai_compatible`] — the escape hatch. Talks to any OpenAI-compatible chat
//!   completions endpoint: the HF router's own OpenAI-compatible surface
//!   ([`HF_ROUTER_OPENAI_BASE_URL`]), for the day Rig's native HuggingFace provider lags a
//!   router change, or an arbitrary OpenAI-compatible server (local TGI/vLLM).
//!
//! Both variants share the same `Backend` type so callers elsewhere in `mate-core` (`M1-2`'s
//! `build_agent`) don't need to know which path is live — see [`crate::agent::BuiltAgent`].

use rig::client::VerifyClient;
use rig::providers::{huggingface, openai};

/// The HuggingFace router's OpenAI-compatible base URL — the default target for the
/// [`Backend::openai_compatible`] fallback path.
pub const HF_ROUTER_OPENAI_BASE_URL: &str = "https://router.huggingface.co/v1";

/// Shared across every session and subagent in the process (§5.3): one provider client, one
/// connection pool, one auth setup.
pub enum Backend {
    HuggingFace(huggingface::Client),
    OpenAiCompatible(openai::CompletionsClient),
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("failed to build provider client: {0}")]
    Client(#[from] rig::http_client::Error),
}

impl Backend {
    /// Builds the shared backend against HuggingFace Inference Providers — the native path.
    ///
    /// `api_key` is the caller's `API_TOKEN` (read from the environment at the CLI boundary,
    /// never here — `mate-core` takes the value, not the env var). `sub_provider` selects the
    /// inference partner serving the model (`"together"`, `"fireworks"`, …); `None` or an
    /// unrecognized name falls back to HF's own `hf-inference`, never a hard error, since the
    /// set of partners changes independently of `mate`. `base_url` overrides the router — a
    /// dedicated Inference Endpoint, or a local server speaking HF's wire format.
    pub fn huggingface(
        api_key: impl AsRef<str>,
        sub_provider: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Self, BackendError> {
        let mut builder = huggingface::Client::builder()
            .api_key(api_key.as_ref())
            .subprovider(parse_sub_provider(sub_provider));
        if let Some(base_url) = base_url {
            builder = builder.base_url(base_url);
        }
        Ok(Self::HuggingFace(builder.build()?))
    }

    /// Builds the shared backend against an OpenAI-compatible endpoint — the fallback path.
    /// Point `base_url` at [`HF_ROUTER_OPENAI_BASE_URL`] to reach the HuggingFace router's
    /// OpenAI-compatible surface, or at an arbitrary OpenAI-compatible server (a local
    /// TGI/vLLM instance).
    pub fn openai_compatible(
        api_key: impl AsRef<str>,
        base_url: impl AsRef<str>,
    ) -> Result<Self, BackendError> {
        let client = openai::Client::builder()
            .api_key(api_key.as_ref())
            .base_url(base_url.as_ref())
            .build()?
            .completions_api();
        Ok(Self::OpenAiCompatible(client))
    }

    /// Verifies the configured provider actually authenticates, regardless of path.
    pub async fn verify(&self) -> Result<(), rig::client::VerifyError> {
        match self {
            Self::HuggingFace(client) => client.verify().await,
            Self::OpenAiCompatible(client) => client.verify().await,
        }
    }
}

fn parse_sub_provider(name: Option<&str>) -> huggingface::SubProvider {
    use huggingface::SubProvider;

    let Some(name) = name else {
        return SubProvider::HFInference;
    };
    match name.to_ascii_lowercase().as_str() {
        "hf-inference" | "hfinference" | "huggingface" => SubProvider::HFInference,
        "together" => SubProvider::Together,
        "sambanova" => SubProvider::SambaNova,
        "fireworks" => SubProvider::Fireworks,
        "hyperbolic" => SubProvider::Hyperbolic,
        "nebius" => SubProvider::Nebius,
        "novita" => SubProvider::Novita,
        other => SubProvider::Custom(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use huggingface::SubProvider;

    #[test]
    fn sub_provider_recognizes_known_partners() {
        assert_eq!(parse_sub_provider(None), SubProvider::HFInference);
        assert_eq!(
            parse_sub_provider(Some("hf-inference")),
            SubProvider::HFInference
        );
        assert_eq!(parse_sub_provider(Some("Together")), SubProvider::Together);
        assert_eq!(
            parse_sub_provider(Some("SAMBANOVA")),
            SubProvider::SambaNova
        );
        assert_eq!(
            parse_sub_provider(Some("fireworks")),
            SubProvider::Fireworks
        );
        assert_eq!(
            parse_sub_provider(Some("hyperbolic")),
            SubProvider::Hyperbolic
        );
        assert_eq!(parse_sub_provider(Some("nebius")), SubProvider::Nebius);
        assert_eq!(parse_sub_provider(Some("novita")), SubProvider::Novita);
    }

    #[test]
    fn sub_provider_falls_back_to_custom() {
        assert_eq!(
            parse_sub_provider(Some("some-new-partner")),
            SubProvider::Custom("some-new-partner".to_string())
        );
    }

    #[test]
    fn builds_offline_with_any_recognized_sub_provider() {
        for sub_provider in [
            None,
            Some("together"),
            Some("fireworks"),
            Some("a-future-partner"),
        ] {
            Backend::huggingface("dummy-key", sub_provider, None)
                .expect("client construction never contacts the network");
        }
    }

    #[test]
    fn builds_offline_with_a_huggingface_base_url_override() {
        Backend::huggingface("dummy-key", None, Some("https://my-endpoint.example.com"))
            .expect("base_url override never contacts the network");
    }

    #[test]
    fn builds_offline_against_the_hf_router_openai_fallback() {
        Backend::openai_compatible("dummy-key", HF_ROUTER_OPENAI_BASE_URL)
            .expect("client construction never contacts the network");
    }

    #[test]
    fn builds_offline_against_an_arbitrary_openai_compatible_base_url() {
        Backend::openai_compatible("dummy-key", "http://127.0.0.1:8080/v1")
            .expect("client construction never contacts the network");
    }
}
