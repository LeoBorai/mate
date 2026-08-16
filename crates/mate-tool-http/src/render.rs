//! Body rendering (§8.2, `M10-6`): turns a fetched response body plus its content type into
//! text a model can read. HTML goes through `readability` (strip nav/ads/boilerplate down to
//! the article) then `html2text` (HTML → wrapped plain text); JSON is pretty-printed and
//! depth-capped so a deeply nested API response doesn't blow the output budget on brackets;
//! everything else `text/*`-shaped passes through as-is. `render_text: false` (the args
//! escape hatch) skips all of that and hands back the raw decoded bytes regardless of type —
//! for when the model needs the literal markup, not the human-readable version.

use mate_tool_api::ToolFailure;
use url::Url;

/// Column width `html2text` wraps rendered HTML to. Fixed, not configurable — matching every
/// other limit in this crate (§10's `[http]` table exposes none of these).
const WRAP_WIDTH: usize = 100;

/// How many levels of JSON nesting survive pretty-printing before collapsing to a placeholder
/// (§8.2's "JSON pretty-printed and depth-capped").
const JSON_MAX_DEPTH: usize = 6;

/// Content types the tool will render at all (§8.2 point 7: "non-text content types refused
/// by name"). Checked *before* a body is even downloaded in the caller, so a binary response
/// never reaches this module.
pub fn is_renderable(content_type: &str) -> bool {
    let base = content_type.split(';').next().unwrap_or("").trim();
    base.starts_with("text/")
        || matches!(
            base,
            "application/json" | "application/xml" | "application/xhtml+xml"
        )
}

/// Renders `bytes` (already known to be within the size cap and a renderable content type)
/// per the content-type dispatch described in the module doc. `render_text: false` bypasses
/// all of it.
pub fn render_body(
    content_type: &str,
    bytes: &[u8],
    url: &Url,
    render_text: bool,
) -> Result<String, ToolFailure> {
    if !render_text {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }

    let base = content_type.split(';').next().unwrap_or("").trim();
    match base {
        "text/html" | "application/xhtml+xml" => Ok(html_to_text(bytes, url)),
        "application/json" => json_pretty_capped(bytes),
        _ => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// Extracts the article body with `readability`, then wraps it to text with `html2text`.
/// Falls back to running `html2text` over the raw HTML directly if extraction fails — a page
/// readability can't parse (a fragment, a non-article page) still deserves *some* text output
/// rather than a hard failure.
fn html_to_text(bytes: &[u8], url: &Url) -> String {
    let mut cursor = std::io::Cursor::new(bytes);
    match readability::extractor::extract(&mut cursor, url) {
        Ok(product) => {
            html2text::from_read(product.content.as_bytes(), WRAP_WIDTH).unwrap_or(product.text)
        }
        Err(_) => html2text::from_read(bytes, WRAP_WIDTH)
            .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned()),
    }
}

fn json_pretty_capped(bytes: &[u8]) -> Result<String, ToolFailure> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|err| ToolFailure::Other(anyhow::anyhow!("invalid JSON body: {err}")))?;
    let capped = cap_depth(&value, JSON_MAX_DEPTH);
    serde_json::to_string_pretty(&capped)
        .map_err(|err| ToolFailure::Other(anyhow::anyhow!("failed to render JSON: {err}")))
}

fn cap_depth(value: &serde_json::Value, remaining: usize) -> serde_json::Value {
    use serde_json::Value;
    if remaining == 0 {
        return match value {
            Value::Object(_) | Value::Array(_) => {
                Value::String("… (max depth reached)".to_string())
            }
            other => other.clone(),
        };
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_depth(v, remaining - 1)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| cap_depth(v, remaining - 1)).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://example.com/article").unwrap()
    }

    #[test]
    fn is_renderable_accepts_text_and_known_structured_types() {
        for ct in [
            "text/html",
            "text/plain; charset=utf-8",
            "application/json",
            "application/xml",
            "application/xhtml+xml",
        ] {
            assert!(is_renderable(ct), "{ct} should be renderable");
        }
    }

    #[test]
    fn is_renderable_refuses_binary_types_by_name() {
        for ct in ["image/png", "application/octet-stream", "application/pdf"] {
            assert!(!is_renderable(ct), "{ct} should be refused");
        }
    }

    #[test]
    fn render_text_false_returns_the_raw_body_regardless_of_type() {
        let html = b"<html><body><p>hi</p></body></html>";
        let out = render_body("text/html", html, &url(), false).unwrap();
        assert_eq!(out, "<html><body><p>hi</p></body></html>");
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        let out = render_body("text/plain", b"hello world", &url(), true).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn html_is_rendered_to_wrapped_text_via_readability_and_html2text() {
        let html = b"<html><body><article><p>hello from the article body, \
                      long enough that word wrap could plausibly matter here</p></article>\
                      </body></html>";
        let out = render_body("text/html", html, &url(), true).unwrap();
        assert!(out.to_lowercase().contains("hello from the article body"));
        assert!(!out.contains("<p>"), "output must be text, not raw HTML");
    }

    #[test]
    fn json_is_pretty_printed() {
        let out = render_body("application/json", br#"{"a":1,"b":[1,2,3]}"#, &url(), true).unwrap();
        assert!(
            out.contains('\n'),
            "pretty-printed JSON should be multi-line"
        );
        assert!(out.contains("\"a\": 1"));
    }

    #[test]
    fn invalid_json_is_reported_rather_than_panicking() {
        let err = render_body("application/json", b"{not json", &url(), true).unwrap_err();
        assert!(matches!(err, ToolFailure::Other(_)));
    }

    #[test]
    fn json_depth_beyond_the_cap_collapses_to_a_placeholder() {
        let mut value = serde_json::json!("leaf");
        for _ in 0..(JSON_MAX_DEPTH + 3) {
            value = serde_json::json!({ "nested": value });
        }
        let capped = cap_depth(&value, JSON_MAX_DEPTH);
        let rendered = serde_json::to_string(&capped).unwrap();
        assert!(rendered.contains("max depth reached"));
    }
}
