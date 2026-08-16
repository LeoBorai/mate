//! The `http_request` tool: outbound network access for the agent, hardened
//! against SSRF — scheme allowlist, resolved-IP validation, DNS-rebinding
//! defense, manual redirect handling, header stripping, and response caps.
//!
//! - [`ip_guard`] — the pure IP-range checks (`M10-2`).
//! - [`limits`] — fixed timeouts, response cap, redirect cap (`M10-1`).
//! - [`shared`] — [`HttpShared`]: the process-wide resolver, per-host rate limiter, and pinned
//!   client factory every agent's tool instance shares (`M10-1`/`M10-2`/`M10-8`).
//! - [`headers`] — request header hygiene (`M10-4`).
//! - [`render`] — body rendering by content type (`M10-6`).
//! - [`http_request`] — [`HttpRequest`], the `Tool` impl tying the above together
//!   (`M10-3`/`M10-5`/`M10-7`).

mod headers;
mod http_request;
mod ip_guard;
mod limits;
mod render;
mod shared;

pub use http_request::{HttpRequest, HttpRequestArgs};
pub use limits::HttpLimits;
pub use shared::HttpShared;
