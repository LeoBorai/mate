//! Provider error mapping and retry policy (§4, `M1-5`).
//!
//! Rig's capability errors (`CompletionError`, `VerifyError`, …) each expose the same
//! `provider_response_status()` / `provider_response_body()` helpers over a captured HTTP
//! status and raw body — generated per-type by rig's `impl_provider_response_helpers!`
//! macro, so there's no shared trait to match on directly. [`ProviderResponse`] is that
//! trait, implemented here for the two capability errors `mate-core` currently produces
//! (`M1-1`'s `Backend::verify`, and completion calls once `M2` lands); [`ProviderError`] is
//! what everything downstream — the retry loop, the UI — actually branches on.

use std::fmt;
use std::time::Duration;

use http::StatusCode;

/// The minimal surface `mate-core` needs from a Rig capability error to classify it.
pub trait ProviderResponse {
    /// HTTP status the provider actually returned, when the error preserves one. `None` for
    /// non-HTTP transports and for errors that carry no captured response.
    fn provider_response_status(&self) -> Option<StatusCode>;

    /// Whether this error is Rig's own "credentials rejected" signal rather than a raw HTTP
    /// status. [`rig::client::VerifyError::InvalidAuthentication`] is produced for a 401/403
    /// *before* any status is preserved (see rig's `VerifyClient::verify`), so status-code
    /// matching alone would miss it.
    fn is_invalid_authentication(&self) -> bool {
        false
    }
}

impl ProviderResponse for rig::completion::CompletionError {
    fn provider_response_status(&self) -> Option<StatusCode> {
        self.provider_response_status()
    }
}

impl ProviderResponse for rig::client::VerifyError {
    fn provider_response_status(&self) -> Option<StatusCode> {
        self.provider_response_status()
    }

    fn is_invalid_authentication(&self) -> bool {
        matches!(self, Self::InvalidAuthentication)
    }
}

/// A provider failure, classified for retry purposes (`M1-5`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// 429 — the account's quota is exhausted for now. Retryable.
    #[error("rate limited by the provider")]
    RateLimited,
    /// 503 — HF's "model is loading" cold-start response. Retryable.
    #[error("model is still warming up")]
    ModelWarming,
    /// 401/403, or Rig's own `InvalidAuthentication`. Never retryable — the credentials are
    /// wrong, not slow.
    #[error("authentication rejected by the provider")]
    AuthFailed,
    /// A 5xx other than the 503 warming case — transient server-side trouble. Retryable.
    #[error("provider server error (status {status})")]
    ServerError { status: u16 },
    /// Everything else: other 4xx client errors, transport/parse failures. Not retryable —
    /// retrying an identical malformed request just reproduces the same failure.
    #[error("provider request failed: {detail}")]
    Other { detail: String },
}

impl ProviderError {
    /// Classifies any Rig capability error into the retry taxonomy above.
    pub fn classify<E>(error: &E) -> Self
    where
        E: ProviderResponse + fmt::Display,
    {
        if error.is_invalid_authentication() {
            return Self::AuthFailed;
        }
        match error.provider_response_status() {
            Some(StatusCode::UNAUTHORIZED) | Some(StatusCode::FORBIDDEN) => Self::AuthFailed,
            Some(StatusCode::TOO_MANY_REQUESTS) => Self::RateLimited,
            Some(StatusCode::SERVICE_UNAVAILABLE) => Self::ModelWarming,
            Some(status) if status.is_server_error() => Self::ServerError {
                status: status.as_u16(),
            },
            _ => Self::Other {
                detail: error.to_string(),
            },
        }
    }

    /// Whether this class of failure is worth retrying at all.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::ModelWarming | Self::ServerError { .. }
        )
    }
}

/// Jittered exponential backoff (`M1-5`) for [`ProviderError`]'s retryable classes, with a
/// hard cap so a persistently failing provider doesn't retry forever.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    pub base_delay: Duration,
    pub factor: f64,
    pub max_delay: Duration,
    pub max_retries: usize,
}

impl Default for RetryPolicy {
    /// 500ms doubling up to 30s, five retries — generous enough to ride out a 429 window or
    /// an HF cold start without hammering the router.
    fn default() -> Self {
        Self {
            base_delay: Duration::from_millis(500),
            factor: 2.0,
            max_delay: Duration::from_secs(30),
            max_retries: 5,
        }
    }
}

impl RetryPolicy {
    /// The delay before retry attempt `attempt` (1 = the first retry, made after the
    /// initial call already failed), or `None` if `error` isn't retryable at all, or
    /// `attempt` exceeds `max_retries` — either way, the caller must stop.
    ///
    /// Full jitter — a uniform draw over `[0, backoff]`, per the AWS backoff-and-jitter
    /// writeup — rather than a fixed exponential delay: with several sessions and
    /// subagents retrying the same rate limit concurrently (§5.3), a fixed schedule has
    /// them all retry in lockstep and re-trip the limit together.
    pub fn backoff(&self, error: &ProviderError, attempt: usize) -> Option<Duration> {
        if !error.is_retryable() || attempt == 0 || attempt > self.max_retries {
            return None;
        }
        let exponent = (attempt - 1) as i32;
        let ceiling = self
            .base_delay
            .mul_f64(self.factor.powi(exponent))
            .min(self.max_delay);
        let jittered_ms = fastrand::u64(0..=ceiling.as_millis() as u64);
        Some(Duration::from_millis(jittered_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::client::VerifyError;
    use rig::http_client::Error as HttpClientError;

    fn http_error(status: StatusCode) -> VerifyError {
        VerifyError::HttpError(HttpClientError::InvalidStatusCodeWithMessage(
            status,
            "boom".to_string(),
        ))
    }

    #[test]
    fn classifies_429_as_rate_limited_and_retryable() {
        let error = ProviderError::classify(&http_error(StatusCode::TOO_MANY_REQUESTS));
        assert_eq!(error, ProviderError::RateLimited);
        assert!(error.is_retryable());
    }

    #[test]
    fn classifies_503_as_model_warming_and_retryable() {
        let error = ProviderError::classify(&http_error(StatusCode::SERVICE_UNAVAILABLE));
        assert_eq!(error, ProviderError::ModelWarming);
        assert!(error.is_retryable());
    }

    #[test]
    fn classifies_other_5xx_as_server_error_and_retryable() {
        let error = ProviderError::classify(&http_error(StatusCode::INTERNAL_SERVER_ERROR));
        assert_eq!(error, ProviderError::ServerError { status: 500 });
        assert!(error.is_retryable());
    }

    #[test]
    fn classifies_401_via_status_as_auth_failed_and_not_retryable() {
        let error = ProviderError::classify(&http_error(StatusCode::UNAUTHORIZED));
        assert_eq!(error, ProviderError::AuthFailed);
        assert!(!error.is_retryable());
    }

    #[test]
    fn classifies_rigs_own_invalid_authentication_as_auth_failed() {
        // The path rig's `VerifyClient::verify` actually takes for a 401/403: no status is
        // preserved at all, just this variant (see rig's `client::mod::verify`).
        let error = ProviderError::classify(&VerifyError::InvalidAuthentication);
        assert_eq!(error, ProviderError::AuthFailed);
        assert!(!error.is_retryable());
    }

    #[test]
    fn classifies_other_4xx_as_other_and_not_retryable() {
        let error = ProviderError::classify(&http_error(StatusCode::BAD_REQUEST));
        assert!(matches!(error, ProviderError::Other { .. }));
        assert!(!error.is_retryable());
    }

    #[test]
    fn a_401_never_retries_regardless_of_attempt_number() {
        let policy = RetryPolicy::default();
        let error = ProviderError::classify(&VerifyError::InvalidAuthentication);
        for attempt in 1..=10 {
            assert_eq!(policy.backoff(&error, attempt), None);
        }
    }

    #[test]
    fn retryable_errors_back_off_within_the_exponential_ceiling() {
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(100),
            factor: 2.0,
            max_delay: Duration::from_secs(10),
            max_retries: 3,
        };
        let error = ProviderError::RateLimited;

        let ceilings = [
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
        ];
        for (attempt, ceiling) in (1..=3).zip(ceilings) {
            let delay = policy
                .backoff(&error, attempt)
                .unwrap_or_else(|| panic!("attempt {attempt} should still retry"));
            assert!(
                delay <= ceiling,
                "attempt {attempt}: {delay:?} > {ceiling:?}"
            );
        }
    }

    #[test]
    fn hits_a_hard_cap_on_retries() {
        let policy = RetryPolicy {
            max_retries: 2,
            ..RetryPolicy::default()
        };
        let error = ProviderError::RateLimited;

        assert!(policy.backoff(&error, 1).is_some());
        assert!(policy.backoff(&error, 2).is_some());
        assert_eq!(policy.backoff(&error, 3), None);
    }

    #[test]
    fn backoff_ceiling_never_exceeds_max_delay() {
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(500),
            factor: 2.0,
            max_delay: Duration::from_secs(1),
            max_retries: 10,
        };
        let error = ProviderError::ModelWarming;

        for attempt in 1..=10 {
            let delay = policy.backoff(&error, attempt).expect("still retryable");
            assert!(
                delay <= Duration::from_secs(1),
                "attempt {attempt}: {delay:?}"
            );
        }
    }
}
