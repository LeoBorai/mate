//! Fixed network limits (§8.2) for the `http_request` tool: connect/total timeouts, the
//! streamed-response byte cap, and the redirect hop cap. Not exposed through `[http]` config
//! (§10's table lists only `enabled`/`policy`/`rate_limit_per_host_per_min`) — these are the
//! same for every agent, so a struct with a real `Default` beats a config knob nobody asked
//! for. Kept as a struct rather than bare constants purely so tests can shrink
//! `max_response_bytes` without downloading megabytes of wiremock fixture data.
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpLimits {
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    /// Streamed response cap (§8.2 point 7). Exceeding it aborts the download mid-stream —
    /// the tool never buffers the whole body first.
    pub max_response_bytes: usize,
    /// Redirect hop cap (§8.2 point 4). Reaching it stops following further hops; the last
    /// response (still a redirect) is returned as-is, with the hop count reported.
    pub max_redirects: u8,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_secs(30),
            max_response_bytes: 2 * 1024 * 1024,
            max_redirects: 5,
        }
    }
}
