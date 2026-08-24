//! `write_file`'s diff preview (§ write_file diff): turns a [`ToolPreview`] (one diff line per
//! source line) into ratatui spans — syntax-highlighted via `synoptic` where the file's
//! extension is recognized, with a green/red background marking added/removed lines. Computed
//! once per transcript entry and cached, the same "compute on first render, reuse after" shape
//! [`crate::wrap::WrapCache`] uses for everything else — the diff/path never change once
//! `Transcript::attach_preview` sets them, so there's no invalidation to wire up beyond `/clear`
//! dropping the whole cache.

use std::collections::HashMap;

use mate_tool_api::DiffTag;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use synoptic::TokOpt;

use crate::transcript::{EntryId, ToolPreview};

const INSERT_BG: Color = Color::Rgb(4, 45, 23);
const DELETE_BG: Color = Color::Rgb(56, 10, 15);

pub(crate) struct PreviewCache {
    cached: HashMap<EntryId, Vec<Vec<Span<'static>>>>,
}

impl PreviewCache {
    pub(crate) fn new() -> Self {
        Self {
            cached: HashMap::new(),
        }
    }

    /// Content spans only — no gutter prefix; that's `ui.rs`'s job, the same split the plain
    /// `styled_line` path uses. One `Vec<Span>` per diff line.
    pub(crate) fn rows(&mut self, id: EntryId, preview: &ToolPreview) -> &[Vec<Span<'static>>] {
        self.cached.entry(id).or_insert_with(|| build_rows(preview))
    }
}

fn build_rows(preview: &ToolPreview) -> Vec<Vec<Span<'static>>> {
    let ext = preview
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mut highlighter = synoptic::from_extension(ext, 4);
    let lines: Vec<String> = preview.diff.iter().map(|d| d.text.clone()).collect();
    if let Some(h) = highlighter.as_mut() {
        h.run(&lines);
    }

    preview
        .diff
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let bg = match d.tag {
                DiffTag::Insert => Some(INSERT_BG),
                DiffTag::Delete => Some(DELETE_BG),
                DiffTag::Equal => None,
            };
            let (marker, marker_fg) = match d.tag {
                DiffTag::Insert => ("+ ", Color::Green),
                DiffTag::Delete => ("- ", Color::Red),
                DiffTag::Equal => ("  ", Color::DarkGray),
            };
            let mut spans = vec![Span::styled(
                marker,
                bg_style(bg).fg(marker_fg).add_modifier(Modifier::BOLD),
            )];
            match &highlighter {
                Some(h) => {
                    for token in h.line(i, &lines[i]) {
                        match token {
                            TokOpt::Some(text, kind) => spans.push(Span::styled(
                                text,
                                bg_style(bg).patch(style_for_kind(&kind)),
                            )),
                            TokOpt::None(text) => spans.push(Span::styled(text, bg_style(bg))),
                        }
                    }
                }
                None => spans.push(Span::styled(d.text.clone(), bg_style(bg))),
            }
            spans
        })
        .collect()
}

fn bg_style(bg: Option<Color>) -> Style {
    match bg {
        Some(color) => Style::default().bg(color),
        None => Style::default(),
    }
}

/// Loose substring match rather than an exhaustive one — `synoptic`'s bundled language files
/// each define their own rule names (`"keyword"`, `"digits"`, ...) and the exact vocabulary
/// isn't fully enumerated anywhere; a token whose `kind` doesn't match anything here just keeps
/// the plain foreground instead of erroring.
fn style_for_kind(kind: &str) -> Style {
    let fg = if kind.contains("comment") {
        Some(Color::DarkGray)
    } else if kind.contains("string") {
        Some(Color::Green)
    } else if kind.contains("keyword") || kind.contains("macro") || kind.contains("attribute") {
        Some(Color::Magenta)
    } else if kind.contains("digit") || kind.contains("number") {
        Some(Color::Cyan)
    } else if kind.contains("bool") {
        Some(Color::Yellow)
    } else {
        None
    };
    match fg {
        Some(color) => Style::default().fg(color),
        None => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_kind_substrings_to_a_foreground_color() {
        assert_eq!(
            style_for_kind("comment"),
            Style::default().fg(Color::DarkGray)
        );
        assert_eq!(style_for_kind("string"), Style::default().fg(Color::Green));
        assert_eq!(
            style_for_kind("keyword"),
            Style::default().fg(Color::Magenta)
        );
        assert_eq!(style_for_kind("digits"), Style::default().fg(Color::Cyan));
        assert_eq!(
            style_for_kind("boolean"),
            Style::default().fg(Color::Yellow)
        );
    }

    #[test]
    fn an_unrecognized_kind_gets_no_foreground_override() {
        assert_eq!(
            style_for_kind("some-language-specific-rule-name"),
            Style::default(),
            "an unmapped kind must fall back to the plain style, not panic or guess"
        );
    }

    #[test]
    fn bg_style_only_sets_the_background_when_given_one() {
        assert_eq!(bg_style(Some(Color::Red)), Style::default().bg(Color::Red));
        assert_eq!(bg_style(None), Style::default());
    }
}
