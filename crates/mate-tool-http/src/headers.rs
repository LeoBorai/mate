//! Request header hygiene (§8.2 point 5, `M10-4`): the `http_request` tool lets a model set
//! arbitrary extra headers, but two categories never reach the wire as the model wrote them.
//!
//! - **Credential headers are refused outright**, not stripped silently — `Authorization`,
//!   `Cookie`, and `Proxy-Authorization` come back as a named [`ToolFailure::Denied`] so the
//!   model gets a reason it can act on (`M10-4`'s acceptance criterion), rather than a
//!   confusing response that quietly lacks the auth it asked for.
//! - **Hop-by-hop / connection-management headers are dropped without error.** Setting
//!   `Connection: keep-alive` or a stray `Host` isn't an attack, just a confused model —
//!   `reqwest` and the connection layer own these, so they're silently ignored instead of
//!   failing the call.

use mate_tool_api::ToolFailure;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// Refused outright: sending credentials the model supplied itself, rather than ones
/// configured by the operator, is exactly the SSRF-adjacent risk §8.2 calls out — a prompt
/// injection that convinces the model to attach a bearer token to a request toward an
/// attacker-controlled host.
const DENIED_HEADERS: &[&str] = &["authorization", "cookie", "proxy-authorization"];

/// Owned by the HTTP layer, not the model: connection lifecycle, framing, and the `Host`
/// header (which must match whatever `reqwest` actually connects to, not a model's guess).
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authenticate",
    "host",
    "content-length",
];

/// Validates and converts model-supplied headers into a [`HeaderMap`] ready to attach to a
/// request. Errs on the first denied header name; silently omits hop-by-hop names; errs with
/// [`ToolFailure::InvalidArgs`] on a name/value that isn't valid HTTP header syntax.
pub fn validate_request_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<HeaderMap, ToolFailure> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if DENIED_HEADERS.contains(&lower.as_str()) {
            return Err(ToolFailure::Denied(format!(
                "header not permitted: {name} (credentials must come from mate's own \
                 configuration, never from the model)"
            )));
        }
        if HOP_BY_HOP_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ToolFailure::InvalidArgs(format!("invalid header name: {name}")))?;
        let header_value = HeaderValue::from_str(value)
            .map_err(|_| ToolFailure::InvalidArgs(format!("invalid header value for {name}")))?;
        map.insert(header_name, header_value);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn an_ordinary_header_passes_through() {
        let map = validate_request_headers(&headers(&[("Accept", "application/json")])).unwrap();
        assert_eq!(map.get("accept").unwrap(), "application/json");
    }

    #[test]
    fn a_model_supplied_auth_header_is_denied_with_a_named_reason() {
        let err = validate_request_headers(&headers(&[("Authorization", "Bearer x")])).unwrap_err();
        match err {
            ToolFailure::Denied(reason) => assert!(reason.contains("Authorization")),
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn denial_is_case_insensitive() {
        let err = validate_request_headers(&headers(&[("cOOkie", "a=b")])).unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[test]
    fn proxy_authorization_is_denied() {
        let err =
            validate_request_headers(&headers(&[("Proxy-Authorization", "Basic x")])).unwrap_err();
        assert!(matches!(err, ToolFailure::Denied(_)));
    }

    #[test]
    fn hop_by_hop_headers_are_silently_dropped_not_errored() {
        let map = validate_request_headers(&headers(&[
            ("Connection", "keep-alive"),
            ("Host", "evil.example"),
            ("Accept", "text/plain"),
        ]))
        .unwrap();
        assert_eq!(map.len(), 1, "only the ordinary header should survive");
        assert!(map.get("accept").is_some());
    }

    #[test]
    fn an_invalid_header_value_is_reported_as_invalid_args() {
        let err = validate_request_headers(&headers(&[("X-Test", "bad\nvalue")])).unwrap_err();
        assert!(matches!(err, ToolFailure::InvalidArgs(_)));
    }
}
