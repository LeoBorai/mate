//! `Backend` (§4): the process-wide, provider-backed client that every session's and
//! subagent's `Agent` is built from — one auth setup, one pooled HTTP connection underneath.
//!
//! Three provider paths, config-selected (`M1-3`):
//!
//! - [`Backend::huggingface`] — the default. Talks to HuggingFace Inference Providers
//!   natively, optionally against a `base_url` override (a dedicated Inference Endpoint, or
//!   a local server speaking HF's wire format).
//! - [`Backend::openai_compatible`] — the escape hatch. Talks to any OpenAI-compatible chat
//!   completions endpoint: the HF router's own OpenAI-compatible surface
//!   ([`HF_ROUTER_OPENAI_BASE_URL`]), for the day Rig's native HuggingFace provider lags a
//!   router change, or an arbitrary OpenAI-compatible server (local TGI/vLLM).
//! - [`Backend::gemini`] — Google's Gemini API, native `generateContent` surface.
//!
//! All variants share the same `Backend` type so callers elsewhere in `mate-core` (`M1-2`'s
//! `build_agent`) don't need to know which path is live — see [`crate::agent::BuiltAgent`].

use rig::client::VerifyClient;
use rig::providers::{gemini, huggingface, openai};

/// The HuggingFace router's OpenAI-compatible base URL — the default target for the
/// [`Backend::openai_compatible`] fallback path.
pub const HF_ROUTER_OPENAI_BASE_URL: &str = "https://router.huggingface.co/v1";

/// The Hub's own account-info endpoint — lives on `huggingface.co`, never on
/// `router.huggingface.co`. See [`verify_huggingface_hub_token`] for why `Backend::verify`
/// hits this directly instead of going through Rig's client.
const HF_HUB_WHOAMI_URL: &str = "https://huggingface.co/api/whoami-v2";

/// Shared across every session and subagent in the process (§5.3): one provider client, one
/// connection pool, one auth setup.
pub enum Backend {
    HuggingFace {
        client: huggingface::Client,
        /// Suffix appended to a model id as `<model>:<qualifier>` before it reaches Rig, e.g.
        /// `"together"`. Rig's `SubProvider` only affects the outgoing chat-completions request
        /// for `Fireworks` (see [`model_qualifier`]) — everything else needs this to actually
        /// route through the chosen partner rather than silently falling back to whatever HF's
        /// router auto-selects. `None` for the default `hf-inference` partner (no suffix
        /// needed) and whenever `base_url` overrides the router (a single-model endpoint has no
        /// partner to select).
        model_qualifier: Option<String>,
        /// `Some` — with the caller's token pre-set as its `Authorization` header — only when
        /// `base_url` was left at the default router; `None` when a caller overrode it (a
        /// dedicated endpoint, a local server, a test double). See
        /// [`verify_huggingface_hub_token`] for why the default path can't just use `client`'s
        /// own (Rig-internal, inaccessible) token for this.
        hub_verify_client: Option<reqwest::Client>,
    },
    OpenAiCompatible(openai::CompletionsClient),
    Gemini(gemini::Client),
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("failed to build provider client: {0}")]
    Client(#[from] rig::http_client::Error),
    #[error("API_TOKEN is not a valid HTTP header value: {0}")]
    InvalidApiKey(#[from] reqwest::header::InvalidHeaderValue),
    #[error("failed to build the HuggingFace hub verification client: {0}")]
    HubClient(#[from] reqwest::Error),
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
        let api_key = api_key.as_ref();
        let parsed_sub_provider = parse_sub_provider(sub_provider);
        let model_qualifier = if base_url.is_none() {
            model_qualifier(&parsed_sub_provider)
        } else {
            None
        };
        let mut builder = huggingface::Client::builder()
            .api_key(api_key)
            .subprovider(parsed_sub_provider);
        if let Some(base_url) = base_url {
            builder = builder.base_url(base_url);
        }
        let hub_verify_client = match base_url {
            None => Some(build_hub_verify_client(api_key)?),
            Some(_) => None,
        };
        Ok(Self::HuggingFace {
            client: builder.build()?,
            model_qualifier,
            hub_verify_client,
        })
    }

    /// Qualifies `model` for the provider path this `Backend` was built against — a no-op for
    /// [`Backend::OpenAiCompatible`], and for [`Backend::HuggingFace`] whenever there's no
    /// `model_qualifier` to apply or `model` is already qualified (idempotency guard, so a
    /// caller-supplied override that's already `model:partner` isn't double-suffixed).
    pub fn qualify_model(&self, model: &str) -> String {
        match self {
            Self::HuggingFace {
                model_qualifier: Some(qualifier),
                ..
            } if !model.contains(':') => format!("{model}:{qualifier}"),
            _ => model.to_string(),
        }
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

    /// Builds the shared backend against Google's Gemini API — its native `generateContent`
    /// surface, not an OpenAI-compatible shim. `api_key` is the caller's `API_TOKEN`, same
    /// convention as every other `Backend` path (read from the environment at the CLI boundary,
    /// never here). Unlike [`Backend::huggingface`]'s default router path, this provider's
    /// `VERIFY_PATH` is a real model-listing endpoint on `generativelanguage.googleapis.com`, so
    /// [`Backend::verify`] needs no host-mismatch workaround for it.
    pub fn gemini(api_key: impl AsRef<str>) -> Result<Self, BackendError> {
        let client = gemini::Client::builder()
            .api_key(api_key.as_ref())
            .build()?;
        Ok(Self::Gemini(client))
    }

    /// Verifies the configured provider actually authenticates, regardless of path. On the
    /// default router path this goes through [`verify_huggingface_hub_token`] instead of Rig's
    /// own `client.verify()` — see that function for why. A `base_url` override takes the
    /// caller at their word about what's actually listening there, `client.verify()` included.
    pub async fn verify(&self) -> Result<(), rig::client::VerifyError> {
        let result = match self {
            Self::HuggingFace {
                hub_verify_client: Some(hub_client),
                ..
            } => {
                tracing::info!(
                    url = HF_HUB_WHOAMI_URL,
                    "verifying API token against the hub"
                );
                verify_huggingface_hub_token(hub_client).await
            }
            Self::HuggingFace { client, .. } => {
                tracing::info!("verifying API token against the overridden base URL");
                client.verify().await
            }
            Self::OpenAiCompatible(client) => {
                tracing::info!("verifying API token against the OpenAI-compatible endpoint");
                client.verify().await
            }
            Self::Gemini(client) => {
                tracing::info!("verifying API token against the Gemini API");
                client.verify().await
            }
        };
        if let Err(err) = &result {
            tracing::info!(error = %err, "API token verification failed");
        }
        result
    }
}

/// Builds the `reqwest::Client` [`verify_huggingface_hub_token`] sends its request through, with
/// the token pre-set as a (sensitive) `Authorization` header.
fn build_hub_verify_client(api_key: &str) -> Result<reqwest::Client, BackendError> {
    let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))?;
    auth.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, auth);
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .build()?)
}

/// Confirms `client`'s token authenticates against the Hub — deliberately *not* Rig's own
/// `huggingface::Client::verify()`. That built-in call sends `GET {base_url}/api/whoami-v2`,
/// and for the default router path (`base_url = https://router.huggingface.co`) that 404s
/// unconditionally: `/api/whoami-v2` is a Hub account-info route, and `router.huggingface.co`
/// only proxies inference endpoints (chat completions, etc.), not general Hub API — so every
/// call through that path fails before a single agent gets built, regardless of model, provider,
/// or token validity. Hitting [`HF_HUB_WHOAMI_URL`] directly sidesteps the mismatch.
async fn verify_huggingface_hub_token(
    client: &reqwest::Client,
) -> Result<(), rig::client::VerifyError> {
    use rig::client::VerifyError;
    use rig::http_client::Error as HttpClientError;

    let response = client
        .get(HF_HUB_WHOAMI_URL)
        .send()
        .await
        .map_err(|err| VerifyError::ProviderError(err.to_string()))?;

    match response.status() {
        reqwest::StatusCode::OK => Ok(()),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            Err(VerifyError::InvalidAuthentication)
        }
        status => {
            let text = response.text().await.unwrap_or_default();
            Err(VerifyError::HttpError(
                HttpClientError::InvalidStatusCodeWithMessage(status, text),
            ))
        }
    }
}

/// The router-slug suffix for `<model>:<suffix>` explicit provider pinning
/// (https://huggingface.co/docs/inference-providers), for every `SubProvider` Rig's own
/// `model_identifier` doesn't already handle. `HFInference` needs none — it's the router's
/// auto-selected default. `Fireworks` needs none *from us* — Rig's `model_identifier` already
/// rewrites the model id to Fireworks' own `accounts/fireworks/models/…` form, and stacking a
/// `:fireworks-ai` suffix on top of that would just be a second, redundant qualifier.
fn model_qualifier(sub_provider: &huggingface::SubProvider) -> Option<String> {
    use huggingface::SubProvider;

    match sub_provider {
        SubProvider::HFInference | SubProvider::Fireworks => None,
        SubProvider::Together => Some("together".to_string()),
        SubProvider::SambaNova => Some("sambanova".to_string()),
        SubProvider::Hyperbolic => Some("hyperbolic".to_string()),
        SubProvider::Nebius => Some("nebius".to_string()),
        SubProvider::Novita => Some("novita".to_string()),
        SubProvider::Custom(name) => Some(name.clone()),
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
    fn default_router_gets_a_hub_verify_client() {
        let backend = Backend::huggingface("dummy-key", None, None).unwrap();
        assert!(matches!(
            backend,
            Backend::HuggingFace {
                hub_verify_client: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn an_overridden_base_url_skips_the_hub_verify_client() {
        let backend =
            Backend::huggingface("dummy-key", None, Some("https://my-endpoint.example.com"))
                .unwrap();
        assert!(matches!(
            backend,
            Backend::HuggingFace {
                hub_verify_client: None,
                ..
            }
        ));
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

    #[test]
    fn qualifies_model_with_the_chosen_sub_provider() {
        let backend = Backend::huggingface("dummy-key", Some("together"), None).unwrap();
        assert_eq!(
            backend.qualify_model("Qwen/Qwen3-Coder-480B"),
            "Qwen/Qwen3-Coder-480B:together"
        );
    }

    #[test]
    fn does_not_qualify_the_default_hf_inference_partner() {
        let backend = Backend::huggingface("dummy-key", None, None).unwrap();
        assert_eq!(
            backend.qualify_model("Qwen/Qwen3-Coder-480B"),
            "Qwen/Qwen3-Coder-480B"
        );
    }

    #[test]
    fn does_not_double_qualify_an_already_qualified_model() {
        let backend = Backend::huggingface("dummy-key", Some("together"), None).unwrap();
        assert_eq!(
            backend.qualify_model("Qwen/Qwen3-Coder-480B:novita"),
            "Qwen/Qwen3-Coder-480B:novita"
        );
    }

    #[test]
    fn does_not_qualify_when_base_url_overrides_the_router() {
        let backend = Backend::huggingface(
            "dummy-key",
            Some("together"),
            Some("https://my-endpoint.example.com"),
        )
        .unwrap();
        assert_eq!(
            backend.qualify_model("Qwen/Qwen3-Coder-480B"),
            "Qwen/Qwen3-Coder-480B"
        );
    }

    #[test]
    fn does_not_qualify_the_openai_compatible_path() {
        let backend = Backend::openai_compatible("dummy-key", HF_ROUTER_OPENAI_BASE_URL).unwrap();
        assert_eq!(
            backend.qualify_model("Qwen/Qwen3-Coder-480B"),
            "Qwen/Qwen3-Coder-480B"
        );
    }

    #[test]
    fn builds_offline_against_gemini() {
        Backend::gemini("dummy-key").expect("client construction never contacts the network");
    }

    #[test]
    fn does_not_qualify_the_gemini_path() {
        let backend = Backend::gemini("dummy-key").unwrap();
        assert_eq!(
            backend.qualify_model("gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
    }
}
