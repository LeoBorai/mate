//! Rendering (`M7-3`/`M7-4`/`M7-5`): the transcript fills everything above the input box, which
//! grows with its content up to a cap. [`View`] carries only what rendering actually needs —
//! not the whole [`crate::app::App`], so this stays testable against a bare [`Transcript`],
//! [`WrapCache`], and [`InputBox`] with no session behind them (`M7-6`).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::input::InputBox;
use crate::transcript::{Entry, Transcript};
use crate::wrap::WrapCache;

const MIN_INPUT_HEIGHT: u16 = 3;
const MAX_INPUT_HEIGHT: u16 = 10;
/// Fixed width reserved for a rendered line's role prefix (`you ›`, `agent ›`, a tool glyph),
/// so wrapped body text lines up under the first line regardless of which prefix produced it.
const PREFIX_WIDTH: u16 = 8;

pub(crate) struct View<'a> {
    pub(crate) transcript: &'a Transcript,
    pub(crate) wrap: &'a mut WrapCache,
    pub(crate) input: &'a InputBox,
    pub(crate) scroll: usize,
    pub(crate) running_turn: bool,
}

pub(crate) fn draw(f: &mut Frame<'_>, view: &mut View<'_>) {
    let area = f.area();
    let input_height = input_height(view);
    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(input_height)]).areas(area);

    render_transcript(f, transcript_area, view);
    render_input(f, input_area, view);
}

fn input_height(view: &View<'_>) -> u16 {
    let lines = view.input.textarea().lines().len() as u16;
    (lines + 2).clamp(MIN_INPUT_HEIGHT, MAX_INPUT_HEIGHT)
}

fn render_input(f: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let title = if view.running_turn {
        " mate · running… "
    } else {
        " mate "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(view.input.textarea(), inner);
}

fn render_transcript(f: &mut Frame<'_>, area: Rect, view: &mut View<'_>) {
    let width = area.width.saturating_sub(PREFIX_WIDTH + 1);
    let need = area.height as usize + view.scroll;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in view.transcript.iter().rev() {
        if lines.len() >= need {
            break;
        }
        let text = render_text(entry);
        let wrapped = view.wrap.wrapped(entry.id(), &text, width).to_vec();
        for (i, raw) in wrapped.iter().enumerate().rev() {
            lines.push(styled_line(entry, raw, i == 0));
            if lines.len() >= need {
                break;
            }
        }
    }
    lines.reverse();

    let take = lines.len().saturating_sub(view.scroll);
    let visible = &lines[..take];
    let start = visible.len().saturating_sub(area.height as usize);

    let para = Paragraph::new(visible[start..].to_vec());
    f.render_widget(para, area);
}

fn render_text(entry: &Entry) -> String {
    match entry {
        Entry::User { text, .. }
        | Entry::Assistant { text, .. }
        | Entry::SystemError { text, .. } => text.clone(),
        Entry::ToolCall {
            name,
            ok,
            summary,
            expanded,
            ..
        } => {
            let status = match ok {
                None => "…",
                Some(true) => "ok",
                Some(false) => "failed",
            };
            if *expanded && !summary.is_empty() {
                format!("{name} {status}\n{summary}")
            } else {
                format!("{name} {status}")
            }
        }
    }
}

fn styled_line(entry: &Entry, raw: &str, first: bool) -> Line<'static> {
    let (label, style) = match entry {
        Entry::User { .. } => ("you ›", Style::default().fg(Color::Cyan)),
        Entry::Assistant { .. } => ("agent ›", Style::default()),
        Entry::ToolCall { ok, .. } => (
            match ok {
                None => "⚙",
                Some(true) => "✓",
                Some(false) => "✗",
            },
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
        Entry::SystemError { .. } => ("!", Style::default().fg(Color::Red)),
    };
    let prefix = if first {
        format!("{label:<width$}", width = PREFIX_WIDTH as usize)
    } else {
        " ".repeat(PREFIX_WIDTH as usize)
    };
    Line::from(vec![
        Span::styled(prefix, style),
        Span::raw(raw.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    /// The `M7-6` baseline snapshot: a short exchange with a resolved tool call, rendered
    /// against a fixed-size `TestBackend` — no session, backend, or terminal required.
    #[test]
    fn baseline_snapshot() {
        let mut transcript = Transcript::new();
        transcript.push_user("what does build_toolset do?".to_string());
        transcript.push_token("Let me check the source.");
        transcript.push_tool_call("read_file".to_string());
        transcript.resolve_tool_call("read_file", true, "210 lines".to_string());
        transcript.push_token("It assembles the toolset for one agent.");
        transcript.end_turn();

        let mut wrap = WrapCache::new();
        let input = InputBox::new();
        let mut view = View {
            transcript: &transcript,
            wrap: &mut wrap,
            input: &input,
            scroll: 0,
            running_turn: false,
        };

        let backend = TestBackend::new(60, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut view)).unwrap();

        insta::assert_snapshot!(terminal.backend());
    }
}
