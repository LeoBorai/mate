//! The agent status panel's widget framework (§9.2/§9.6/§9.7/§9.8, `M12-1`): a vertical stack
//! of [`PanelWidget`]s over one shared [`PanelView`], not a hardcoded layout. `AgentStatusPanel`
//! registers the five widgets §9.2 lists, in order, and owns the vertical-budget allocation
//! (`M12-3`) — `ModelWidget`/`ContextWidget` are fixed-height and always render in full; the
//! three list widgets share the remainder, collapsing in reverse priority order **documents,
//! then network, then subagents** (§9.2: subagents are the liveness signal during delegation,
//! so they're the last thing to lose room), except a focused widget (`M12-9`) always goes first
//! regardless of its usual priority — "expands on focus" (§9.6).
//!
//! Every widget here is a stateless unit struct — the state it renders lives entirely on
//! [`crate::app::SessionTab`] (§9.1: "no panel data on `App`"), reached through the borrowed
//! [`PanelView`] built fresh each frame in `crate::app::App::view`.

use std::collections::VecDeque;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use mate_core::cost::CostEstimate;
use mate_core::streaming::UsageRollup;
use mate_tool_api::{AgentId, FileOp};

use crate::panel::{DocRow, NetRow};
use crate::roster::{ROSTER_SHOWN, SubagentRow, SubagentStatus};
use crate::text::{middle_truncate, truncate_end, truncate_left};

/// Rows of the network/documents logs actually drawn beyond the header (§9.7/§9.8) — the ring
/// buffers themselves hold up to 50; only the newest few are worth screen space.
const NETWORK_SHOWN: usize = 6;
const DOCUMENTS_SHOWN: usize = 6;

/// Which panel widget has focus (`M12-9`) — `Tab`/`Shift+Tab` cycles this, `↑`/`↓` moves
/// [`PanelFocus::row`] within whichever of the three list widgets is focused. Lives on
/// `SessionTab`, not here — this enum is just the shared vocabulary `crate::app` and this
/// module both need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelWidgetKind {
    Model,
    Context,
    Subagents,
    Network,
    Documents,
}

impl PanelWidgetKind {
    const ORDER: [PanelWidgetKind; 5] = [
        PanelWidgetKind::Model,
        PanelWidgetKind::Context,
        PanelWidgetKind::Subagents,
        PanelWidgetKind::Network,
        PanelWidgetKind::Documents,
    ];

    pub(crate) fn next(self) -> Self {
        let i = Self::ORDER.iter().position(|k| *k == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    pub(crate) fn prev(self) -> Self {
        let i = Self::ORDER.iter().position(|k| *k == self).unwrap_or(0);
        Self::ORDER[(i + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }

    /// Whether this widget has row-level navigation and an `Enter`-opened detail — `Model` and
    /// `Context` don't (§9.4/§9.5 have no rows to focus; `Context`'s own `Enter` toggles its
    /// root/subagent split instead, handled directly in `crate::app`, not through a row).
    pub(crate) fn is_list(self) -> bool {
        matches!(
            self,
            PanelWidgetKind::Subagents | PanelWidgetKind::Network | PanelWidgetKind::Documents
        )
    }
}

/// `Ctrl+P`/`Tab`/arrows state (`M12-9`), owned by `SessionTab` — a tab's panel focus is that
/// tab's own, same as everything else in §9.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelFocus {
    pub(crate) widget: PanelWidgetKind,
    pub(crate) row: usize,
}

/// Rows needed at full detail, and the floor a widget collapses toward under vertical pressure
/// (§9.2).
pub(crate) struct WidgetSize {
    pub(crate) ideal: u16,
    pub(crate) min: u16,
}

/// One frame's worth of borrowed panel data (§9.1: all of it session-scoped, none of it on
/// `App`). Built fresh in `crate::app::App::view` from the active tab's fields.
pub(crate) struct PanelView<'a> {
    pub(crate) model: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) subagent_model: Option<&'a str>,
    pub(crate) usage: &'a UsageRollup,
    pub(crate) cost: CostEstimate,
    pub(crate) context_split: bool,
    pub(crate) running_turn: bool,
    pub(crate) subagents: &'a VecDeque<SubagentRow>,
    pub(crate) network: &'a VecDeque<NetRow>,
    pub(crate) documents: &'a VecDeque<DocRow>,
    pub(crate) network_turn_requests: u32,
    pub(crate) root: &'a Path,
    pub(crate) focus: Option<PanelFocus>,
}

impl PanelView<'_> {
    fn row_focus(&self, kind: PanelWidgetKind) -> Option<usize> {
        self.focus.filter(|f| f.widget == kind).map(|f| f.row)
    }
}

/// One widget in the panel stack (§9.2). No `on_key` here — every widget is a stateless unit
/// struct (all the state it renders lives on `SessionTab`), and every panel key (`Tab`,
/// arrows, `Enter`, `x`) is routed centrally through `crate::app::App::on_key`, the same place
/// every other app-level key (`Ctrl+B`, `Ctrl+G`, tab switching…) already is, rather than
/// dispatched per widget.
pub(crate) trait PanelWidget {
    fn title(&self) -> &str;
    fn size(&self, view: &PanelView<'_>) -> WidgetSize;
    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, collapsed: bool);
}

struct ModelWidget;
struct ContextWidget;
struct SubagentRosterWidget;
struct NetworkLogWidget;
struct DocumentsLogWidget;

const MODEL_ROWS: u16 = 3;
const CONTEXT_ROWS: u16 = 4;

impl PanelWidget for ModelWidget {
    fn title(&self) -> &str {
        "MODEL"
    }

    fn size(&self, _view: &PanelView<'_>) -> WidgetSize {
        WidgetSize {
            ideal: MODEL_ROWS,
            min: MODEL_ROWS,
        }
    }

    /// §9.4: root model (truncated from the left, since a model id's distinguishing suffix is
    /// what's worth keeping), sub-provider, and the subagent model only when it differs from
    /// the root's — always exactly three lines regardless, so the widget never fights the
    /// allocator for a variable amount of space.
    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, _collapsed: bool) {
        let width = area.width as usize;
        let mut lines = vec![
            panel_header("MODEL"),
            Line::from(truncate_left(view.model, width)),
        ];
        let provider_line = match view.subagent_model {
            Some(sub) if sub != view.model => {
                format!("{} · sub: {}", view.provider, truncate_left(sub, width / 2))
            }
            _ => view.provider.to_string(),
        };
        lines.push(Line::from(middle_truncate(&provider_line, width)));
        f.render_widget(Paragraph::new(lines), area);
    }
}

/// §9.5's sparkline glyphs, one per `values` entry (oldest first), scaled against the window's
/// own max — a session that never sent more than 500 tokens in one turn shouldn't render every
/// bar as a flat baseline just because some *other* session once sent 50k.
const SPARK_LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn sparkline(values: &[u64], width: usize) -> String {
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let shown = &values[values.len().saturating_sub(width)..];
    let max = shown.iter().copied().max().unwrap_or(0).max(1);
    shown
        .iter()
        .map(|&v| {
            let level =
                ((v as f64 / max as f64) * (SPARK_LEVELS.len() - 1) as f64).round() as usize;
            SPARK_LEVELS[level.min(SPARK_LEVELS.len() - 1)]
        })
        .collect()
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_cost(cost: &CostEstimate) -> String {
    if !cost.known {
        "~$? · unpriced model, see [pricing]".to_string()
    } else {
        format!(
            "~${:.2} · avg ${:.3}/turn",
            cost.total_usd, cost.per_turn_avg
        )
    }
}

impl PanelWidget for ContextWidget {
    fn title(&self) -> &str {
        "CONTEXT"
    }

    fn size(&self, _view: &PanelView<'_>) -> WidgetSize {
        WidgetSize {
            ideal: CONTEXT_ROWS,
            min: CONTEXT_ROWS,
        }
    }

    /// §9.5: sent/received (root + subagents combined, unless `context_split` — `Enter` on
    /// this widget — asks for the two broken out), a `+…` marker mid-stream instead of a fake
    /// live estimate, and the `~`-prefixed cost estimate.
    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, _collapsed: bool) {
        let width = area.width as usize;
        let live_marker = if view.running_turn { " +…" } else { "" };
        let lines = if view.context_split {
            vec![
                panel_header("CONTEXT · root/sub"),
                Line::from(format!(
                    "root  {}↑ {}↓",
                    format_tokens(view.usage.root.input_tokens),
                    format_tokens(view.usage.root.output_tokens)
                )),
                Line::from(format!(
                    "sub   {}↑ {}↓{}",
                    format_tokens(view.usage.subagents.input_tokens),
                    format_tokens(view.usage.subagents.output_tokens),
                    live_marker
                )),
                Line::from(format_cost(&view.cost)),
            ]
        } else {
            let sent = view.usage.root.input_tokens + view.usage.subagents.input_tokens;
            let recv = view.usage.root.output_tokens + view.usage.subagents.output_tokens;
            let spark_width = width.saturating_sub(16);
            let spark = sparkline(&view.usage.per_turn, spark_width);
            vec![
                panel_header("CONTEXT"),
                Line::from(format!(
                    "sent  {}{live_marker}  {spark}",
                    format_tokens(sent)
                )),
                Line::from(format!("recv  {}", format_tokens(recv))),
                Line::from(format_cost(&view.cost)),
            ]
        };
        f.render_widget(Paragraph::new(lines), area);
    }
}

impl PanelWidget for SubagentRosterWidget {
    fn title(&self) -> &str {
        "SUBAGENTS"
    }

    fn size(&self, view: &PanelView<'_>) -> WidgetSize {
        let n = view.subagents.len().min(ROSTER_SHOWN);
        WidgetSize {
            ideal: 1 + n as u16,
            min: if n == 0 { 1 } else { 2 },
        }
    }

    /// §9.6: one line per subagent, hard constraint — `glyph · label · elapsed · activity`,
    /// truncated to whatever's actually left of the row's width after the fixed columns.
    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, collapsed: bool) {
        let shown = area.height.saturating_sub(1) as usize;
        let hidden = view.subagents.len().saturating_sub(shown);
        let running = view
            .subagents
            .iter()
            .filter(|r| !r.status.is_terminal())
            .count();
        let mut header = format!("SUBAGENTS  {running}/{}", view.subagents.len());
        if hidden > 0 {
            header.push_str(&format!(" +{hidden} more"));
        }
        let mut lines = vec![panel_header_marker(&header, collapsed)];
        let focused_row = view.row_focus(PanelWidgetKind::Subagents);
        for (i, row) in view.subagents.iter().take(shown).enumerate() {
            lines.push(subagent_row_line(row, area.width, Some(i) == focused_row));
        }
        f.render_widget(Paragraph::new(lines), area);
    }
}

fn subagent_row_line(row: &SubagentRow, width: u16, focused: bool) -> Line<'static> {
    let elapsed = row.started.elapsed().as_secs();
    let elapsed_text = format!("{}:{:02}", elapsed / 60, elapsed % 60);
    let glyph = row.status.glyph();
    let label_budget = 10usize;
    let label = truncate_end(&row.label, label_budget);
    let suffix = if row.status.is_terminal() {
        format!("{} turns", row.turns)
    } else {
        row.activity.clone()
    };
    let fixed = glyph.chars().count() + 1 + label_budget + 1 + elapsed_text.chars().count() + 1;
    let activity_budget = (width as usize).saturating_sub(fixed).max(1);
    let text = format!(
        "{glyph} {label:<label_budget$} {elapsed_text} {}",
        truncate_end(&suffix, activity_budget)
    );
    let style = if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if row.status == SubagentStatus::AwaitingApproval {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    Line::from(Span::styled(text, style))
}

impl PanelWidget for NetworkLogWidget {
    fn title(&self) -> &str {
        "NETWORK"
    }

    fn size(&self, view: &PanelView<'_>) -> WidgetSize {
        let n = view.network.len().min(NETWORK_SHOWN);
        WidgetSize {
            ideal: 1 + n as u16,
            min: 1,
        }
    }

    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, collapsed: bool) {
        let header = format!("NETWORK · {}↑", view.network_turn_requests);
        let mut lines = vec![panel_header_marker(&header, collapsed)];
        let shown = area.height.saturating_sub(1) as usize;
        let focused_row = view.row_focus(PanelWidgetKind::Network);
        for (i, row) in view.network.iter().take(shown).enumerate() {
            lines.push(net_row_line(row, area.width, Some(i) == focused_row));
        }
        f.render_widget(Paragraph::new(lines), area);
    }
}

impl PanelWidget for DocumentsLogWidget {
    fn title(&self) -> &str {
        "DOCUMENTS"
    }

    fn size(&self, view: &PanelView<'_>) -> WidgetSize {
        let n = view.documents.len().min(DOCUMENTS_SHOWN);
        WidgetSize {
            ideal: 1 + n as u16,
            min: 1,
        }
    }

    fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>, collapsed: bool) {
        let mut lines = vec![panel_header_marker("DOCUMENTS", collapsed)];
        let shown = area.height.saturating_sub(1) as usize;
        let focused_row = view.row_focus(PanelWidgetKind::Documents);
        for (i, row) in view.documents.iter().take(shown).enumerate() {
            lines.push(doc_row_line(
                row,
                view.root,
                area.width,
                Some(i) == focused_row,
            ));
        }
        f.render_widget(Paragraph::new(lines), area);
    }
}

fn panel_header(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))
}

/// A header row with a trailing `▸` when the widget didn't get its full ideal height (§9.2:
/// "renders as its header row with a count... and expands on focus").
fn panel_header_marker(text: &str, collapsed: bool) -> Line<'static> {
    let text = if collapsed {
        format!("{text} ▸")
    } else {
        text.to_string()
    };
    panel_header(&text)
}

/// `200 docs.rs/rig-core 412ms` for a request that reached a server; `BLK 169.254.169.254
/// link-local` for one an SSRF guard refused before it did (§9.7). A subagent's row is
/// prefixed with its id.
fn net_row_line(row: &NetRow, width: u16, focused: bool) -> Line<'static> {
    let (status_text, mut style) = match row.status {
        Some(code) if (200..300).contains(&code) => (
            code.to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
        ),
        Some(code) if (300..400).contains(&code) => (
            code.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Some(code) => (code.to_string(), Style::default().fg(Color::Red)),
        None => ("BLK".to_string(), Style::default().fg(Color::Red)),
    };
    let agent_prefix = subagent_prefix(row.agent);
    let budget = (width as usize)
        .saturating_sub(status_text.len() + agent_prefix.len() + 2)
        .max(1);
    let detail = match &row.reason {
        Some(reason) => middle_truncate(&format!("{} {reason}", row.host), budget),
        None => middle_truncate(&format!("{}{} {}ms", row.host, row.path, row.ms), budget),
    };
    if focused {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Line::from(Span::styled(
        format!("{agent_prefix}{status_text} {detail}"),
        style,
    ))
}

/// `R Cargo.toml 24L` (§9.8) — `R`/`W`/`+`/`−` for read/write/create/delete, path relative to
/// the workspace root and middle-truncated so the filename always survives.
fn doc_row_line(row: &DocRow, root: &Path, width: u16, focused: bool) -> Line<'static> {
    let op = match row.op {
        FileOp::Read => "R",
        FileOp::Write => "W",
        FileOp::Create => "+",
        FileOp::Delete => "−",
    };
    let relative = row.path.strip_prefix(root).unwrap_or(&row.path);
    let agent_prefix = subagent_prefix(row.agent);
    let budget = (width as usize)
        .saturating_sub(op.len() + agent_prefix.len() + 8)
        .max(1);
    let path = middle_truncate(&relative.to_string_lossy(), budget);
    let style = if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(Span::styled(
        format!("{agent_prefix}{op} {path} {}L", row.lines),
        style,
    ))
}

fn subagent_prefix(agent: AgentId) -> String {
    if agent == AgentId::ROOT {
        String::new()
    } else {
        format!("[{}] ", agent.0)
    }
}

/// The vertical-budget allocator (`M12-3`), split out from [`AgentStatusPanel::render`] so it's
/// unit-testable without a `Frame`. `sizes` is `[subagents, network, documents]` regardless of
/// `order` — the result is returned in that same fixed `[subagents, network, documents]`
/// shape, so the caller never has to un-permute it.
///
/// Two passes, both walking `order`: first every widget's floor (`min`), so a higher-priority
/// widget's floor is guaranteed before a lower one gets anything at all; then whatever's left
/// grows each widget toward its `ideal`, same order — so `subagents` (or the focused widget,
/// rotated to the front of `order`) is first to reclaim the room `documents`/`network`
/// collapsing frees up (§9.2's "documents, then network, then subagents" collapse order).
fn allocate_list_heights(
    list_budget: u16,
    order: [PanelWidgetKind; 3],
    sizes: [&WidgetSize; 3],
) -> [u16; 3] {
    let idx = |kind: PanelWidgetKind| match kind {
        PanelWidgetKind::Subagents => 0,
        PanelWidgetKind::Network => 1,
        PanelWidgetKind::Documents => 2,
        _ => unreachable!("only list widgets appear in `order`"),
    };

    let mut heights = [0u16; 3];
    let mut remaining = list_budget;
    for kind in order {
        let i = idx(kind);
        let floor = sizes[i].min.min(remaining);
        heights[i] = floor;
        remaining -= floor;
    }
    for kind in order {
        let i = idx(kind);
        let extra = sizes[i].ideal.saturating_sub(heights[i]).min(remaining);
        heights[i] += extra;
        remaining -= extra;
    }
    heights
}

/// The panel's widget stack (§9.2). Constructed once per frame in `crate::ui::render_panel` —
/// cheap, since every widget is a stateless unit struct.
pub(crate) struct AgentStatusPanel {
    widgets: Vec<Box<dyn PanelWidget>>,
}

impl AgentStatusPanel {
    pub(crate) fn new() -> Self {
        Self {
            widgets: vec![
                Box::new(ModelWidget),
                Box::new(ContextWidget),
                Box::new(SubagentRosterWidget),
                Box::new(NetworkLogWidget),
                Box::new(DocumentsLogWidget),
            ],
        }
    }

    /// Renders the full stack into `area` (§9.2/§9.3): `Model`/`Context` get their fixed rows
    /// first, the remainder is allocated to the three list widgets in priority order — whichever
    /// is focused first (if any), then subagents, network, documents — each getting
    /// `min(ideal, remaining)`, so a widget that can't fit its ideal still renders as many rows
    /// as the leftover budget allows rather than jumping straight to its floor.
    pub(crate) fn render(&self, f: &mut Frame<'_>, area: Rect, view: &PanelView<'_>) {
        let fixed_total = MODEL_ROWS + CONTEXT_ROWS;
        let fixed_height = fixed_total.min(area.height);
        let list_budget = area.height.saturating_sub(fixed_height);

        let sizes: Vec<WidgetSize> = self.widgets.iter().map(|w| w.size(view)).collect();

        let mut order = [
            PanelWidgetKind::Subagents,
            PanelWidgetKind::Network,
            PanelWidgetKind::Documents,
        ];
        if let Some(focus) = view.focus
            && let Some(pos) = order.iter().position(|k| *k == focus.widget)
        {
            order[..=pos].rotate_right(1);
        }

        let heights = allocate_list_heights(list_budget, order, [&sizes[2], &sizes[3], &sizes[4]]);

        let constraints = [
            Constraint::Length(MODEL_ROWS.min(area.height)),
            Constraint::Length(CONTEXT_ROWS.min(fixed_height.saturating_sub(MODEL_ROWS))),
            Constraint::Length(heights[0]),
            Constraint::Length(heights[1]),
            Constraint::Length(heights[2]),
        ];
        let areas = Layout::vertical(constraints).split(area);

        for (i, widget) in self.widgets.iter().enumerate() {
            let widget_area = areas[i];
            if widget_area.height == 0 {
                continue;
            }
            let collapsed = widget_area.height < sizes[i].ideal;
            widget.render(f, widget_area, view, collapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_produces_one_glyph_per_value_scaled_to_the_windows_own_max() {
        let bars = sparkline(&[0, 50, 100], 3);
        assert_eq!(bars.chars().count(), 3);
        assert_eq!(
            bars.chars().last(),
            Some('█'),
            "the max value must hit the top level"
        );
    }

    #[test]
    fn sparkline_only_shows_the_trailing_window() {
        let bars = sparkline(&[1, 2, 3, 4, 5], 2);
        assert_eq!(
            bars.chars().count(),
            2,
            "must clip to `width`, not render every sample"
        );
    }

    #[test]
    fn kind_cycles_forward_and_back_without_leaving_the_five_variants() {
        let mut k = PanelWidgetKind::Model;
        for _ in 0..5 {
            k = k.next();
        }
        assert_eq!(
            k,
            PanelWidgetKind::Model,
            "five `next`s from Model must return to Model"
        );
        assert_eq!(PanelWidgetKind::Model.prev(), PanelWidgetKind::Documents);
    }

    #[test]
    fn format_tokens_uses_k_and_m_suffixes_above_their_thresholds() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(2_000_000), "2.0m");
    }

    const DEFAULT_ORDER: [PanelWidgetKind; 3] = [
        PanelWidgetKind::Subagents,
        PanelWidgetKind::Network,
        PanelWidgetKind::Documents,
    ];

    #[test]
    fn everything_gets_its_ideal_when_the_budget_is_generous() {
        let subagents = WidgetSize { ideal: 4, min: 2 };
        let network = WidgetSize { ideal: 6, min: 1 };
        let documents = WidgetSize { ideal: 6, min: 1 };
        let heights = allocate_list_heights(30, DEFAULT_ORDER, [&subagents, &network, &documents]);
        assert_eq!(
            heights,
            [4, 6, 6],
            "M12-3: at 40 total rows everything must render in full"
        );
    }

    #[test]
    fn subagents_still_render_when_the_budget_is_tight() {
        // 20-row panel, 7 rows already spent on Model+Context — 13 left for the three lists.
        let subagents = WidgetSize { ideal: 9, min: 2 };
        let network = WidgetSize { ideal: 7, min: 1 };
        let documents = WidgetSize { ideal: 7, min: 1 };
        let heights = allocate_list_heights(13, DEFAULT_ORDER, [&subagents, &network, &documents]);
        assert!(
            heights[0] >= subagents.min,
            "M12-3: at 20 total rows the subagent roster must still render, never collapse to 0"
        );
    }

    #[test]
    fn documents_collapses_before_network_and_network_before_subagents() {
        let subagents = WidgetSize { ideal: 9, min: 2 };
        let network = WidgetSize { ideal: 7, min: 1 };
        let documents = WidgetSize { ideal: 7, min: 1 };
        // Only enough room for the highest-priority widget's own floor.
        let heights = allocate_list_heights(2, DEFAULT_ORDER, [&subagents, &network, &documents]);
        assert_eq!(
            heights,
            [2, 0, 0],
            "subagents (highest priority) must be the only one to get anything"
        );
    }

    #[test]
    fn a_focused_widget_jumps_to_the_front_of_the_priority_order() {
        let subagents = WidgetSize { ideal: 9, min: 2 };
        let network = WidgetSize { ideal: 7, min: 1 };
        let documents = WidgetSize { ideal: 7, min: 1 };
        let order = [
            PanelWidgetKind::Documents,
            PanelWidgetKind::Subagents,
            PanelWidgetKind::Network,
        ];
        // One row available — normally subagents' floor would claim it; a focused Documents
        // must claim it instead.
        let heights = allocate_list_heights(1, order, [&subagents, &network, &documents]);
        assert_eq!(
            heights,
            [0, 0, 1],
            "whichever kind leads `order` must be the one that wins the scarce budget"
        );
    }

    #[test]
    fn format_cost_reports_the_unpriced_hint_rather_than_a_number_that_looks_real() {
        let unknown = CostEstimate {
            total_usd: 0.0,
            per_turn_avg: 0.0,
            known: false,
        };
        assert!(format_cost(&unknown).starts_with("~$?"));
    }
}
