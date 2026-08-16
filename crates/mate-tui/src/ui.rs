//! Rendering: a one-row tab bar (`M8-1`) over the single-tab body `M7` already built. The tab
//! bar is a pure function of `Vec<TabSummary>` + the active index — [`tab_bar_segments`] does
//! the overflow/windowing math.
//!
//! [`AppView`] wraps the tab bar's input together with the active tab's own [`View`] (`M7-3`/
//! `M7-4`/`M7-5`, unchanged) — switching tabs swaps which session's `View` gets built, not
//! anything about how a session renders.
//!
//! No test module here (see `.agents/docs/testing.md`) — rendering/layout tests were dropped as
//! not worth their upkeep.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::SpawnField;
use crate::input::InputBox;
use crate::transcript::{Entry, Transcript};
use crate::wrap::WrapCache;

const MIN_INPUT_HEIGHT: u16 = 3;
const MAX_INPUT_HEIGHT: u16 = 10;
/// Fixed width reserved for a rendered line's role prefix (`you ›`, `agent ›`, a tool glyph),
/// so wrapped body text lines up under the first line regardless of which prefix produced it.
const PREFIX_WIDTH: u16 = 8;
/// Height of the bottom status bar (glyphs + values, no border).
const STATUS_BAR_HEIGHT: u16 = 1;
/// Tab titles longer than this are middle-truncated with `…` — keeps every tab's width
/// bounded, which is what makes the overflow math in [`tab_bar_segments`] tractable.
const MAX_TAB_TITLE: usize = 10;

/// One tab's worth of state the tab bar needs to render it (`M8-1`/`M8-4`) — owned rather than
/// borrowed, since it's cheap to clone a handful of these per frame and doing so sidesteps
/// borrowing the same `Vec<SessionTab>` both immutably (for every tab's summary) and mutably
/// (for the active tab's [`WrapCache`]) at once.
pub(crate) struct TabSummary {
    pub(crate) title: String,
    pub(crate) streaming: bool,
    pub(crate) unread: bool,
    pub(crate) needs_attention: bool,
}

pub(crate) struct View<'a> {
    pub(crate) transcript: &'a Transcript,
    pub(crate) wrap: &'a mut WrapCache,
    pub(crate) input: &'a InputBox,
    pub(crate) scroll: usize,
    pub(crate) running_turn: bool,
    pub(crate) model: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) tokens_sent: u64,
    pub(crate) tokens_received: u64,
    pub(crate) quit_armed: bool,
    pub(crate) close_confirm: bool,
}

/// `Ctrl+T`'s spawn form (`M8-3`), borrowed for one frame's render. `error`, when set, is shown
/// under the fields rather than replacing them — the user's half-typed directory shouldn't
/// vanish just because it didn't resolve.
pub(crate) struct SpawnFormView<'a> {
    pub(crate) dir: &'a str,
    pub(crate) model: &'a str,
    pub(crate) http_enabled: bool,
    pub(crate) focus: SpawnField,
    pub(crate) error: Option<&'a str>,
}

pub(crate) struct AppView<'a> {
    pub(crate) tabs: Vec<TabSummary>,
    pub(crate) active: usize,
    pub(crate) session: View<'a>,
    pub(crate) spawn_form: Option<SpawnFormView<'a>>,
}

pub(crate) fn draw(f: &mut Frame<'_>, view: &mut AppView<'_>) {
    let area = f.area();
    let [tab_area, body_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(STATUS_BAR_HEIGHT),
    ])
    .areas(area);
    render_tab_bar(f, tab_area, &view.tabs, view.active);
    draw_body(f, body_area, &mut view.session);
    render_status_bar(f, status_area, &view.session);
    if let Some(form) = &view.spawn_form {
        render_spawn_form(f, area, form);
    }
}

fn draw_body(f: &mut Frame<'_>, area: Rect, view: &mut View<'_>) {
    let input_height = input_height(view);
    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(input_height)]).areas(area);

    if view.transcript.is_empty() {
        render_welcome(f, transcript_area);
    } else {
        render_transcript(f, transcript_area, view);
    }
    render_input(f, input_area, view);
}

const RAINBOW: [Color; 6] = [
    Color::Red,
    Color::Yellow,
    Color::Green,
    Color::Cyan,
    Color::Blue,
    Color::Magenta,
];

const MATE_ART: [&str; 7] = [
    "                                 .            ",
    "                              .o8            ",
    "ooo. .oo.  .oo.    .oooo.   .o888oo  .ooooo. ",
    "`888P\"Y88bP\"Y88b  `P  )88b    888   d88' `88b",
    " 888   888   888   .oP\"888    888   888ooo888",
    " 888   888   888  d8(  888    888 . 888    .o",
    "o888o o888o o888o `Y888\"\"8o   \"888\" `Y8bod8P'",
];

/// Welcome screen (startup, and any freshly `Ctrl+T`-spawned tab): a centered "mate" wordmark
/// stands in for the transcript until [`Transcript::is_empty`] turns false, i.e. until the
/// tab's first prompt is sent. Each column cycles through [`RAINBOW`] for the rainbow effect.
fn render_welcome(f: &mut Frame<'_>, area: Rect) {
    let width = MATE_ART
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let height = MATE_ART.len() as u16;
    let popup = centered_rect(width, height, area);

    let lines: Vec<Line<'static>> = MATE_ART
        .iter()
        .map(|row| {
            let spans: Vec<Span<'static>> = row
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if c == ' ' {
                        Span::raw(" ")
                    } else {
                        Span::styled(
                            c.to_string(),
                            Style::default().fg(RAINBOW[i % RAINBOW.len()]),
                        )
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect();

    f.render_widget(Paragraph::new(lines), popup);
}

struct TabSegment {
    text: String,
    active: bool,
}

fn truncate_title(title: &str) -> String {
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= MAX_TAB_TITLE {
        return title.to_string();
    }
    let mut truncated: String = chars[..MAX_TAB_TITLE.saturating_sub(1)].iter().collect();
    truncated.push('…');
    truncated
}

fn tab_marker(tab: &TabSummary) -> &'static str {
    if tab.streaming {
        " ⣾"
    } else if tab.needs_attention {
        " !"
    } else if tab.unread {
        " ●"
    } else {
        ""
    }
}

fn tab_label(n: usize, tab: &TabSummary) -> String {
    format!("⟨{n} {}{}⟩", truncate_title(&tab.title), tab_marker(tab))
}

/// Builds the tab bar's segments (`M8-1`), windowing around `active` so it stays visible when
/// the full set doesn't fit in `width` columns, and reporting how many tabs are hidden on each
/// side. Pure and unit-tested directly against plain strings (see the tests module) — the
/// render side (`render_tab_bar`) only adds styling on top of what this returns.
fn tab_bar_segments(tabs: &[TabSummary], active: usize, width: u16) -> Vec<TabSegment> {
    let width = width as usize;
    if tabs.is_empty() {
        return vec![TabSegment {
            text: "[+]".to_string(),
            active: false,
        }];
    }
    let active = active.min(tabs.len() - 1);
    let labels: Vec<String> = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| tab_label(i + 1, t))
        .collect();

    const PLUS: &str = "[+]";
    let full_width: usize = labels.iter().map(|l| l.chars().count()).sum::<usize>()
        + labels.len()
        + PLUS.chars().count();
    if full_width <= width {
        let mut segs: Vec<TabSegment> = labels
            .iter()
            .enumerate()
            .map(|(i, text)| TabSegment {
                text: format!("{text} "),
                active: i == active,
            })
            .collect();
        segs.push(TabSegment {
            text: PLUS.to_string(),
            active: false,
        });
        return segs;
    }

    // Doesn't fit as-is: grow a window outward from `active`, cheaper side first, stopping
    // early enough to leave room for hidden-count indicators and `[+]`.
    const RESERVE: usize = 10;
    let mut lo = active;
    let mut hi = active;
    let mut used = labels[active].chars().count();
    loop {
        let can_left = lo > 0;
        let can_right = hi + 1 < labels.len();
        if !can_left && !can_right {
            break;
        }
        let left_w = if can_left {
            labels[lo - 1].chars().count() + 1
        } else {
            usize::MAX
        };
        let right_w = if can_right {
            labels[hi + 1].chars().count() + 1
        } else {
            usize::MAX
        };
        if can_left && left_w <= right_w {
            if used + left_w + RESERVE > width {
                break;
            }
            lo -= 1;
            used += left_w;
        } else if can_right {
            if used + right_w + RESERVE > width {
                break;
            }
            hi += 1;
            used += right_w;
        } else {
            break;
        }
    }

    let hidden_left = lo;
    let hidden_right = labels.len() - 1 - hi;

    let mut segs = Vec::new();
    if hidden_left > 0 {
        segs.push(TabSegment {
            text: format!("«{hidden_left} "),
            active: false,
        });
    }
    for (i, label) in labels.iter().enumerate().take(hi + 1).skip(lo) {
        segs.push(TabSegment {
            text: format!("{label} "),
            active: i == active,
        });
    }
    if hidden_right > 0 {
        segs.push(TabSegment {
            text: format!("{hidden_right}» "),
            active: false,
        });
    }
    segs.push(TabSegment {
        text: PLUS.to_string(),
        active: false,
    });
    segs
}

fn render_tab_bar(f: &mut Frame<'_>, area: Rect, tabs: &[TabSummary], active: usize) {
    let spans: Vec<Span<'static>> = tab_bar_segments(tabs, active, area.width)
        .into_iter()
        .map(|seg| {
            let style = if seg.active {
                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Span::styled(seg.text, style)
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A small centered popup (`M8-3`) — `Clear` first, since ratatui doesn't clear what's
/// underneath a widget on its own and the transcript would otherwise show through.
fn render_spawn_form(f: &mut Frame<'_>, area: Rect, form: &SpawnFormView<'_>) {
    let width = area.width.saturating_sub(4).clamp(20, 50);
    let height = if form.error.is_some() { 6 } else { 5 };
    let popup = centered_rect(width, height, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" new session — Enter submit, Esc cancel ");
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let field = |label: &str, value: String, focused: bool| {
        let style = if focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        Line::from(vec![
            Span::raw(format!("{label}: ")),
            Span::styled(value, style),
        ])
    };
    let mut lines = vec![
        field(
            "dir",
            if form.dir.is_empty() {
                ".".to_string()
            } else {
                form.dir.to_string()
            },
            form.focus == SpawnField::Dir,
        ),
        field(
            "model",
            if form.model.is_empty() {
                "(default)".to_string()
            } else {
                form.model.to_string()
            },
            form.focus == SpawnField::Model,
        ),
        field(
            "http",
            if form.http_enabled { "on" } else { "off" }.to_string(),
            form.focus == SpawnField::Http,
        ),
    ];
    if let Some(err) = form.error {
        lines.push(Line::from(Span::styled(
            err.to_string(),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Bottom status bar (deprecates the old left model/provider panel): a single row, no border,
/// filled with a solid gray background — glyphs stand in for labels so the values (provider,
/// agent model, tokens sent/received) fit without eating transcript width.
fn render_status_bar(f: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let style = Style::default().bg(Color::DarkGray).fg(Color::White);
    let line = Line::from(format!(
        " 📡 {}  🤖 {}  ↑{}  ↓{}",
        view.provider, view.model, view.tokens_sent, view.tokens_received
    ));
    f.render_widget(Paragraph::new(line).style(style), area);
}

fn input_height(view: &View<'_>) -> u16 {
    let lines = view.input.textarea().lines().len() as u16;
    (lines + 2).clamp(MIN_INPUT_HEIGHT, MAX_INPUT_HEIGHT)
}

fn render_input(f: &mut Frame<'_>, area: Rect, view: &View<'_>) {
    let title = if view.close_confirm {
        " mate · Ctrl+W again to close (streaming) "
    } else if view.quit_armed {
        " mate · Ctrl+C again to quit "
    } else if view.running_turn {
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
