//! The `http_request` tool itself (§7.2-shaped, §8.2, `M10-3`/`M10-5`/`M10-7`): orchestrates
//! method gating, header hygiene, the manual redirect loop, the streamed size cap, and body
//! rendering into the one `Tool` impl the agent actually calls.
//!
//! **Method gating (`M10-7`).** Only `GET`/`HEAD` run unattended; everything else is refused
//! outright. §8.2 describes routing mutating methods through approval instead, but no
//! approval channel exists yet (`ToolCtx::approvals` is deferred to `M13` — see that type's
//! doc comment) — so for now "no approval channel" and "always refuse" are the same thing.
//!
//! **The manual redirect loop (`M10-3`).** `HttpShared::pinned_client` is built with
//! `redirect::Policy::none()` specifically so this loop can re-run the *whole* validation
//! pipeline — scheme, DNS resolution, IP-range check — on every hop's URL, not just the
//! first. A public host redirecting to `169.254.169.254` fails at
//! [`HttpShared::resolve_validated`] the moment this loop tries to follow that hop, before
//! any connection to it is attempted. Hitting `max_redirects` stops following further hops;
//! the loop returns whatever response it has (typically still a redirect) rather than erroring,
//! with the hop count reported in the output so the model can see it didn't reach a final page.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use mate_tool_api::{ToolCtx, ToolFailure, truncate_with_notice};
use rig::tool::{PortableTool, ToolExecutionError};
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

use crate::headers::validate_request_headers;
use crate::render;
use crate::shared::HttpShared;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HttpRequestArgs {
    /// The URL to fetch. Only the http and https schemes are allowed.
    pub url: String,
    /// HTTP method. Only "GET" and "HEAD" are permitted — mutating methods are always
    /// refused, since there is no approval flow yet to route them through. Defaults to GET.
    #[serde(default)]
    pub method: Option<String>,
    /// Extra request headers. Authorization, Cookie, and Proxy-Authorization are refused —
    /// credentials come from mate's own configuration, never from the model.
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    /// Render the body for reading: HTML is extracted to plain text via readability, JSON is
    /// pretty-printed. Set to false to get the raw decoded body instead. Defaults to true.
    #[serde(default)]
    pub render_text: Option<bool>,
}

/// Outbound network access for the agent, hardened against SSRF (§8.2). One instance is built
/// per agent (root or subagent) in `mate-core::toolset::build_toolset`, narrowed the same way
/// every other tool is: `allow_localhost` mirrors `HttpAccessPolicy::AllowLocalhost`, and
/// `shared` is the same process-wide [`HttpShared`] every agent in the process holds an `Arc`
/// to (§5.3).
pub struct HttpRequest {
    ctx: ToolCtx,
    shared: Arc<HttpShared>,
    allow_localhost: bool,
}

impl HttpRequest {
    pub fn new(ctx: ToolCtx, shared: Arc<HttpShared>, allow_localhost: bool) -> Self {
        Self {
            ctx,
            shared,
            allow_localhost,
        }
    }
}

impl PortableTool for HttpRequest {
    const NAME: &'static str = "http_request";
    type Args = HttpRequestArgs;
    type Output = String;
    type Error = ToolFailure;

    fn description(&self) -> String {
        "Fetch a URL over HTTP or HTTPS. Only GET and HEAD are supported; mutating methods \
         are refused. Requests to private, loopback, link-local, and other non-public \
         addresses are blocked. HTML responses are converted to readable text and JSON is \
         pretty-printed by default — set render_text to false for the raw body. Output leads \
         with the status, final URL (after any redirects), content type, and redirect count."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        schemars::schema_for!(HttpRequestArgs).to_value()
    }

    fn map_error(&self, error: ToolFailure) -> ToolExecutionError {
        error.into()
    }

    async fn call(&self, args: HttpRequestArgs) -> Result<String, ToolFailure> {
        let method = parse_method(args.method.as_deref())?;
        let header_map = validate_request_headers(&args.headers.unwrap_or_default())?;
        let render_text = args.render_text.unwrap_or(true);

        let start_url = Url::parse(&args.url)
            .map_err(|err| ToolFailure::InvalidArgs(format!("invalid url: {err}")))?;

        let limits = self.shared.limits();
        let mut current_url = start_url;
        let mut hops: u8 = 0;

        let (mut response, hops) = loop {
            validate_scheme(&current_url)?;
            let host = current_url
                .host_str()
                .ok_or_else(|| ToolFailure::InvalidArgs("url has no host".to_string()))?
                .to_string();
            let port = current_url.port_or_known_default().ok_or_else(|| {
                ToolFailure::InvalidArgs("url has no resolvable port".to_string())
            })?;

            let ip = self
                .shared
                .resolve_validated(&host, self.allow_localhost)
                .await?;
            self.shared.throttle(&host).await;
            let client = self
                .shared
                .pinned_client(&host, SocketAddr::new(ip, port))?;

            let response = client
                .request(method.clone(), current_url.clone())
                .headers(header_map.clone())
                .send()
                .await
                .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?;

            let location = response
                .status()
                .is_redirection()
                .then(|| response.headers().get(reqwest::header::LOCATION).cloned())
                .flatten();

            match location {
                Some(location) if hops < limits.max_redirects => {
                    let location = location.to_str().map_err(|_| {
                        ToolFailure::Other(anyhow::anyhow!("invalid Location header"))
                    })?;
                    current_url = current_url.join(location).map_err(|err| {
                        ToolFailure::Other(anyhow::anyhow!("invalid redirect target: {err}"))
                    })?;
                    hops += 1;
                }
                _ => break (response, hops),
            }
        };

        let status = response.status();
        let final_url = response.url().clone();
        // A missing header is common on empty bodies (redirects that hit the hop cap, HEAD
        // responses) and isn't the server naming a binary type — default to something
        // renderable, not `application/octet-stream`, or every such response gets refused for
        // a type the server never actually declared.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_string();

        if !render::is_renderable(&content_type) {
            return Err(ToolFailure::Denied(format!(
                "refused content type: {content_type}"
            )));
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > limits.max_response_bytes {
                return Err(ToolFailure::TooLarge {
                    limit: limits.max_response_bytes,
                });
            }
        }

        let rendered = render::render_body(&content_type, &body, &final_url, render_text)?;
        let truncated = truncate_with_notice(&rendered, self.ctx.max_output_bytes);

        Ok(format!(
            "{} {}\ncontent-type: {content_type}\nredirects: {hops}\n\n{truncated}",
            status.as_u16(),
            final_url,
        ))
    }
}

fn validate_scheme(url: &Url) -> Result<(), ToolFailure> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(ToolFailure::Denied(format!(
            "scheme not permitted: {other} (only http and https are allowed)"
        ))),
    }
}

/// Method gating (`M10-7`): only unattended-safe methods run. `None` (the argument omitted)
/// defaults to GET.
fn parse_method(method: Option<&str>) -> Result<reqwest::Method, ToolFailure> {
    match method.unwrap_or("GET").to_ascii_uppercase().as_str() {
        "GET" => Ok(reqwest::Method::GET),
        "HEAD" => Ok(reqwest::Method::HEAD),
        other => Err(ToolFailure::Denied(format!(
            "method not permitted: {other} (only GET and HEAD run without an approval flow, \
             which does not exist yet)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_and_head_are_permitted() {
        assert!(matches!(parse_method(Some("GET")), Ok(m) if m == reqwest::Method::GET));
        assert!(matches!(parse_method(Some("head")), Ok(m) if m == reqwest::Method::HEAD));
        assert!(matches!(parse_method(None), Ok(m) if m == reqwest::Method::GET));
    }

    #[test]
    fn post_is_refused_outright_by_default() {
        let err = parse_method(Some("POST")).unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[test]
    fn validate_scheme_allows_http_and_https_only() {
        assert!(validate_scheme(&Url::parse("https://example.com").unwrap()).is_ok());
        assert!(validate_scheme(&Url::parse("http://example.com").unwrap()).is_ok());
        assert!(validate_scheme(&Url::parse("file:///etc/passwd").unwrap()).is_err());
        assert!(validate_scheme(&Url::parse("ftp://example.com").unwrap()).is_err());
    }

    #[tokio::test]
    async fn schema_carries_field_descriptions() {
        let (activity, _rx) = tokio::sync::mpsc::channel(8);
        let ctx = ToolCtx {
            agent: mate_tool_api::AgentId::ROOT,
            root: std::env::temp_dir(),
            max_output_bytes: 1_000_000,
            spawner: None,
            activity,
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let shared = Arc::new(HttpShared::new(60).unwrap());
        let tool = HttpRequest::new(ctx, shared, false);
        let schema = tool.parameters();
        let description = schema["properties"]["url"]["description"].as_str().unwrap();
        assert!(description.contains("http"));
    }
}
