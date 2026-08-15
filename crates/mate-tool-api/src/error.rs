//! `ToolFailure` (§6.1) and the small text utilities every `mate-tool-*` crate needs to
//! shape output for a model rather than a human: line numbering, byte-capped truncation,
//! and the size/binary refusal checks a file-reading tool runs before it touches content
//! (`M3-6`).

use std::time::Duration;

use rig::tool::{ToolErrorKind, ToolExecutionError};

/// Why a tool call didn't produce a result.
///
/// Deliberately not fatal: Rig feeds a tool's `Err` back to the model as a tool result,
/// so every variant's [`std::fmt::Display`] is written as a recovery instruction for the
/// model to read, not an operator-facing diagnostic. [`From<ToolFailure> for
/// ToolExecutionError`] carries that same string through as the model-visible feedback
/// rather than falling back to Rig's redacted kind-level text.
#[derive(Debug, thiserror::Error)]
pub enum ToolFailure {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("denied: {0}")]
    Denied(String),
    #[error("invalid argument: {0}")]
    InvalidArgs(String),
    #[error("too large (limit {limit} bytes)")]
    TooLarge { limit: usize },
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("cancelled by user")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<ToolFailure> for ToolExecutionError {
    fn from(error: ToolFailure) -> Self {
        let kind = match &error {
            ToolFailure::NotFound(_) => ToolErrorKind::NotFound,
            ToolFailure::Denied(_) => ToolErrorKind::PermissionDenied,
            ToolFailure::InvalidArgs(_) => ToolErrorKind::InvalidArgs,
            ToolFailure::TooLarge { .. } => ToolErrorKind::Other,
            ToolFailure::Timeout(_) => ToolErrorKind::Timeout,
            ToolFailure::Cancelled => ToolErrorKind::Cancelled,
            ToolFailure::Other(_) => ToolErrorKind::Other,
        };
        // `ToolExecutionError::new` makes `message` the model-visible feedback too —
        // exactly what a `ToolFailure`'s `Display` is written for.
        ToolExecutionError::new(kind, error.to_string())
    }
}

/// Prefixes each line of `text` with its 1-based file line number, `start` being the
/// number of the first line in `text` — a `read_file` slice starting at line 40 numbers
/// from 40, not 1.
pub fn number_lines(text: &str, start: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(offset, line)| format!("{:>6}\t{line}", start + offset))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Truncates `text` to at most `max_bytes` bytes (never splitting a UTF-8 character) and
/// appends a notice naming how much was dropped. Returns the text unchanged, with no
/// notice, when it already fits.
pub fn truncate_with_notice(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    format!(
        "{}\n... [truncated: showing {end} of {} bytes]",
        &text[..end],
        text.len()
    )
}

/// Refuses content whose size exceeds `max_output_bytes` (`M3-6`) rather than silently
/// truncating it — a partial binary or a half-read huge file serves the model nothing.
pub fn enforce_max_size(len: u64, max_output_bytes: usize) -> Result<(), ToolFailure> {
    if len > max_output_bytes as u64 {
        return Err(ToolFailure::TooLarge {
            limit: max_output_bytes,
        });
    }
    Ok(())
}

/// Refuses content that looks binary: a NUL byte anywhere in the first 8 KiB (`M3-6`).
/// Text formats never contain NUL; this is the same heuristic `file`/`grep -I` use.
pub fn refuse_binary(bytes: &[u8]) -> Result<(), ToolFailure> {
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.contains(&0) {
        return Err(ToolFailure::Denied(
            "binary content refused (NUL byte found)".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_lines_starts_from_the_given_offset() {
        let numbered = number_lines("a\nb\nc", 40);
        assert_eq!(numbered, "    40\ta\n    41\tb\n    42\tc");
    }

    #[test]
    fn truncate_with_notice_passes_short_text_through_unchanged() {
        let text = "hello world";
        assert_eq!(truncate_with_notice(text, 1024), text);
    }

    #[test]
    fn truncate_with_notice_cuts_at_the_byte_limit_and_appends_a_notice() {
        let text = "0123456789";
        let truncated = truncate_with_notice(text, 4);
        assert_eq!(truncated, "0123\n... [truncated: showing 4 of 10 bytes]");
    }

    #[test]
    fn truncate_with_notice_never_splits_a_utf8_character() {
        // Each "é" is 2 bytes; a limit of 3 lands mid-character on the naive cut.
        let text = "ééé";
        let truncated = truncate_with_notice(text, 3);
        assert!(truncated.starts_with("é\n"));
    }

    #[test]
    fn error_display_strings_are_written_for_a_model_to_read() {
        assert_eq!(
            ToolFailure::NotFound("src/x.rs".into()).to_string(),
            "not found: src/x.rs"
        );
        assert_eq!(
            ToolFailure::Denied("id_rsa".into()).to_string(),
            "denied: id_rsa"
        );
        assert_eq!(
            ToolFailure::InvalidArgs("start_line must be >= 1".into()).to_string(),
            "invalid argument: start_line must be >= 1"
        );
        assert_eq!(
            ToolFailure::TooLarge { limit: 1024 }.to_string(),
            "too large (limit 1024 bytes)"
        );
        assert_eq!(ToolFailure::Cancelled.to_string(), "cancelled by user");
    }

    #[test]
    fn tool_failure_display_survives_the_conversion_to_model_feedback() {
        let error: ToolExecutionError = ToolFailure::NotFound("src/x.rs".into()).into();
        assert_eq!(error.kind(), ToolErrorKind::NotFound);
        assert_eq!(error.model_feedback(), Some("not found: src/x.rs"));
    }

    #[test]
    fn enforce_max_size_refuses_oversized_content() {
        assert!(enforce_max_size(10, 20).is_ok());
        assert!(matches!(
            enforce_max_size(30, 20),
            Err(ToolFailure::TooLarge { limit: 20 })
        ));
    }

    #[test]
    fn refuse_binary_detects_a_nul_byte_in_the_first_8kib() {
        assert!(refuse_binary(b"plain text").is_ok());
        assert!(refuse_binary(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").is_err());
    }
}
