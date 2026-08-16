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

use std::path::PathBuf;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use mate_core::session::{SessionCmd, SessionEvent, SessionHandle, SessionId, SessionManager};
use mate_core::streaming::AgentEvent;
use mate_tool_api::AgentId;
use tokio::sync::mpsc;
use tokio::time::{Duration, Interval, MissedTickBehavior, interval};

use crate::input::InputBox;
use crate::session_factory::{self, SessionDefaults};
use crate::transcript::Transcript;
use crate::ui::{self, AppView};
use crate::wrap::WrapCache;

const TICK: Duration = Duration::from_millis(33);

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
}

/// One tab's live state (`M8-2`). Everything that was a flat field on `App` before `M8` — the
/// transcript, the wrap cache, the input draft, scroll position — moved here unchanged; the
/// only things `App` still owns directly are the manager and which tab is active.
struct SessionTab {
    id: SessionId,
    handle: SessionHandle,
    title: String,
    model: String,
    provider: String,
    transcript: Transcript,
    wrap: WrapCache,
    input: InputBox,
    scroll: usize,
    running_turn: bool,
    /// Set when a background tab produces visible activity (`M8-4`); cleared on switching to it.
    unread: bool,
    /// Set when a background tab errors (`M8-4`); cleared on switching to it. Takes marker
    /// priority over `unread` — an error is worth more attention than "something happened".
    needs_attention: bool,
}

impl SessionTab {
    fn new(
        id: SessionId,
        handle: SessionHandle,
        title: String,
        model: String,
        provider: String,
    ) -> Self {
        Self {
            id,
            handle,
            title,
            model,
            provider,
            transcript: Transcript::new(),
            wrap: WrapCache::new(),
            input: InputBox::new(),
            scroll: 0,
            running_turn: false,
            unread: false,
            needs_attention: false,
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

pub struct App {
    manager: SessionManager,
    events: mpsc::Receiver<SessionEvent>,
    defaults: SessionDefaults,
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
    ) -> Self {
        let tabs = sessions
            .into_iter()
            .map(|s| SessionTab::new(s.session_id, s.handle, s.title, s.model, s.provider))
            .collect();
        Self {
            manager,
            events,
            defaults,
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
        AppView {
            tabs,
            active,
            spawn_form,
            session: ui::View {
                transcript: &tab.transcript,
                wrap: &mut tab.wrap,
                input: &tab.input,
                scroll: tab.scroll,
                running_turn: tab.running_turn,
                model: &tab.model,
                provider: &tab.provider,
                quit_armed,
                close_confirm,
            },
        }
    }

    /// Routes one session's event to its tab (`M8-2`), regardless of which tab is active.
    /// Background activity marks the tab unread; a background error marks it needing
    /// attention. Only the root agent has a producer before `M9`'s subagents exist.
    fn on_session_event(&mut self, event: SessionEvent) {
        if event.agent != AgentId::ROOT {
            return;
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == event.session) else {
            return;
        };
        self.dirty = true;
        let is_active = idx == self.active;
        let tab = &mut self.tabs[idx];

        let mut activity = false;
        let evicted = match event.event {
            AgentEvent::Token(text) => {
                activity = true;
                tab.transcript.push_token(&text)
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
            // No producer yet: approvals land at `M13`, subagent lifecycle at `M9`, and
            // `Usage` only matters once the context widget exists (`M12`).
            AgentEvent::ApprovalRequired { .. }
            | AgentEvent::SubagentSpawned { .. }
            | AgentEvent::SubagentFinished { .. }
            | AgentEvent::Usage(_) => None,
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

        if let Some(prompt) = self.tabs[self.active].input.on_key(key) {
            // §5.2: one turn at a time — a prompt sent while another is in flight is dropped
            // by the session task anyway, so don't even send it, and keep the draft the user
            // typed instead of losing it.
            if !self.tabs[self.active].running_turn {
                self.tabs[self.active].transcript.push_user(prompt.clone());
                self.tabs[self.active].running_turn = true;
                let _ = self.tabs[self.active]
                    .handle
                    .send(SessionCmd::Prompt(prompt))
                    .await;
            }
        }
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
        let ctx = session_factory::build_tool_ctx(root, defaults.max_output_bytes);

        match self.manager.spawn(&spec, ctx) {
            Ok(handle) => {
                let provider = defaults.provider_label();
                self.tabs.push(SessionTab::new(
                    handle.id,
                    handle,
                    title,
                    defaults.model.clone(),
                    provider,
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
            Event::Resize(_, _) => self.dirty = true,
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
) -> Result<(), TuiError> {
    if sessions.is_empty() {
        return Ok(());
    }
    let mut terminal = ratatui::try_init()?;
    let mut app = App::new(manager, events, sessions, defaults);
    let result = run_loop(&mut app, &mut terminal).await;
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
    use mate_tool_api::ToolCtx;
    use tokio_util::sync::CancellationToken;

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
        }
    }

    /// Spawns `n` offline sessions — `Backend::huggingface` never touches the network at
    /// construction, matching `mate-core`'s own crash-isolation tests — and wraps them in an
    /// `App`. Enough to exercise tab switching, marker routing, and close bookkeeping without a
    /// real provider. `SessionManager::spawn` calls `tokio::spawn` internally, so every test
    /// using this must run under `#[tokio::test]` even when it never itself awaits anything.
    fn test_app(n: usize) -> App {
        let backend = Arc::new(Backend::huggingface("dummy-key", None, None).unwrap());
        let (mut manager, events_rx) = SessionManager::new(backend, 8);
        let mut sessions = Vec::new();
        for i in 0..n {
            let handle = manager.spawn(&spec(&format!("s{i}")), ctx()).unwrap();
            sessions.push(InitialSession {
                session_id: handle.id,
                handle,
                title: format!("s{i}"),
                model: "org/model".to_string(),
                provider: "huggingface".to_string(),
            });
        }
        App::new(manager, events_rx, sessions, defaults())
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
}
