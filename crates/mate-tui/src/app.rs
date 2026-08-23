//! App state and the event loop: `select!`s over terminal input and every open session's
//! `SessionEvent`s, redrawing at ~30 fps only when something actually changed (`M7-2`).
//!
//! `M8` turns the single session `M7` drove into a tab list. [`SessionTab`] is `M8-2`'s
//! per-session view state — transcript, wrap cache, input draft, scroll position, and the
//! streaming/unread/needs-attention flags the tab bar reads (`M8-4`) — one instance per open
//! tab, so switching the active tab is nothing more than pointing [`App::view`] at a different
//! index. [`App::on_session_event`] now routes by `event.session` to the matching tab instead
//! of assuming there's only one, and marks a background tab unread (or, on an error,
//! needs-attention) instead of touching the transcript the user is actually looking at.
//!
//! `Ctrl+T` (`M8-3`) opens [`SpawnForm`], a small modal that captures a workspace root, model
//! override, and http toggle, then calls [`crate::session_factory::build_spec`] the same way
//! `mate-cli`'s startup wiring does for the first tab. `Ctrl+W` closes the active tab, with a
//! one-more-press confirm if it's mid-turn (`SessionManager::close` cancels it). `Ctrl+C`'s
//! existing double-press quit (`M7`) now shuts every open tab down on the way out, not just
//! one — see [`run`].
//!
//! `Ctrl+B` toggles each tab's [`crate::panel::Panel`] plus the full `M12` [`AgentStatusPanel`](
//! crate::panel_widgets::AgentStatusPanel) it feeds: `network`/`documents` ring buffers and the
//! [`crate::roster::Roster`] are folded straight out of `AgentEvent::Activity`/`SubagentSpawned`/
//! `SubagentFinished`/`Usage`/`ApprovalRequired` in [`App::on_session_event`] — every one of
//! those is routed regardless of which agent produced it, root or subagent, since the panel
//! shows the whole session's activity (§9.3) even though the transcript itself stays root-only
//! (§9.9).
//!
//! `Ctrl+P` (`M12-9`) focuses the panel: `Tab`/`Shift+Tab` cycles which widget has focus,
//! `↑`/`↓` moves the focused row of whichever list widget (subagents/network/documents/skills)
//! that is, `Enter` opens a read-only [`DetailModal`] for that row (or, on the context widget,
//! toggles its root/subagent split), `x` cancels the focused subagent, and any printable key
//! returns focus to the input and is inserted there — there is no code path that routes text
//! into a subagent (§7.6).

use std::io;
use std::path::PathBuf;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use mate_core::cost::{ModelRate, estimate_cost};
use mate_core::session::{SessionCmd, SessionEvent, SessionHandle, SessionId, SessionManager};
use mate_core::streaming::{AgentEvent, UsageRollup};
use mate_tool_api::{AgentId, SkillMetadata};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;
use tokio::time::{Duration, Interval, MissedTickBehavior, interval};
use ulid::Ulid;

use crate::input::InputBox;
use crate::panel::Panel;
use crate::panel_widgets::{PanelFocus, PanelWidgetKind};
use crate::roster::Roster;
use crate::session_factory::{self, SessionDefaults};
use crate::slash::SlashCommand;
use crate::transcript::Transcript;
use crate::ui::{self, AppView};
use crate::wrap::WrapCache;

const TICK: Duration = Duration::from_millis(33);

/// Lines the transcript scrolls per mouse wheel notch — matches the step most terminals use for
/// a keyboard `PageUp`/`PageDown`-style nudge rather than a single line, so a wheel click feels
/// like it actually moved something.
const SCROLL_STEP: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One already-spawned session handed to [`run`] at startup (`M8-5`: one per `-C` root).
pub struct InitialSession {
    pub handle: SessionHandle,
    pub session_id: SessionId,
    pub title: String,
    pub model: String,
    pub provider: String,
    /// The session's workspace root — the documents log (§9.8) renders file paths relative to
    /// this rather than the absolute paths `ToolActivity::FileTouched` carries.
    pub root: PathBuf,
    /// The `MODEL` widget's third line (§9.4) — `None` when delegation is off.
    pub subagent_model: Option<String>,
    /// Whether this tab's agent actually has `http_request` attached — `/tools`/`/http`
    /// (`M13-3`) report this, not `SessionDefaults`, since a tab's own toolset can differ from
    /// the defaults it started from (the spawn form's http toggle).
    pub http_enabled: bool,
    /// Whether this tab's root agent actually has `spawn_agent` attached.
    pub may_delegate: bool,
    /// Skills discovered for this session's workspace root — the same list its
    /// `ToolCtx::skills` was built from, so the SKILLS widget's catalog matches the `skill`
    /// tool's own inventory exactly.
    pub skills: Vec<SkillMetadata>,
    /// The project-instructions filename discovered for this session's workspace root
    /// (`AGENTS.md`, `CLAUDE.md`, ...), if any — the `PROJECT` widget's tick.
    pub agents_md: Option<String>,
}

/// A read-only detail popup (`M12-9`'s `Enter` on a focused panel row) — one flavor for
/// whichever list widget was focused, built once at open time from that row's already-known
/// fields. Never re-reads anything live (no re-fetch, no new tool call): the panel only ever
/// shows what already streamed through, and this modal is no exception.
pub(crate) struct DetailModal {
    pub(crate) title: String,
    pub(crate) lines: Vec<String>,
}

/// One outstanding approval request (`M13-2`), queued per tab — `Enter`/`y`/`n` on the front of
/// the queue is the whole interaction surface (§7.4: binary, no free text). `agent` is who
/// asked, root or a subagent, so the modal can label it distinctly.
pub(crate) struct PendingApproval {
    pub(crate) id: Ulid,
    pub(crate) agent: AgentId,
    pub(crate) name: String,
    pub(crate) detail: String,
}

/// One tab's live state (`M8-2`). Everything that was a flat field on `App` before `M8` — the
/// transcript, the wrap cache, the input draft, scroll position — moved here unchanged; the
/// only things `App` still owns directly are the manager, the pricing table, and which tab is
/// active.
struct SessionTab {
    id: SessionId,
    handle: SessionHandle,
    title: String,
    model: String,
    provider: String,
    subagent_model: Option<String>,
    root: PathBuf,
    transcript: Transcript,
    wrap: WrapCache,
    input: InputBox,
    scroll: usize,
    running_turn: bool,
    /// Root + subagent usage, split, with the sparkline's bounded per-turn history (§9.5,
    /// `M11-5`) — the source both the bottom status bar and the `CONTEXT` widget read from.
    usage: UsageRollup,
    /// `Enter` on the `CONTEXT` widget (§9.5) toggles this.
    context_split: bool,
    /// Set when a background tab produces visible activity (`M8-4`); cleared on switching to it.
    unread: bool,
    /// Set when a background tab errors (`M8-4`); cleared on switching to it. Takes marker
    /// priority over `unread` — an error is worth more attention than "something happened".
    needs_attention: bool,
    /// Network/documents activity logs (§9.7/§9.8), folded from `AgentEvent::Activity` — one
    /// panel per tab, never shared across tabs (§9.1).
    panel: Panel,
    /// The subagent roster (§9.6), folded from `SubagentSpawned`/`Finished`/`Activity`/`Usage`/
    /// `ApprovalRequired` tagged with a non-root `AgentId`.
    roster: Roster,
    /// `Ctrl+B` toggle, per tab so a spawn form's freshly-opened tab always starts visible
    /// regardless of what another tab's user preference is (§9.1: panel state is per-session).
    panel_visible: bool,
    /// `Ctrl+P`/`Tab`/arrows (`M12-9`) — `None` when the panel doesn't have input focus.
    panel_focus: Option<PanelFocus>,
    /// `Enter` on a focused list row (`M12-9`) — read-only, closed by `Esc` or `Enter` again.
    detail_modal: Option<DetailModal>,
    /// `M13-2`'s approval queue — front is what `App::view` renders and `handle_approval_key`
    /// decides; anything behind it just waits.
    pending_approvals: VecDeque<PendingApproval>,
    /// `/tools`/`/http` (`M13-3`) read these rather than `SessionDefaults`, since a tab's actual
    /// toolset can diverge from the defaults it was spawned from.
    http_enabled: bool,
    may_delegate: bool,
    /// The `PROJECT` widget's tick — set once at tab-open time from `ToolCtx::agents_md`, never
    /// re-derived (the same "fixed for the tab's lifetime" treatment `skills`' catalog gets).
    agents_md: Option<String>,
}

impl SessionTab {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: SessionId,
        handle: SessionHandle,
        title: String,
        model: String,
        provider: String,
        subagent_model: Option<String>,
        root: PathBuf,
        http_enabled: bool,
        may_delegate: bool,
        skills: Vec<SkillMetadata>,
        agents_md: Option<String>,
    ) -> Self {
        Self {
            id,
            handle,
            title,
            model,
            provider,
            subagent_model,
            root,
            transcript: Transcript::new(),
            wrap: WrapCache::new(),
            input: InputBox::new(),
            scroll: 0,
            running_turn: false,
            usage: UsageRollup::default(),
            context_split: false,
            unread: false,
            needs_attention: false,
            panel: Panel::new(skills),
            roster: Roster::default(),
            panel_visible: true,
            panel_focus: None,
            detail_modal: None,
            pending_approvals: VecDeque::new(),
            http_enabled,
            may_delegate,
            agents_md,
        }
    }
}

/// Which field of [`SpawnForm`] currently has focus. `pub(crate)` so `ui.rs` can match on it
/// without `mate-tui` inventing a second copy of the same three-way choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnField {
    Dir,
    Model,
    Http,
}

/// `Ctrl+T`'s spawn form (`M8-3`): a workspace root, an optional model override, and an http
/// on/off toggle — the three knobs worth exposing per tab without building a full settings UI.
/// Everything else a new session needs comes from [`SessionDefaults`].
struct SpawnForm {
    dir: String,
    model: String,
    http_enabled: bool,
    focus: SpawnField,
    error: Option<String>,
}

impl SpawnForm {
    fn new() -> Self {
        Self {
            dir: String::new(),
            model: String::new(),
            http_enabled: true,
            focus: SpawnField::Dir,
            error: None,
        }
    }

    fn next_field(&mut self) {
        self.focus = match self.focus {
            SpawnField::Dir => SpawnField::Model,
            SpawnField::Model => SpawnField::Http,
            SpawnField::Http => SpawnField::Dir,
        };
    }

    fn prev_field(&mut self) {
        self.focus = match self.focus {
            SpawnField::Dir => SpawnField::Http,
            SpawnField::Model => SpawnField::Dir,
            SpawnField::Http => SpawnField::Model,
        };
    }

    fn push_char(&mut self, c: char) {
        match self.focus {
            SpawnField::Dir => self.dir.push(c),
            SpawnField::Model => self.model.push(c),
            SpawnField::Http => {}
        }
    }

    fn backspace(&mut self) {
        match self.focus {
            SpawnField::Dir => {
                self.dir.pop();
            }
            SpawnField::Model => {
                self.model.pop();
            }
            SpawnField::Http => {}
        }
    }
}

/// Derives a tab title from the prompt just sent — the tab bar's `title` is meant to name the
/// task in flight, so it's refreshed on every prompt rather than only the first. Falls back to
/// `None` (keeping the previous title) for a prompt that's blank once trimmed; the display side
/// (`ui::truncate_title`) handles anything longer than fits.
fn task_title(prompt: &str) -> Option<String> {
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();
    (!first_line.is_empty()).then(|| first_line.to_string())
}

pub struct App {
    manager: SessionManager,
    events: mpsc::Receiver<SessionEvent>,
    defaults: SessionDefaults,
    /// `[pricing]` (§9.5, `M11-6`), converted once from `mate-cli`'s TOML-facing config shape —
    /// shared across every tab, the same table regardless of which session asks.
    pricing: HashMap<String, ModelRate>,
    tabs: Vec<SessionTab>,
    active: usize,
    quit_armed: bool,
    close_confirm: bool,
    dirty: bool,
    should_quit: bool,
    spawn_form: Option<SpawnForm>,
}

impl App {
    fn new(
        manager: SessionManager,
        events: mpsc::Receiver<SessionEvent>,
        sessions: Vec<InitialSession>,
        defaults: SessionDefaults,
        pricing: HashMap<String, ModelRate>,
    ) -> Self {
        let tabs = sessions
            .into_iter()
            .map(|s| {
                SessionTab::new(
                    s.session_id,
                    s.handle,
                    s.title,
                    s.model,
                    s.provider,
                    s.subagent_model,
                    s.root,
                    s.http_enabled,
                    s.may_delegate,
                    s.skills,
                    s.agents_md,
                )
            })
            .collect();
        Self {
            manager,
            events,
            defaults,
            pricing,
            tabs,
            active: 0,
            quit_armed: false,
            close_confirm: false,
            dirty: true,
            should_quit: false,
            spawn_form: None,
        }
    }

    fn view(&mut self) -> AppView<'_> {
        let tabs: Vec<ui::TabSummary> = self
            .tabs
            .iter()
            .map(|t| ui::TabSummary {
                title: t.title.clone(),
                streaming: t.running_turn,
                unread: t.unread,
                needs_attention: t.needs_attention,
            })
            .collect();
        let active = self.active;
        let quit_armed = self.quit_armed;
        let close_confirm = self.close_confirm;
        let spawn_form = self.spawn_form.as_ref().map(|form| ui::SpawnFormView {
            dir: &form.dir,
            model: &form.model,
            http_enabled: form.http_enabled,
            focus: form.focus,
            error: form.error.as_deref(),
        });
        let tab = &mut self.tabs[active];
        let cost = estimate_cost(
            &tab.usage,
            &tab.model,
            tab.subagent_model.as_deref().unwrap_or(&tab.model),
            &self.pricing,
        );
        let detail_modal = tab.detail_modal.as_ref().map(|m| ui::DetailModalView {
            title: &m.title,
            lines: &m.lines,
        });
        let approval_modal = tab
            .pending_approvals
            .front()
            .map(|a| ui::ApprovalModalView {
                agent_label: if a.agent == AgentId::ROOT {
                    "mate"
                } else {
                    tab.roster
                        .rows()
                        .iter()
                        .find(|r| r.id == a.agent)
                        .map(|r| r.label.as_str())
                        .unwrap_or("a subagent")
                },
                name: &a.name,
                detail: &a.detail,
                queued: tab.pending_approvals.len().saturating_sub(1),
            });
        AppView {
            tabs,
            active,
            spawn_form,
            detail_modal,
            approval_modal,
            session: ui::View {
                transcript: &tab.transcript,
                wrap: &mut tab.wrap,
                input: &tab.input,
                root: &tab.root,
                panel_visible: tab.panel_visible,
                subagents: tab.roster.rows(),
                network: &tab.panel.network,
                documents: &tab.panel.documents,
                network_turn_requests: tab.panel.turn_requests,
                skills: &tab.panel.skills,
                agents_md: tab.agents_md.as_deref(),
                scroll: &mut tab.scroll,
                running_turn: tab.running_turn,
                model: &tab.model,
                provider: &tab.provider,
                subagent_model: tab.subagent_model.as_deref(),
                usage: &tab.usage,
                cost,
                context_split: tab.context_split,
                panel_focus: tab.panel_focus,
                quit_armed,
                close_confirm,
            },
        }
    }

    /// Routes one session's event to its tab (`M8-2`), regardless of which tab is active.
    /// Background activity marks the tab unread; a background error marks it needing
    /// attention. `AgentEvent::Activity` is the one exception to "root agent only" below — the
    /// panel shows the whole session's tool activity, root and subagents together (§9.3), even
    /// though the transcript itself stays root-only (§9.9: subagent chatter never inlines).
    fn on_session_event(&mut self, event: SessionEvent) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == event.session) else {
            return;
        };
        self.dirty = true;
        let is_active = idx == self.active;
        let tab = &mut self.tabs[idx];

        // Every event kind that isn't root-only (§9.9): the panel — network/documents logs and
        // the subagent roster alike — shows the whole session's activity, root and subagents
        // together (§9.3), even though the transcript stays root-only below. Each arm returns,
        // same shape the original `Activity`-only special case had.
        match &event.event {
            AgentEvent::Activity(record) => {
                if event.agent != AgentId::ROOT {
                    tab.roster.note_activity(event.agent, record);
                }
                tab.panel.push(event.agent, record.clone());
                if !is_active {
                    tab.unread = true;
                }
                return;
            }
            AgentEvent::SubagentSpawned { id, label, .. } => {
                tab.roster.spawn(*id, label.clone());
                if !is_active {
                    tab.unread = true;
                }
                return;
            }
            AgentEvent::SubagentFinished { id, outcome } => {
                tab.roster.finish(*id, outcome);
                if !is_active {
                    tab.unread = true;
                }
                return;
            }
            // `M13-2`: queued regardless of which agent asked — root and subagent requests
            // share one per-tab queue, rendered as a modal only while this tab is active
            // (`App::view`). A subagent's request also updates its roster row, since that's
            // the only place a *background subagent's* pending approval is visible at all.
            AgentEvent::ApprovalRequired { id, name, detail } => {
                if event.agent != AgentId::ROOT {
                    tab.roster.awaiting_approval(event.agent);
                }
                tab.pending_approvals.push_back(PendingApproval {
                    id: *id,
                    agent: event.agent,
                    name: name.clone(),
                    detail: detail.clone(),
                });
                if !is_active {
                    tab.needs_attention = true;
                }
                return;
            }
            AgentEvent::Usage(usage) if event.agent != AgentId::ROOT => {
                tab.usage.record_subagent_turn(*usage);
                tab.roster.record_turn(event.agent);
                if !is_active {
                    tab.unread = true;
                }
                return;
            }
            _ => {}
        }

        if event.agent != AgentId::ROOT {
            return;
        }

        let mut activity = false;
        let evicted = match event.event {
            AgentEvent::Token(text) => {
                activity = true;
                let push = tab.transcript.push_token(&text);
                tab.wrap.invalidate(push.changed);
                push.evicted
            }
            AgentEvent::ToolCallStarted { name } => {
                activity = true;
                tab.transcript.push_tool_call(name)
            }
            AgentEvent::ToolResult { name, ok, summary } => {
                activity = true;
                tab.transcript.resolve_tool_call(&name, ok, summary);
                None
            }
            AgentEvent::TurnComplete => {
                activity = true;
                tab.transcript.end_turn();
                tab.running_turn = false;
                None
            }
            AgentEvent::Error(text) => {
                tab.transcript.push_error(text);
                tab.running_turn = false;
                if !is_active {
                    tab.needs_attention = true;
                }
                None
            }
            AgentEvent::Usage(usage) => {
                tab.usage.record_root_turn(usage);
                None
            }
            // Handled unconditionally above, before the root-only gate — every
            // `ApprovalRequired`, root or subagent, returns from that arm.
            AgentEvent::ApprovalRequired { .. } => {
                unreachable!("handled above, before the root-only gate")
            }
            // Always tagged with a non-root `AgentId` (`crate::subagent::drive_subagent`), so
            // these never reach here — the arms above already returned. Kept for exhaustiveness.
            AgentEvent::SubagentSpawned { .. } | AgentEvent::SubagentFinished { .. } => None,
            AgentEvent::Activity(_) => unreachable!("handled above, before the root-only gate"),
        };
        if activity && !is_active {
            tab.unread = true;
        }
        if let Some(id) = evicted {
            tab.wrap.invalidate(id);
        }
    }

    /// `Ctrl+C`, `Ctrl+W`, `Ctrl+O`, `Ctrl+T`, `Ctrl+G`, `Ctrl+←/→`, and `Alt+1..9` are
    /// intercepted here, ahead of the active tab's input box, since they're app- or
    /// tab-management commands rather than editing keys the textarea should ever see.
    async fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.dirty = true;

        if self.tabs.is_empty() {
            return;
        }

        if self.spawn_form.is_some() {
            self.handle_spawn_form_key(key).await;
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            if self.tabs[self.active].running_turn {
                let _ = self.tabs[self.active].handle.send(SessionCmd::Cancel).await;
            } else if self.quit_armed {
                self.should_quit = true;
            } else {
                self.quit_armed = true;
            }
            return;
        }
        self.quit_armed = false;

        if ctrl && matches!(key.code, KeyCode::Char('w' | 'W')) {
            self.request_close_active().await;
            return;
        }
        self.close_confirm = false;

        if ctrl && matches!(key.code, KeyCode::Char('o' | 'O')) {
            self.tabs[self.active]
                .transcript
                .toggle_last_tool_expanded();
            return;
        }

        if ctrl && matches!(key.code, KeyCode::Char('t' | 'T')) {
            self.spawn_form = Some(SpawnForm::new());
            return;
        }

        if ctrl && matches!(key.code, KeyCode::Char('g' | 'G')) {
            if let Some(idx) = self.next_attention_tab() {
                self.switch_to(idx);
            }
            return;
        }

        if ctrl && matches!(key.code, KeyCode::Char('b' | 'B')) {
            let visible = &mut self.tabs[self.active].panel_visible;
            *visible = !*visible;
            return;
        }

        if ctrl && matches!(key.code, KeyCode::Char('p' | 'P')) {
            self.toggle_panel_focus();
            return;
        }

        if ctrl && matches!(key.code, KeyCode::Left) {
            let len = self.tabs.len();
            self.switch_to((self.active + len - 1) % len);
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Right) {
            let len = self.tabs.len();
            self.switch_to((self.active + 1) % len);
            return;
        }

        if alt
            && let KeyCode::Char(c) = key.code
            && let Some(d) = c.to_digit(10)
            && (1..=9).contains(&d)
        {
            self.switch_to((d as usize) - 1);
            return;
        }

        // `M13-2`: an open approval request takes priority over everything below — including
        // the detail modal and panel focus — since a decision blocks the calling tool's turn.
        if !self.tabs[self.active].pending_approvals.is_empty() {
            self.handle_approval_key(key).await;
            return;
        }

        // `M12-9`: the modal takes priority when open, then panel focus — a printable key with
        // no panel-specific meaning clears focus and falls through to the input box below,
        // rather than being swallowed, so the character the user typed still lands (§9.12).
        if self.tabs[self.active].detail_modal.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.tabs[self.active].detail_modal = None;
            }
            return;
        }

        if self.tabs[self.active].panel_focus.is_some() && self.handle_panel_key(key).await {
            return;
        }

        if let Some(prompt) = self.tabs[self.active].input.on_key(key) {
            // `M13-3`: parsed and fully dispatched before anything below ever considers sending
            // a turn — an unknown command still never reaches the model (its own `Unknown` arm
            // just prints feedback), and a known command never falls through to `SessionCmd::
            // Prompt` at all.
            if let Some(command) = crate::slash::parse(&prompt) {
                self.handle_slash_command(command).await;
                return;
            }
            // §5.2: one turn at a time — a prompt sent while another is in flight is dropped
            // by the session task anyway, so don't even send it, and keep the draft the user
            // typed instead of losing it.
            if !self.tabs[self.active].running_turn {
                self.tabs[self.active].transcript.push_user(prompt.clone());
                self.tabs[self.active].running_turn = true;
                self.tabs[self.active].panel.reset_turn();
                if let Some(title) = task_title(&prompt) {
                    self.tabs[self.active].title = title;
                }
                let _ = self.tabs[self.active]
                    .handle
                    .send(SessionCmd::Prompt(prompt))
                    .await;
            }
        }
    }

    /// `Ctrl+P` (`M12-9`): focuses the panel on `ModelWidget` if nothing was focused, or
    /// releases focus back to the input if something already was — a toggle, matching `Ctrl+B`
    /// right above it. Focusing also makes sure the panel is actually visible; focusing a
    /// hidden panel would be pointless.
    fn toggle_panel_focus(&mut self) {
        let tab = &mut self.tabs[self.active];
        if tab.panel_focus.is_some() {
            tab.panel_focus = None;
        } else {
            tab.panel_focus = Some(PanelFocus {
                widget: PanelWidgetKind::Model,
                row: 0,
            });
            tab.panel_visible = true;
        }
    }

    /// Rows actually shown for a list widget right now — the same caps
    /// `crate::panel_widgets` renders against — so `↑`/`↓` can't walk focus past what's on
    /// screen.
    fn panel_row_count(&self, idx: usize, widget: PanelWidgetKind) -> usize {
        let tab = &self.tabs[idx];
        match widget {
            PanelWidgetKind::Subagents => tab.roster.len().min(crate::roster::ROSTER_SHOWN),
            PanelWidgetKind::Network => tab.panel.network.len().min(6),
            PanelWidgetKind::Documents => tab.panel.documents.len().min(6),
            PanelWidgetKind::Skills => tab.panel.skills.len().min(6),
            PanelWidgetKind::Model | PanelWidgetKind::Context | PanelWidgetKind::AgentsMd => 0,
        }
    }

    /// Handles one key while the active tab's panel has focus (`M12-9`). Returns `true` if the
    /// key was consumed — `false` only for a plain printable character, which clears focus and
    /// is left for the caller to hand to the input box instead, so the keystroke isn't lost.
    async fn handle_panel_key(&mut self, key: KeyEvent) -> bool {
        let idx = self.active;
        let Some(focus) = self.tabs[idx].panel_focus else {
            return false;
        };

        match key.code {
            KeyCode::Esc => {
                self.tabs[idx].panel_focus = None;
                true
            }
            KeyCode::Tab => {
                self.tabs[idx].panel_focus = Some(PanelFocus {
                    widget: focus.widget.next(),
                    row: 0,
                });
                true
            }
            KeyCode::BackTab => {
                self.tabs[idx].panel_focus = Some(PanelFocus {
                    widget: focus.widget.prev(),
                    row: 0,
                });
                true
            }
            KeyCode::Up if focus.widget.is_list() => {
                self.tabs[idx].panel_focus = Some(PanelFocus {
                    widget: focus.widget,
                    row: focus.row.saturating_sub(1),
                });
                true
            }
            KeyCode::Down if focus.widget.is_list() => {
                let max = self.panel_row_count(idx, focus.widget).saturating_sub(1);
                self.tabs[idx].panel_focus = Some(PanelFocus {
                    widget: focus.widget,
                    row: (focus.row + 1).min(max),
                });
                true
            }
            KeyCode::Enter => {
                self.activate_panel_focus(idx, focus);
                true
            }
            KeyCode::Char('x' | 'X') if focus.widget == PanelWidgetKind::Subagents => {
                self.cancel_focused_subagent(idx, focus).await;
                true
            }
            KeyCode::Char(_) => {
                // §9.12: any other printable key returns focus to the input and is inserted
                // there — never routed anywhere near a subagent (§7.6).
                self.tabs[idx].panel_focus = None;
                false
            }
            _ => true,
        }
    }

    /// `Enter` on a focused panel row (`M12-9`): toggles the context widget's split, or opens a
    /// read-only [`DetailModal`] built from that row's already-known fields — never a live
    /// re-fetch.
    fn activate_panel_focus(&mut self, idx: usize, focus: PanelFocus) {
        match focus.widget {
            PanelWidgetKind::Model | PanelWidgetKind::AgentsMd => {}
            PanelWidgetKind::Context => {
                let split = &mut self.tabs[idx].context_split;
                *split = !*split;
            }
            PanelWidgetKind::Subagents => {
                let Some(row) = self.tabs[idx].roster.rows().get(focus.row) else {
                    return;
                };
                let elapsed = row.started.elapsed().as_secs();
                self.tabs[idx].detail_modal = Some(DetailModal {
                    title: format!("subagent · {}", row.label),
                    lines: vec![
                        format!("status: {:?}", row.status),
                        format!("elapsed: {}:{:02}", elapsed / 60, elapsed % 60),
                        format!("turns: {}", row.turns),
                        format!("activity: {}", row.activity),
                    ],
                });
            }
            PanelWidgetKind::Network => {
                let Some(row) = self.tabs[idx].panel.network.get(focus.row) else {
                    return;
                };
                let mut lines = vec![format!("host: {}", row.host), format!("path: {}", row.path)];
                match &row.reason {
                    Some(reason) => lines.push(format!("blocked: {reason}")),
                    None => {
                        let status = row
                            .status
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "—".to_string());
                        lines.push(format!("status: {status}"));
                        lines.push(format!("duration: {}ms", row.ms));
                    }
                }
                self.tabs[idx].detail_modal = Some(DetailModal {
                    title: "request".to_string(),
                    lines,
                });
            }
            PanelWidgetKind::Documents => {
                let Some(row) = self.tabs[idx].panel.documents.get(focus.row) else {
                    return;
                };
                self.tabs[idx].detail_modal = Some(DetailModal {
                    title: "document".to_string(),
                    lines: vec![
                        format!("path: {}", row.path.display()),
                        format!("op: {:?}", row.op),
                        format!("lines: {}", row.lines),
                    ],
                });
            }
            PanelWidgetKind::Skills => {
                let Some(row) = self.tabs[idx].panel.skills.get(focus.row) else {
                    return;
                };
                self.tabs[idx].detail_modal = Some(DetailModal {
                    title: format!("skill · {}", row.name),
                    lines: vec![
                        format!(
                            "status: {}",
                            if row.active { "active" } else { "not loaded" }
                        ),
                        format!("description: {}", row.description),
                    ],
                });
            }
        }
    }

    /// `x` on a focused subagent row (`M12-9`): routed through `SessionCmd::CancelSubagent`,
    /// never `Cancel` — this must stop only that one subagent, not the root turn.
    async fn cancel_focused_subagent(&mut self, idx: usize, focus: PanelFocus) {
        let Some(id) = self.tabs[idx]
            .roster
            .rows()
            .get(focus.row)
            .map(|row| row.id)
        else {
            return;
        };
        let _ = self.tabs[idx]
            .handle
            .send(SessionCmd::CancelSubagent(id))
            .await;
    }

    /// `y`/`n`/`Esc` on the front of the active tab's approval queue (`M13-2`). Any other key
    /// is ignored (the queue stays exactly as it was) rather than falling through anywhere
    /// else — while a decision is pending, no other key has a meaning here.
    async fn handle_approval_key(&mut self, key: KeyEvent) {
        let idx = self.active;
        let granted = match key.code {
            KeyCode::Char('y' | 'Y') => true,
            KeyCode::Char('n' | 'N') | KeyCode::Esc => false,
            _ => return,
        };
        let Some(approval) = self.tabs[idx].pending_approvals.pop_front() else {
            return;
        };
        let _ = self.tabs[idx]
            .handle
            .send(SessionCmd::Approve {
                id: approval.id,
                granted,
            })
            .await;
    }

    /// Appends one line of local command feedback to the active tab (`M13-3`) — never sent to
    /// the model, and rendered distinctly from `SystemError` (§ `Entry::System`'s own doc).
    fn push_system(&mut self, text: impl Into<String>) {
        let tab = &mut self.tabs[self.active];
        if let Some(evicted) = tab.transcript.push_system(text.into()) {
            tab.wrap.invalidate(evicted);
        }
    }

    /// `/` command dispatch (`M13-3`), parsed by [`crate::slash::parse`] before this is ever
    /// called — every arm here either performs a local action or writes one `push_system` line,
    /// and none of them can reach `SessionCmd::Prompt`.
    async fn handle_slash_command(&mut self, command: SlashCommand) {
        match command {
            SlashCommand::New(dir) => self.spawn_tab_from_command(dir).await,
            SlashCommand::Close => self.request_close_active().await,
            SlashCommand::Rename(Some(name)) => self.tabs[self.active].title = name,
            SlashCommand::Rename(None) => self.push_system("usage: /rename <name>"),
            SlashCommand::Model(arg) => self.show_or_set_model(arg),
            SlashCommand::Provider(arg) => self.show_or_set_provider(arg),
            SlashCommand::Tools => self.show_tools(),
            SlashCommand::Http(arg) => self.show_or_set_http(arg),
            SlashCommand::Clear => self.clear_active_transcript(),
            SlashCommand::Tokens => self.show_tokens(),
            SlashCommand::Quit => self.should_quit = true,
            SlashCommand::Unknown(name) => self.push_system(format!("unknown command: /{name}")),
        }
    }

    /// `/new [dir]` (`M13-3`): the non-modal equivalent of `Ctrl+T` + Enter — same
    /// `session_factory` assembly `submit_spawn_form` uses, minus the model/http override
    /// fields the modal form exposes. A bad directory or a full `SessionManager` reports as a
    /// `push_system` line rather than popping a form back up, since there's no form here to
    /// return to.
    async fn spawn_tab_from_command(&mut self, dir: Option<String>) {
        let dir_input = dir.unwrap_or_else(|| ".".to_string());
        let root = match dunce::canonicalize(&dir_input) {
            Ok(root) => root,
            Err(_) => {
                self.push_system(format!("no such directory: {dir_input}"));
                return;
            }
        };
        let title = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "mate".to_string());
        let http_enabled = self.defaults.http.enabled;
        let spec = session_factory::build_spec(&self.defaults, &root, title.clone(), http_enabled);
        let ctx = session_factory::build_tool_ctx(
            root.clone(),
            self.defaults.max_output_bytes,
            self.defaults.agents_md_enabled,
            self.defaults.agents_md_max_bytes,
        );
        let skills = ctx.skills.to_vec();
        let agents_md = ctx.agents_md.as_ref().map(|s| s.filename.to_string());

        match self.manager.spawn(&spec, ctx) {
            Ok(handle) => {
                let provider = self.defaults.provider_label();
                let subagent_model = self.defaults.subagent_model_label();
                let may_delegate = self.defaults.delegation.enabled;
                self.tabs.push(SessionTab::new(
                    handle.id,
                    handle,
                    title,
                    self.defaults.model.clone(),
                    provider,
                    subagent_model,
                    root,
                    http_enabled,
                    may_delegate,
                    skills,
                    agents_md,
                ));
                let new_idx = self.tabs.len() - 1;
                self.switch_to(new_idx);
            }
            Err(err) => self.push_system(err.to_string()),
        }
    }

    /// `/model [id]` (`M13-3`): reports the active tab's own model with no argument. With one,
    /// updates the default new tabs (`Ctrl+T`/`/new`) start from — the running agent's model is
    /// baked into its already-built `Agent<M>` and can't be swapped live, so this is honest
    /// about what it actually changes rather than promising something it can't do.
    fn show_or_set_model(&mut self, arg: Option<String>) {
        match arg {
            None => {
                let model = self.tabs[self.active].model.clone();
                self.push_system(format!("model: {model} (this tab)"));
            }
            Some(model) => {
                self.defaults.model = model.clone();
                self.push_system(format!(
                    "default model set to {model} for new tabs (/new, Ctrl+T) — this tab keeps its own"
                ));
            }
        }
    }

    fn show_or_set_provider(&mut self, arg: Option<String>) {
        match arg {
            None => {
                let provider = self.tabs[self.active].provider.clone();
                self.push_system(format!("provider: {provider} (this tab)"));
            }
            Some(provider) => {
                self.defaults.sub_provider = Some(provider.clone());
                self.push_system(format!(
                    "default provider set to {provider} for new tabs (/new, Ctrl+T) — this tab keeps its own"
                ));
            }
        }
    }

    /// `/tools` (`M13-3`): lists the active tab's actually-attached tools, derived from
    /// `SessionTab::http_enabled`/`may_delegate` rather than `SessionDefaults` — a tab's own
    /// toolset can diverge from the defaults it was spawned from (the spawn form's http toggle).
    fn show_tools(&mut self) {
        let tab = &self.tabs[self.active];
        let mut names = vec!["read_file", "list_dir", "find_files"];
        if tab.http_enabled {
            names.push("http_request");
        }
        if tab.may_delegate {
            names.push("spawn_agent");
        }
        self.push_system(format!("tools: {}", names.join(", ")));
    }

    /// `/http [on|off]` (`M13-3`): same "report this tab, set the default" shape as `/model` —
    /// the http tool is attached at agent-build time, so toggling it can't take effect on a
    /// tab that's already running.
    fn show_or_set_http(&mut self, arg: Option<String>) {
        match arg {
            None => {
                let enabled = self.tabs[self.active].http_enabled;
                self.push_system(format!(
                    "http: {} (this tab)",
                    if enabled { "on" } else { "off" }
                ));
            }
            Some(value) => {
                let enabled = match value.to_ascii_lowercase().as_str() {
                    "on" => true,
                    "off" => false,
                    _ => {
                        self.push_system("usage: /http [on|off]");
                        return;
                    }
                };
                self.defaults.http.enabled = enabled;
                self.push_system(format!(
                    "http tool set to {} for new tabs (/new, Ctrl+T) — this tab's agent is \
                     already built and can't be changed live",
                    if enabled { "on" } else { "off" }
                ));
            }
        }
    }

    /// `/tokens` (`M13-3`): the same sent/received/cost figures the bottom status bar and
    /// `CONTEXT` widget already show, as one line the user can keep in their scrollback.
    fn show_tokens(&mut self) {
        let tab = &self.tabs[self.active];
        let cost = estimate_cost(
            &tab.usage,
            &tab.model,
            tab.subagent_model.as_deref().unwrap_or(&tab.model),
            &self.pricing,
        );
        let sent = tab.usage.root.input_tokens + tab.usage.subagents.input_tokens;
        let recv = tab.usage.root.output_tokens + tab.usage.subagents.output_tokens;
        let cost_text = if cost.known {
            format!(
                "~${:.2} (avg ${:.3}/turn)",
                cost.total_usd, cost.per_turn_avg
            )
        } else {
            "~$? (unpriced model, see [pricing])".to_string()
        };
        self.push_system(format!("tokens: {sent}↑ {recv}↓ · {cost_text}"));
    }

    /// `/clear` (`M13-3`): wipes the active tab's transcript and its wrap cache — a fresh
    /// `Transcript`/`WrapCache` is simpler and just as correct as evicting entry by entry, since
    /// nothing outside this tab holds a reference to either.
    fn clear_active_transcript(&mut self) {
        let tab = &mut self.tabs[self.active];
        tab.transcript = Transcript::new();
        tab.wrap = WrapCache::new();
        tab.scroll = 0;
    }

    async fn handle_spawn_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.spawn_form = None;
                return;
            }
            KeyCode::Enter => {
                self.submit_spawn_form().await;
                return;
            }
            _ => {}
        }
        let Some(form) = self.spawn_form.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Tab => form.next_field(),
            KeyCode::BackTab => form.prev_field(),
            KeyCode::Char(' ') if form.focus == SpawnField::Http => {
                form.http_enabled = !form.http_enabled;
            }
            KeyCode::Left | KeyCode::Right if form.focus == SpawnField::Http => {
                form.http_enabled = !form.http_enabled;
            }
            KeyCode::Backspace => form.backspace(),
            KeyCode::Char(c) => form.push_char(c),
            _ => {}
        }
    }

    /// Builds a session from the form's fields and spawns it (`M8-3`). A bad directory or a
    /// full `SessionManager` puts the form back up with an error line instead of losing what
    /// the user typed.
    async fn submit_spawn_form(&mut self) {
        let Some(form) = self.spawn_form.take() else {
            return;
        };
        let dir_input = if form.dir.trim().is_empty() {
            ".".to_string()
        } else {
            form.dir.clone()
        };
        let root: PathBuf = match dunce::canonicalize(&dir_input) {
            Ok(root) => root,
            Err(_) => {
                self.spawn_form = Some(SpawnForm {
                    error: Some(format!("no such directory: {dir_input}")),
                    ..form
                });
                return;
            }
        };

        let mut defaults = self.defaults.clone();
        if !form.model.trim().is_empty() {
            defaults.model = form.model.clone();
        }
        let title = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "mate".to_string());

        let spec = session_factory::build_spec(&defaults, &root, title.clone(), form.http_enabled);
        let ctx = session_factory::build_tool_ctx(
            root.clone(),
            defaults.max_output_bytes,
            defaults.agents_md_enabled,
            defaults.agents_md_max_bytes,
        );
        let skills = ctx.skills.to_vec();
        let agents_md = ctx.agents_md.as_ref().map(|s| s.filename.to_string());

        match self.manager.spawn(&spec, ctx) {
            Ok(handle) => {
                let provider = defaults.provider_label();
                let subagent_model = defaults.subagent_model_label();
                let may_delegate = defaults.delegation.enabled;
                self.tabs.push(SessionTab::new(
                    handle.id,
                    handle,
                    title,
                    defaults.model.clone(),
                    provider,
                    subagent_model,
                    root,
                    form.http_enabled,
                    may_delegate,
                    skills,
                    agents_md,
                ));
                let new_idx = self.tabs.len() - 1;
                self.switch_to(new_idx);
            }
            Err(err) => {
                self.spawn_form = Some(SpawnForm {
                    error: Some(err.to_string()),
                    ..form
                });
            }
        }
    }

    /// `Ctrl+W` (`M8-3`): closes outright if the active tab is idle, otherwise arms a
    /// confirmation — a second `Ctrl+W` (or any other key disarms it, same as `quit_armed`).
    async fn request_close_active(&mut self) {
        let running = self.tabs[self.active].running_turn;
        if running && !self.close_confirm {
            self.close_confirm = true;
            return;
        }
        self.close_confirm = false;
        self.close_tab(self.active).await;
    }

    async fn close_tab(&mut self, idx: usize) {
        let tab = self.tabs.remove(idx);
        self.manager.close(tab.id).await;
        if self.tabs.is_empty() {
            self.should_quit = true;
            return;
        }
        if idx < self.active {
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    fn switch_to(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.active = idx;
        self.tabs[idx].unread = false;
        self.tabs[idx].needs_attention = false;
        self.dirty = true;
    }

    /// `Ctrl+G` (`M8-4`): the next tab (wrapping, starting after the active one) that's unread
    /// or needs attention. `None` if nothing else is flagged.
    fn next_attention_tab(&self) -> Option<usize> {
        let len = self.tabs.len();
        if len <= 1 {
            return None;
        }
        (1..len)
            .map(|d| (self.active + d) % len)
            .find(|&i| self.tabs[i].unread || self.tabs[i].needs_attention)
    }

    async fn on_term_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key).await,
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Resize(_, _) => self.dirty = true,
            _ => {}
        }
    }

    /// Routes the wheel to the active tab's transcript scroll — not the input box (`M7-1`
    /// follow-up: without this, mouse wheel events fall through to `InputBox`'s `Up`/`Down`
    /// history recall instead, since a terminal with mouse reporting off emulates wheel motion
    /// as arrow keys). Ignored while a modal surface (spawn form, detail modal, approval, panel
    /// focus) has input focus, matching how `on_key` gates those same cases.
    fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.tabs.is_empty() || self.spawn_form.is_some() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let modal_has_focus = tab.detail_modal.is_some()
            || tab.panel_focus.is_some()
            || !tab.pending_approvals.is_empty();
        if modal_has_focus {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                tab.scroll = tab.scroll.saturating_add(SCROLL_STEP);
                self.dirty = true;
            }
            MouseEventKind::ScrollDown => {
                tab.scroll = tab.scroll.saturating_sub(SCROLL_STEP);
                self.dirty = true;
            }
            _ => {}
        }
    }
}

/// Runs the tabbed TUI (`M7`/`M8`) to completion: initializes the terminal (raw mode, alternate
/// screen, and — via [`ratatui::try_init`] — the panic hook that restores it), drives the event
/// loop, and restores the terminal on the way out. `Shutdown` goes to every open tab regardless
/// of how the loop ended (`M8-6`), so no in-flight turn in any tab outlives the UI.
pub async fn run(
    manager: SessionManager,
    events: mpsc::Receiver<SessionEvent>,
    sessions: Vec<InitialSession>,
    defaults: SessionDefaults,
    pricing: HashMap<String, ModelRate>,
) -> Result<(), TuiError> {
    if sessions.is_empty() {
        return Ok(());
    }
    let mut terminal = ratatui::try_init()?;
    crossterm::execute!(io::stdout(), EnableMouseCapture)?;
    let mut app = App::new(manager, events, sessions, defaults, pricing);
    let result = run_loop(&mut app, &mut terminal).await;
    let _ = crossterm::execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    for tab in &app.tabs {
        let _ = tab.handle.send(SessionCmd::Shutdown).await;
    }
    result
}

async fn run_loop(app: &mut App, terminal: &mut ratatui::DefaultTerminal) -> Result<(), TuiError> {
    let mut term_events = EventStream::new();
    let mut tick: Interval = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    {
        let mut view = app.view();
        terminal.draw(|f| ui::draw(f, &mut view))?;
    }
    app.dirty = false;

    loop {
        if app.should_quit {
            break;
        }
        tokio::select! {
            biased;
            maybe_event = term_events.next() => {
                match maybe_event {
                    Some(Ok(event)) => app.on_term_event(event).await,
                    Some(Err(_)) | None => break,
                }
            }
            maybe_session_event = app.events.recv() => {
                match maybe_session_event {
                    Some(event) => app.on_session_event(event),
                    None => break,
                }
            }
            _ = tick.tick() => {
                if app.dirty {
                    let mut view = app.view();
                    terminal.draw(|f| ui::draw(f, &mut view))?;
                    app.dirty = false;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mate_core::backend::Backend;
    use mate_core::config::{AgentSpec, DelegationPolicy, HttpPolicy, SessionSpec};
    use mate_tool_api::{SubagentOutcome, ToolCtx};
    use mate_tool_http::HttpShared;
    use tokio_util::sync::CancellationToken;

    use crate::roster::SubagentStatus;

    use super::*;

    fn spec(title: &str) -> SessionSpec {
        SessionSpec {
            title: title.to_string(),
            root: PathBuf::from("."),
            agent: AgentSpec {
                model: "org/model".to_string(),
                sub_provider: None,
                base_url: None,
                preamble: String::new(),
                temperature: 0.2,
                max_tokens: 512,
                max_turns: 4,
                http: HttpPolicy::default(),
                may_delegate: false,
                delegation: DelegationPolicy::default(),
            },
            delegation: DelegationPolicy::default(),
            max_turns: 4,
        }
    }

    fn ctx() -> ToolCtx {
        ToolCtx {
            agent: AgentId::ROOT,
            root: PathBuf::from("."),
            max_output_bytes: 1_000_000,
            spawner: None,
            activity: tokio::sync::mpsc::channel(1).0,
            cancel: CancellationToken::new(),
            approvals: None,
            skills: std::sync::Arc::from([]),
            agents_md: None,
        }
    }

    fn defaults() -> SessionDefaults {
        SessionDefaults {
            model: "org/model".to_string(),
            sub_provider: None,
            temperature: 0.2,
            max_tokens: 512,
            max_turns: 4,
            http: HttpPolicy::default(),
            delegation: DelegationPolicy::default(),
            max_output_bytes: 1_000_000,
            agents_md_enabled: true,
            agents_md_max_bytes: 32_768,
        }
    }

    /// Spawns `n` offline sessions — `Backend::huggingface` never touches the network at
    /// construction, matching `mate-core`'s own crash-isolation tests — and wraps them in an
    /// `App`. Enough to exercise tab switching, marker routing, and close bookkeeping without a
    /// real provider. `SessionManager::spawn` calls `tokio::spawn` internally, so every test
    /// using this must run under `#[tokio::test]` even when it never itself awaits anything.
    fn test_app(n: usize) -> App {
        let backend = Arc::new(Backend::huggingface("dummy-key", None, None).unwrap());
        let http = Arc::new(HttpShared::new(60).unwrap());
        let (mut manager, events_rx) = SessionManager::new(backend, http, 8);
        let mut sessions = Vec::new();
        for i in 0..n {
            let handle = manager.spawn(&spec(&format!("s{i}")), ctx()).unwrap();
            sessions.push(InitialSession {
                session_id: handle.id,
                handle,
                title: format!("s{i}"),
                model: "org/model".to_string(),
                provider: "huggingface".to_string(),
                root: PathBuf::from("."),
                subagent_model: None,
                http_enabled: true,
                may_delegate: false,
                skills: Vec::new(),
                agents_md: None,
            });
        }
        App::new(manager, events_rx, sessions, defaults(), HashMap::new())
    }

    fn scroll(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[tokio::test]
    async fn mouse_wheel_scrolls_the_transcript_not_the_input() {
        let mut app = test_app(1);

        app.on_mouse(scroll(MouseEventKind::ScrollUp));

        assert_eq!(
            app.tabs[0].scroll, SCROLL_STEP,
            "wheel-up should move the transcript scroll offset, not fall through to the input box"
        );
        assert!(
            app.tabs[0].input.is_empty(),
            "wheel scroll must not touch the input box's draft"
        );
    }

    #[tokio::test]
    async fn mouse_wheel_down_does_not_underflow_past_the_bottom() {
        let mut app = test_app(1);

        app.on_mouse(scroll(MouseEventKind::ScrollDown));

        assert_eq!(
            app.tabs[0].scroll, 0,
            "scrolling down from the bottom saturates at 0 rather than wrapping"
        );
    }

    #[tokio::test]
    async fn mouse_wheel_is_ignored_while_the_spawn_form_is_open() {
        let mut app = test_app(1);
        app.spawn_form = Some(SpawnForm::new());

        app.on_mouse(scroll(MouseEventKind::ScrollUp));

        assert_eq!(
            app.tabs[0].scroll, 0,
            "a modal overlay owns input focus, so the wheel must not reach the transcript underneath"
        );
    }

    #[tokio::test]
    async fn switch_to_updates_active_and_clears_the_targets_markers() {
        let mut app = test_app(3);
        app.tabs[1].unread = true;
        app.tabs[1].needs_attention = true;

        app.switch_to(1);

        assert_eq!(app.active, 1);
        assert!(!app.tabs[1].unread);
        assert!(!app.tabs[1].needs_attention);
    }

    #[tokio::test]
    async fn switching_to_the_current_tab_is_a_no_op() {
        let mut app = test_app(2);
        app.dirty = false;
        app.switch_to(0);
        assert!(!app.dirty);
    }

    #[tokio::test]
    async fn background_activity_marks_a_tab_unread_but_not_the_active_one() {
        let mut app = test_app(2);
        let background = app.tabs[1].id;

        app.on_session_event(SessionEvent {
            session: background,
            agent: AgentId::ROOT,
            event: AgentEvent::Token("hi".to_string()),
        });

        assert!(app.tabs[1].unread);
        assert!(!app.tabs[0].unread);
    }

    #[tokio::test]
    async fn a_background_error_marks_needs_attention_not_just_unread() {
        let mut app = test_app(2);
        let background = app.tabs[1].id;

        app.on_session_event(SessionEvent {
            session: background,
            agent: AgentId::ROOT,
            event: AgentEvent::Error("boom".to_string()),
        });

        assert!(app.tabs[1].needs_attention);
    }

    #[tokio::test]
    async fn activity_on_the_active_tab_never_sets_unread() {
        let mut app = test_app(2);
        let active = app.tabs[0].id;

        app.on_session_event(SessionEvent {
            session: active,
            agent: AgentId::ROOT,
            event: AgentEvent::Token("hi".to_string()),
        });

        assert!(!app.tabs[0].unread);
    }

    #[tokio::test]
    async fn next_attention_tab_wraps_and_skips_clean_tabs() {
        let mut app = test_app(3);
        app.tabs[2].unread = true;

        assert_eq!(app.next_attention_tab(), Some(2));
    }

    #[tokio::test]
    async fn next_attention_tab_is_none_when_nothing_is_flagged() {
        let app = test_app(3);
        assert_eq!(app.next_attention_tab(), None);
    }

    #[tokio::test]
    async fn closing_a_non_active_tab_shifts_the_active_index_down() {
        let mut app = test_app(3);
        app.active = 2;
        app.close_tab(0).await;
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 1);
    }

    #[tokio::test]
    async fn closing_the_last_tab_quits() {
        let mut app = test_app(1);
        app.close_tab(0).await;
        assert!(app.tabs.is_empty());
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn submitting_the_spawn_form_with_a_valid_dir_opens_and_switches_to_a_new_tab() {
        let mut app = test_app(1);
        let tmp = tempfile::tempdir().unwrap();
        app.spawn_form = Some(SpawnForm {
            dir: tmp.path().to_string_lossy().into_owned(),
            model: String::new(),
            http_enabled: true,
            focus: SpawnField::Dir,
            error: None,
        });

        app.submit_spawn_form().await;

        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active, 1);
        assert!(app.spawn_form.is_none());
    }

    #[tokio::test]
    async fn submitting_the_spawn_form_with_a_bad_dir_keeps_it_open_with_an_error() {
        let mut app = test_app(1);
        app.spawn_form = Some(SpawnForm {
            dir: "/does/not/exist/anywhere".to_string(),
            model: String::new(),
            http_enabled: true,
            focus: SpawnField::Dir,
            error: None,
        });

        app.submit_spawn_form().await;

        assert_eq!(app.tabs.len(), 1, "no tab should open for a bad directory");
        assert!(app.spawn_form.as_ref().unwrap().error.is_some());
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn net_activity() -> mate_tool_api::ToolActivity {
        mate_tool_api::ToolActivity::NetRequest {
            method: http::Method::GET,
            host: "example.com".to_string(),
            path: "/".to_string(),
            status: Some(200),
            ms: 5,
            bytes: 2,
            redirects: 0,
            reason: None,
        }
    }

    #[tokio::test]
    async fn ctrl_b_toggles_the_active_tabs_panel_visibility() {
        let mut app = test_app(1);
        assert!(
            app.tabs[0].panel_visible,
            "a fresh tab starts with the panel visible"
        );

        app.on_key(ctrl_key('b')).await;
        assert!(!app.tabs[0].panel_visible);

        app.on_key(ctrl_key('b')).await;
        assert!(app.tabs[0].panel_visible);
    }

    #[tokio::test]
    async fn a_subagents_activity_still_reaches_the_panel_though_not_the_transcript() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;

        app.on_session_event(SessionEvent {
            session,
            agent: AgentId(1),
            event: AgentEvent::Activity(net_activity()),
        });

        assert_eq!(
            app.tabs[0].panel.network.len(),
            1,
            "a subagent's own NetRequest must still fold into this tab's panel"
        );
        assert!(
            app.tabs[0].transcript.is_empty(),
            "subagent chatter must never inline into the root transcript (§9.9)"
        );
    }

    #[tokio::test]
    async fn activity_on_a_background_tab_marks_it_unread() {
        let mut app = test_app(2);
        let background = app.tabs[1].id;

        app.on_session_event(SessionEvent {
            session: background,
            agent: AgentId::ROOT,
            event: AgentEvent::Activity(net_activity()),
        });

        assert!(app.tabs[1].unread);
        assert_eq!(app.tabs[1].panel.network.len(), 1);
    }

    // --- `M12-6`/`M12-9`: the subagent roster and panel navigation --------------------------

    #[tokio::test]
    async fn subagent_spawned_and_finished_route_into_the_roster_not_the_transcript() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;

        app.on_session_event(SessionEvent {
            session,
            agent: AgentId(1),
            event: AgentEvent::SubagentSpawned {
                id: AgentId(1),
                label: "deps".to_string(),
                task: "find deps".to_string(),
            },
        });
        assert_eq!(app.tabs[0].roster.len(), 1);
        assert!(
            app.tabs[0].transcript.is_empty(),
            "a subagent's own lifecycle events must never inline into the root transcript (§9.9)"
        );

        app.on_session_event(SessionEvent {
            session,
            agent: AgentId(1),
            event: AgentEvent::SubagentFinished {
                id: AgentId(1),
                outcome: SubagentOutcome::Completed {
                    summary: "done".to_string(),
                },
            },
        });
        assert_eq!(app.tabs[0].roster.rows()[0].status, SubagentStatus::Done);
    }

    #[tokio::test]
    async fn a_subagent_tagged_usage_event_updates_the_subagent_side_not_root() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;
        app.on_session_event(SessionEvent {
            session,
            agent: AgentId(1),
            event: AgentEvent::SubagentSpawned {
                id: AgentId(1),
                label: "deps".to_string(),
                task: "t".to_string(),
            },
        });

        app.on_session_event(SessionEvent {
            session,
            agent: AgentId(1),
            event: AgentEvent::Usage(rig::completion::Usage {
                input_tokens: 40,
                output_tokens: 8,
                total_tokens: 48,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
                tool_use_prompt_tokens: 0,
                reasoning_tokens: 0,
            }),
        });

        assert_eq!(app.tabs[0].usage.subagents.input_tokens, 40);
        assert_eq!(app.tabs[0].usage.root.input_tokens, 0);
        assert_eq!(
            app.tabs[0].roster.rows()[0].turns,
            1,
            "a completion-call Usage event must count as one of this subagent's turns"
        );
    }

    #[tokio::test]
    async fn ctrl_p_opens_panel_focus_on_model_and_makes_the_panel_visible() {
        let mut app = test_app(1);
        app.tabs[0].panel_visible = false;

        app.on_key(ctrl_key('p')).await;

        assert_eq!(
            app.tabs[0].panel_focus,
            Some(PanelFocus {
                widget: PanelWidgetKind::Model,
                row: 0
            })
        );
        assert!(app.tabs[0].panel_visible);

        app.on_key(ctrl_key('p')).await;
        assert_eq!(
            app.tabs[0].panel_focus, None,
            "a second Ctrl+P releases focus"
        );
    }

    #[tokio::test]
    async fn tab_while_focused_cycles_through_every_widget_and_back_to_model() {
        let mut app = test_app(1);
        app.on_key(ctrl_key('p')).await;

        let tab_key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        for _ in 0..7 {
            app.on_key(tab_key).await;
        }

        assert_eq!(
            app.tabs[0].panel_focus.unwrap().widget,
            PanelWidgetKind::Model,
            "seven Tabs from Model must cycle through all seven widgets and land back on Model"
        );
    }

    #[tokio::test]
    async fn enter_on_the_context_widget_toggles_the_split() {
        let mut app = test_app(1);
        app.tabs[0].panel_focus = Some(PanelFocus {
            widget: PanelWidgetKind::Context,
            row: 0,
        });
        assert!(!app.tabs[0].context_split);

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

        assert!(app.tabs[0].context_split);
    }

    #[tokio::test]
    async fn enter_on_a_focused_subagent_row_opens_a_read_only_detail_modal() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;
        app.on_session_event(SessionEvent {
            session,
            agent: AgentId(1),
            event: AgentEvent::SubagentSpawned {
                id: AgentId(1),
                label: "deps".to_string(),
                task: "t".to_string(),
            },
        });
        app.tabs[0].panel_focus = Some(PanelFocus {
            widget: PanelWidgetKind::Subagents,
            row: 0,
        });

        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

        assert!(app.tabs[0].detail_modal.is_some());
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        app.on_key(esc).await;
        assert!(
            app.tabs[0].detail_modal.is_none(),
            "Esc must close the modal"
        );
    }

    #[tokio::test]
    async fn a_printable_key_while_focused_clears_focus_and_still_reaches_the_input() {
        let mut app = test_app(1);
        app.on_key(ctrl_key('p')).await;
        assert!(app.tabs[0].panel_focus.is_some());

        app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
            .await;

        assert_eq!(
            app.tabs[0].panel_focus, None,
            "a printable key must return focus to the input"
        );
        assert_eq!(app.tabs[0].input.textarea().lines()[0], "h");
    }

    #[tokio::test]
    async fn x_on_an_empty_roster_is_a_harmless_no_op() {
        let mut app = test_app(1);
        app.tabs[0].panel_focus = Some(PanelFocus {
            widget: PanelWidgetKind::Subagents,
            row: 0,
        });

        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .await;

        assert!(app.tabs[0].detail_modal.is_none());
    }

    // --- `M13-2`: the approval modal ---------------------------------------------------------

    async fn submit(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .await;
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;
    }

    fn approval_required(id: Ulid) -> AgentEvent {
        AgentEvent::ApprovalRequired {
            id,
            name: "http_request".to_string(),
            detail: "POST https://example.com".to_string(),
        }
    }

    #[tokio::test]
    async fn an_approval_request_queues_regardless_of_which_agent_asked() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;
        let id = Ulid::generate();

        app.on_session_event(SessionEvent {
            session,
            agent: AgentId::ROOT,
            event: approval_required(id),
        });

        assert_eq!(app.tabs[0].pending_approvals.len(), 1);
        assert_eq!(app.tabs[0].pending_approvals[0].agent, AgentId::ROOT);
    }

    #[tokio::test]
    async fn a_background_tabs_approval_sets_needs_attention_without_touching_the_active_tab() {
        let mut app = test_app(2);
        let background = app.tabs[1].id;

        app.on_session_event(SessionEvent {
            session: background,
            agent: AgentId::ROOT,
            event: approval_required(Ulid::generate()),
        });

        assert!(app.tabs[1].needs_attention);
        assert!(app.tabs[0].pending_approvals.is_empty());
    }

    #[tokio::test]
    async fn y_grants_the_front_of_the_queue_and_sends_session_cmd_approve() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;
        let id = Ulid::generate();
        app.on_session_event(SessionEvent {
            session,
            agent: AgentId::ROOT,
            event: approval_required(id),
        });

        app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .await;

        assert!(
            app.tabs[0].pending_approvals.is_empty(),
            "a decision must pop the request off the queue"
        );
    }

    #[tokio::test]
    async fn while_an_approval_is_pending_a_printable_key_never_reaches_the_input() {
        let mut app = test_app(1);
        let session = app.tabs[0].id;
        app.on_session_event(SessionEvent {
            session,
            agent: AgentId::ROOT,
            event: approval_required(Ulid::generate()),
        });

        app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
            .await;

        assert!(
            app.tabs[0].input.is_empty(),
            "an unrelated printable key while an approval is open must not leak into the input"
        );
        assert_eq!(
            app.tabs[0].pending_approvals.len(),
            1,
            "an unrecognized key (not y/n/Esc) must leave the pending request untouched"
        );
    }

    // --- `M13-3`: slash commands -------------------------------------------------------------

    #[tokio::test]
    async fn an_unknown_command_never_reaches_the_model_and_reports_itself() {
        let mut app = test_app(1);
        submit(&mut app, "/frobnicate").await;

        assert!(
            !app.tabs[0].running_turn,
            "an unknown slash command must never start a turn"
        );
        let entries: Vec<&crate::transcript::Entry> = app.tabs[0].transcript.iter().collect();
        assert!(matches!(
            entries.last(),
            Some(crate::transcript::Entry::System { text, .. })
                if text.contains("unknown command: /frobnicate")
        ));
    }

    #[tokio::test]
    async fn quit_sets_should_quit_without_a_confirmation() {
        let mut app = test_app(1);
        submit(&mut app, "/quit").await;
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn rename_with_no_argument_reports_usage_and_leaves_the_title_unchanged() {
        let mut app = test_app(1);
        let original = app.tabs[0].title.clone();
        submit(&mut app, "/rename").await;

        assert_eq!(app.tabs[0].title, original);
        let entries: Vec<&crate::transcript::Entry> = app.tabs[0].transcript.iter().collect();
        assert!(matches!(
            entries.last(),
            Some(crate::transcript::Entry::System { text, .. }) if text.contains("usage")
        ));
    }

    #[tokio::test]
    async fn rename_with_an_argument_renames_the_active_tab() {
        let mut app = test_app(1);
        submit(&mut app, "/rename api").await;
        assert_eq!(app.tabs[0].title, "api");
    }

    #[tokio::test]
    async fn clear_empties_the_transcript() {
        let mut app = test_app(1);
        app.tabs[0].transcript.push_user("hello".to_string());
        assert!(!app.tabs[0].transcript.is_empty());

        submit(&mut app, "/clear").await;

        assert!(app.tabs[0].transcript.is_empty());
    }

    #[tokio::test]
    async fn tools_lists_only_what_this_tab_actually_has_attached() {
        let mut app = test_app(1);
        app.tabs[0].http_enabled = false;
        app.tabs[0].may_delegate = false;

        submit(&mut app, "/tools").await;

        let entries: Vec<&crate::transcript::Entry> = app.tabs[0].transcript.iter().collect();
        let crate::transcript::Entry::System { text, .. } = entries.last().unwrap() else {
            panic!("expected a System entry");
        };
        assert!(!text.contains("http_request"));
        assert!(!text.contains("spawn_agent"));
        assert!(text.contains("read_file"));
    }
}
