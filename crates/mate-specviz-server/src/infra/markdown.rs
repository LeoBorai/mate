use pulldown_cmark::{CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::html::{IncludeBackground, styled_line_to_highlighted_html};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::domain::DocKind;
use crate::domain::openspec::{delta_op, is_requirement_heading, is_scenario_heading};

/// Every class OpenSpec-aware rendering may emit, per element. Sanitization
/// allows exactly these and strips any other class, including ones authored
/// in a document's raw HTML. The client styles them in `style/input.css`.
const SV_CLASSES: &[(&str, &[&str])] = &[
    (
        "h2",
        &[
            "sv-delta",
            "sv-delta-added",
            "sv-delta-modified",
            "sv-delta-removed",
            "sv-delta-renamed",
        ],
    ),
    ("h3", &["sv-req"]),
    ("section", &["sv-scenario"]),
    ("span", &["sv-badge"]),
    ("strong", &["sv-kw"]),
];

const SCENARIO_KEYWORDS: &[&str] = &["WHEN", "THEN", "AND"];

pub trait MarkdownRenderer: Send + Sync {
    /// Markdown -> sanitized HTML: GFM parse, OpenSpec structure (only for
    /// `DocKind::OpenSpec`), server-side syntax highlighting on fenced code
    /// blocks, then sanitize.
    fn render(&self, markdown: &str, kind: DocKind) -> String;
}

pub struct PulldownRenderer {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl PulldownRenderer {
    pub fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme = ThemeSet::load_defaults().themes["InspiredGitHub"].clone();
        Self { syntax_set, theme }
    }

    /// Buffers fenced code blocks so their text events can be highlighted
    /// as one unit, then re-emits each as a single trusted `Html` event.
    /// Every other event passes through unchanged.
    fn with_highlighted_code<'a>(&self, input: Vec<Event<'a>>) -> Vec<Event<'a>> {
        let mut events = Vec::with_capacity(input.len());
        let mut code_block: Option<(String, String)> = None;

        for event in input {
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    code_block = Some((lang, String::new()));
                }
                Event::Text(text) if code_block.is_some() => {
                    if let Some((_, buf)) = code_block.as_mut() {
                        buf.push_str(&text);
                    }
                }
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((lang, code)) = code_block.take() {
                        let html = self.highlight_code_block(&code, &lang);
                        events.push(Event::Html(CowStr::from(html)));
                    }
                }
                other => events.push(other),
            }
        }

        events
    }

    fn highlight_code_block(&self, code: &str, lang: &str) -> String {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);

        let mut out = String::from("<pre><code>");
        for line in LinesWithEndings::from(code) {
            let Ok(ranges) = highlighter.highlight_line(line, &self.syntax_set) else {
                continue;
            };
            if let Ok(html) = styled_line_to_highlighted_html(&ranges, IncludeBackground::No) {
                out.push_str(&html);
            }
        }
        out.push_str("</code></pre>");
        out
    }
}

impl Default for PulldownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownRenderer for PulldownRenderer {
    fn render(&self, markdown: &str, kind: DocKind) -> String {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES;

        let mut events: Vec<Event<'_>> = Parser::new_ext(markdown, options).collect();
        if kind == DocKind::OpenSpec {
            events = with_openspec_structure(events);
        }
        let events = self.with_highlighted_code(events);

        let mut raw_html = String::new();
        html::push_html(&mut raw_html, events.into_iter());

        sanitize(&raw_html)
    }
}

/// Rewrites OpenSpec's Markdown conventions into classed markup:
///
/// - `## ADDED|MODIFIED|REMOVED|RENAMED Requirements` -> `h2.sv-delta` whose
///   operation word is a `span.sv-badge`;
/// - `### Requirement: ...` -> `h3.sv-req`;
/// - `#### Scenario: ...` and everything after it, up to the next heading
///   of level 1-4, wrapped in `section.sv-scenario`;
/// - inside a scenario, a list item's leading `WHEN`/`THEN`/`AND` (plain or
///   already bold) -> `strong.sv-kw`.
///
/// A delta heading is re-emitted as badge + "Requirements": its whole text
/// already matched `delta_op`, so nothing authored is lost. Other heading
/// content passes through as ordinary events, so inline
/// markup inside a requirement heading still renders (and is still escaped)
/// as usual.
fn with_openspec_structure(events: Vec<Event<'_>>) -> Vec<Event<'_>> {
    let mut out = Vec::with_capacity(events.len() + 8);
    let mut in_scenario = false;
    let mut i = 0;

    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::Heading { level, .. }) => {
                let level = *level;
                let Some(end) = heading_end(&events, i) else {
                    out.extend(events[i..].iter().cloned());
                    break;
                };
                let inner = &events[i + 1..end];
                let text = plain_text(inner);

                if in_scenario && heading_rank(level) <= 4 {
                    out.push(Event::Html(CowStr::from("</section>")));
                    in_scenario = false;
                }

                if level == HeadingLevel::H4 && is_scenario_heading(&text) {
                    out.push(Event::Html(CowStr::from(r#"<section class="sv-scenario">"#)));
                    in_scenario = true;
                }

                match (level, delta_op(&text)) {
                    (HeadingLevel::H2, Some(op)) => {
                        out.push(Event::Html(CowStr::from(format!(
                            r#"<h2 class="sv-delta {}"><span class="sv-badge">{}</span> Requirements</h2>"#,
                            op.class(),
                            op.label()
                        ))));
                    }
                    (HeadingLevel::H3, _) if is_requirement_heading(&text) => {
                        out.push(Event::Html(CowStr::from(r#"<h3 class="sv-req">"#)));
                        out.extend(inner.iter().cloned());
                        out.push(Event::Html(CowStr::from("</h3>")));
                    }
                    _ => out.extend(events[i..=end].iter().cloned()),
                }
                i = end + 1;
            }
            Event::Start(Tag::Item) if in_scenario => {
                out.push(events[i].clone());
                let mut j = i + 1;
                if matches!(events.get(j), Some(Event::Start(Tag::Paragraph))) {
                    out.push(events[j].clone());
                    j += 1;
                }
                i = emphasize_keyword(&events, j, &mut out);
            }
            other => {
                out.push(other.clone());
                i += 1;
            }
        }
    }

    if in_scenario {
        out.push(Event::Html(CowStr::from("</section>")));
    }
    out
}

/// If a scenario keyword starts at `events[j]` — either `**WHEN**` (a
/// `Strong` wrapping only the keyword) or plain text beginning `WHEN ` —
/// pushes it as `strong.sv-kw` (plus any trailing text) and returns the
/// index after what it consumed; otherwise returns `j` untouched.
fn emphasize_keyword<'a>(events: &[Event<'a>], j: usize, out: &mut Vec<Event<'a>>) -> usize {
    if let (
        Some(Event::Start(Tag::Strong)),
        Some(Event::Text(text)),
        Some(Event::End(TagEnd::Strong)),
    ) = (events.get(j), events.get(j + 1), events.get(j + 2))
        && let Some(keyword) = SCENARIO_KEYWORDS.iter().find(|k| text.trim() == **k)
    {
        out.push(keyword_html(keyword));
        return j + 3;
    }

    if let Some(Event::Text(text)) = events.get(j) {
        for keyword in SCENARIO_KEYWORDS {
            if let Some(rest) = text.strip_prefix(keyword)
                && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            {
                out.push(keyword_html(keyword));
                if !rest.is_empty() {
                    out.push(Event::Text(CowStr::from(rest.to_owned())));
                }
                return j + 1;
            }
        }
    }

    j
}

fn keyword_html(keyword: &str) -> Event<'static> {
    Event::InlineHtml(CowStr::from(format!(
        r#"<strong class="sv-kw">{keyword}</strong>"#
    )))
}

/// Index of the `End(Heading)` matching the `Start(Heading)` at `start`.
fn heading_end(events: &[Event<'_>], start: usize) -> Option<usize> {
    events[start + 1..]
        .iter()
        .position(|e| matches!(e, Event::End(TagEnd::Heading(_))))
        .map(|offset| start + 1 + offset)
}

/// A heading's text content with inline markup stripped.
fn plain_text(events: &[Event<'_>]) -> String {
    let mut text = String::new();
    for event in events {
        if let Event::Text(t) | Event::Code(t) = event {
            text.push_str(t);
        }
    }
    text
}

fn heading_rank(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn sanitize(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(["span", "section"])
        .add_tag_attributes("span", ["style"]);
    for (tag, classes) in SV_CLASSES {
        builder.add_allowed_classes(tag, classes.iter().copied());
    }
    builder.clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_plain(md: &str) -> String {
        PulldownRenderer::new().render(md, DocKind::Plain)
    }

    fn render_openspec(md: &str) -> String {
        PulldownRenderer::new().render(md, DocKind::OpenSpec)
    }

    #[test]
    fn renders_headings_and_tables() {
        let html = render_plain("# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<h1>Title</h1>"), "H1 renders as-is: {html}");
        assert!(html.contains("<table>"), "GFM tables are enabled: {html}");
    }

    /// Fenced code blocks come back with syntect's inline-styled spans, not
    /// just a plain `<pre><code>` wrap.
    #[test]
    fn highlights_fenced_code_blocks() {
        let html = render_plain("```rust\nfn main() {}\n```\n");
        assert!(html.contains("<pre><code>"), "code block wrapper: {html}");
        assert!(html.contains("style="), "syntect's inline styles survive sanitizing: {html}");
    }

    #[test]
    fn strips_disallowed_tags() {
        let html = render_plain("<script>alert(1)</script>\n\ntext");
        assert!(!html.contains("<script>"), "scripts never survive: {html}");
    }

    #[test]
    fn delta_headings_in_a_plain_doc_stay_plain() {
        let html = render_plain("## ADDED Requirements\n");
        assert!(
            html.contains("<h2>ADDED Requirements</h2>"),
            "plain docs get no OpenSpec treatment: {html}"
        );
        assert!(!html.contains("sv-"), "no viewer classes at all: {html}");
    }

    #[test]
    fn delta_headings_get_distinct_operation_badges() {
        let html = render_openspec("## ADDED Requirements\n\n## REMOVED Requirements\n");
        assert!(
            html.contains(r#"<h2 class="sv-delta sv-delta-added"><span class="sv-badge">ADDED</span>"#),
            "added badge: {html}"
        );
        assert!(
            html.contains(r#"<h2 class="sv-delta sv-delta-removed"><span class="sv-badge">REMOVED</span>"#),
            "removed badge, distinct from added: {html}"
        );
    }

    #[test]
    fn requirement_headings_are_classed() {
        let html = render_openspec("### Requirement: Export\n");
        assert!(
            html.contains(r#"<h3 class="sv-req">Requirement: Export</h3>"#),
            "requirement heading: {html}"
        );
    }

    #[test]
    fn consecutive_scenarios_do_not_nest() {
        let md = "### Requirement: R\n\n#### Scenario: one\n- **WHEN** a\n- **THEN** b\n\n#### Scenario: two\n- WHEN c\n- THEN d\n";
        let html = render_openspec(md);
        assert_eq!(
            html.matches(r#"<section class="sv-scenario">"#).count(),
            2,
            "one section per scenario: {html}"
        );
        let second = html.find("Scenario: two").unwrap();
        let first_close = html.find("</section>").unwrap();
        assert!(
            first_close < second,
            "the first scenario closes before the second opens: {html}"
        );
        assert!(html.trim_end().ends_with("</section>"), "the last scenario closes at EOF: {html}");
    }

    #[test]
    fn a_requirement_heading_closes_an_open_scenario() {
        let html = render_openspec("#### Scenario: s\n- WHEN a\n\n### Requirement: next\n");
        let close = html.find("</section>").unwrap();
        let next = html.find("Requirement: next").unwrap();
        assert!(close < next, "a level-3 heading ends the scenario: {html}");
    }

    #[test]
    fn scenario_keywords_are_emphasized_bold_or_plain() {
        let html = render_openspec("#### Scenario: s\n- **WHEN** the user saves\n- THEN it works\n- AND more\n");
        assert!(
            html.contains(r#"<strong class="sv-kw">WHEN</strong> the user saves"#),
            "already-bold keyword: {html}"
        );
        assert!(
            html.contains(r#"<strong class="sv-kw">THEN</strong> it works"#),
            "plain keyword: {html}"
        );
        assert!(html.contains(r#"<strong class="sv-kw">AND</strong> more"#), "AND: {html}");
    }

    #[test]
    fn keywords_outside_scenarios_are_left_alone() {
        let html = render_openspec("- WHEN outside\n");
        assert!(!html.contains("sv-kw"), "only scenario bullets get keyword emphasis: {html}");
    }

    #[test]
    fn authored_classes_are_stripped() {
        let html = render_openspec("<div class=\"evil\">x</div>\n\n<h2 class=\"sv-delta evil\">y</h2>\n");
        assert!(!html.contains("evil"), "classes outside the sv-* set never survive: {html}");
        assert!(!html.contains("<script"), "sanitizing still applies: {html}");
    }
}
