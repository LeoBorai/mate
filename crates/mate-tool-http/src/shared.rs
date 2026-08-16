//! [`HttpShared`] (§5.3, §8.2, `M10-1`/`M10-2`/`M10-8`): the process-wide state every
//! `http_request` tool instance — root or subagent, any session — shares. Built once by
//! `mate-core` and handed down as an `Arc`, the same way `Backend` is (§5.3's table): one
//! DNS resolver, one per-host rate limiter map, one set of network limits.
//!
//! **Why the rate limiter must live here and not on the tool.** Four tabs times three
//! subagents is twelve agents that could each hold their own limiter and hammer one host at
//! twelve times the configured rate if the limiter were per-tool-instance instead of
//! process-wide (§5.3).
//!
//! **Why DNS resolution and IP pinning live here too.** [`HttpShared::resolve_validated`]
//! resolves a host with `hickory-resolver` and validates every candidate address against
//! [`crate::ip_guard`] before returning one. The caller then builds a request client via
//! [`HttpShared::pinned_client`], which pins that exact validated address into the
//! connection with `reqwest::ClientBuilder::resolve` — so the socket that actually opens is
//! the one we checked, not whatever a second DNS lookup might return a moment later (a TOCTOU
//! hole: validating an address and then letting `reqwest` re-resolve independently would let
//! DNS rebinding slip straight past the check).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use dashmap::DashMap;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use hickory_resolver::{Resolver, TokioResolver};
use mate_tool_api::ToolFailure;

use crate::ip_guard::blocked_reason;
use crate::limits::HttpLimits;

/// Identifies this tool to whatever it talks to (§8.2 point 1's "forced user-agent" — never
/// left at reqwest's default, and never something the model can override).
fn user_agent() -> String {
    format!("mate-http-tool/{}", env!("CARGO_PKG_VERSION"))
}

pub struct HttpShared {
    resolver: TokioResolver,
    limits: HttpLimits,
    quota: Quota,
    /// One [`DefaultDirectRateLimiter`] per host, created lazily on first use and reused for
    /// the life of the process — the mechanism behind "two sessions hitting one host share
    /// the budget" (`M10-8`'s acceptance criterion).
    hosts: DashMap<String, Arc<DefaultDirectRateLimiter>>,
}

impl HttpShared {
    /// Builds the shared state with the default network limits (§8.2). `rate_limit_per_host_per_min`
    /// comes from `[http].rate_limit_per_host_per_min` (§10), process-wide regardless of how
    /// many sessions or subagents end up sharing this instance.
    pub fn new(rate_limit_per_host_per_min: u32) -> Result<Self, ToolFailure> {
        Self::with_limits(rate_limit_per_host_per_min, HttpLimits::default())
    }

    /// As [`Self::new`], with explicit [`HttpLimits`] — the seam tests use to shrink
    /// `max_response_bytes` so a "response too large" test doesn't need to actually transfer
    /// megabytes of wiremock fixture data.
    pub fn with_limits(
        rate_limit_per_host_per_min: u32,
        limits: HttpLimits,
    ) -> Result<Self, ToolFailure> {
        let resolver = Resolver::builder_tokio()
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?
            .build()
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?;
        let rate = std::num::NonZeroU32::new(rate_limit_per_host_per_min)
            .unwrap_or(std::num::NonZeroU32::MIN);
        Ok(Self {
            resolver,
            limits,
            quota: Quota::per_minute(rate),
            hosts: DashMap::new(),
        })
    }

    pub fn limits(&self) -> HttpLimits {
        self.limits
    }

    /// Resolves `host` and returns the first candidate address that passes
    /// [`blocked_reason`] (§8.2 point 2), or an error naming why every candidate was refused.
    /// A literal IP in `host` (no DNS involved) is validated directly.
    ///
    /// Deliberately doesn't require *every* resolved address to be safe — only the one
    /// address actually chosen and pinned matters, since [`Self::pinned_client`] ensures the
    /// connection only ever reaches that one address regardless of what else the name
    /// resolved to.
    pub async fn resolve_validated(
        &self,
        host: &str,
        allow_localhost: bool,
    ) -> Result<IpAddr, ToolFailure> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return match blocked_reason(ip, allow_localhost) {
                Some(reason) => Err(ToolFailure::Denied(format!(
                    "blocked address {ip} ({reason})"
                ))),
                None => Ok(ip),
            };
        }

        let response = self
            .resolver
            .lookup_ip(host)
            .await
            .map_err(|err| ToolFailure::NotFound(format!("{host}: {err}")))?;

        let mut first_blocked: Option<(IpAddr, &'static str)> = None;
        for ip in response.iter() {
            match blocked_reason(ip, allow_localhost) {
                None => return Ok(ip),
                Some(reason) => {
                    first_blocked.get_or_insert((ip, reason));
                }
            }
        }

        match first_blocked {
            Some((ip, reason)) => Err(ToolFailure::Denied(format!(
                "blocked address {ip} ({reason})"
            ))),
            None => Err(ToolFailure::NotFound(format!("no addresses for {host}"))),
        }
    }

    /// Waits until `host`'s per-host quota (§5.3, process-wide) allows another request,
    /// creating that host's limiter on first use.
    pub async fn throttle(&self, host: &str) {
        let limiter = self
            .hosts
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(RateLimiter::direct(self.quota)))
            .clone();
        limiter.until_ready().await;
    }

    /// Builds a one-off `reqwest::Client` with `addr` pinned as `host`'s resolution (`M10-2`):
    /// redirects never auto-follow (the manual loop in `http_request.rs` handles them so each
    /// hop can be re-validated), a forced user-agent, and the configured timeouts. Built fresh
    /// per hop rather than pooled — an `http_request` call is at most
    /// `1 + max_redirects` hosts, not a hot path worth pooling connections across.
    pub fn pinned_client(
        &self,
        host: &str,
        addr: SocketAddr,
    ) -> Result<reqwest::Client, ToolFailure> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(self.limits.connect_timeout)
            .timeout(self.limits.total_timeout)
            .user_agent(user_agent())
            .resolve(host, addr)
            .build()
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_zero_rate_limit_clamps_to_the_minimum_quota_rather_than_panicking() {
        // Quota::per_minute needs a NonZeroU32; a misconfigured 0 must not panic the whole
        // tool at construction. Built inside a runtime like every other construction here —
        // `Resolver::builder_tokio` is a Tokio-backed resolver, not guaranteed safe to build
        // outside one.
        let shared = HttpShared::new(0);
        assert!(shared.is_ok());
    }

    #[tokio::test]
    async fn resolve_validated_accepts_a_safe_literal_ip_without_dns() {
        let shared = HttpShared::new(60).unwrap();
        let ip = shared
            .resolve_validated("93.184.216.34", false)
            .await
            .unwrap();
        assert_eq!(ip, "93.184.216.34".parse::<IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn resolve_validated_denies_a_blocked_literal_ip() {
        let shared = HttpShared::new(60).unwrap();
        let err = shared
            .resolve_validated("169.254.169.254", false)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[tokio::test]
    async fn resolve_validated_allows_loopback_only_with_the_flag_set() {
        let shared = HttpShared::new(60).unwrap();
        assert!(shared.resolve_validated("127.0.0.1", false).await.is_err());
        assert!(shared.resolve_validated("127.0.0.1", true).await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn throttle_shares_one_limiter_across_concurrent_callers_for_the_same_host() {
        // A quota of 1/min means a second call for the same host must wait — proving the
        // limiter is shared (looked up by host), not a fresh one per call.
        let shared = Arc::new(HttpShared::new(1).unwrap());
        shared.throttle("example.com").await;

        let waited = {
            let shared = shared.clone();
            tokio::time::timeout(std::time::Duration::from_millis(50), async move {
                shared.throttle("example.com").await;
            })
            .await
        };
        assert!(
            waited.is_err(),
            "a second request to the same host within the same minute must be throttled"
        );
    }
}
