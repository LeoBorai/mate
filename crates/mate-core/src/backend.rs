//! `Backend` (§4): the process-wide, provider-backed client that every session's and
//! subagent's `Agent` is built from — one auth setup, one pooled HTTP connection underneath.
//!
//! Rig's client machinery is itself generic (`client::Client<Ext, H>`, one `Ext` per
//! provider); HuggingFace Inference Providers is the `Ext` this ticket wires up. `base_url`
//! overrides and an OpenAI-compatible fallback path land as their own ticket — this type is
//! named and shaped so that work slots in without a rename.

use rig::providers::huggingface;

/// Shared across every session and subagent in the process (§5.3): one HuggingFace `Client`,
/// one connection pool, one auth setup.
pub struct Backend {
    client: huggingface::Client,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("failed to build HuggingFace client: {0}")]
    Client(#[from] rig::http_client::Error),
}

impl Backend {
    /// Builds the shared backend against HuggingFace Inference Providers.
    ///
    /// `api_key` is the caller's `API_TOKEN` (read from the environment at the CLI boundary,
    /// never here — `mate-core` takes the value, not the env var). `sub_provider` selects the
    /// inference partner serving the model (`"together"`, `"fireworks"`, …); `None` or an
    /// unrecognized name falls back to HF's own `hf-inference`, never a hard error, since the
    /// set of partners changes independently of `mate`.
    pub fn new(api_key: impl AsRef<str>, sub_provider: Option<&str>) -> Result<Self, BackendError> {
        let client = huggingface::Client::builder()
            .api_key(api_key.as_ref())
            .subprovider(parse_sub_provider(sub_provider))
            .build()?;
        Ok(Self { client })
    }

    /// The underlying Rig client, for building agents against (§4, `M1-2`).
    pub fn client(&self) -> &huggingface::Client {
        &self.client
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
            Backend::new("dummy-key", sub_provider)
                .expect("client construction never contacts the network");
        }
    }
}
