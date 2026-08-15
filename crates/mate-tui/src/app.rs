//! App state and the event loop (`M7-2`): `select!`s over terminal input and this session's
//! `SessionEvent`s, redrawing at ~30 fps only when something actually changed.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use mate_core::session::{SessionCmd, SessionEvent, SessionHandle, SessionId};
use mate_core::streaming::AgentEvent;
use mate_tool_api::AgentId;
use tokio::sync::mpsc;
use tokio::time::{Duration, Interval, MissedTickBehavior, interval};

use crate::input::InputBox;
use crate::transcript::Transcript;
use crate::ui::{self, View};
use crate::wrap::WrapCache;

const TICK: Duration = Duration::from_millis(33);

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct App {
    handle: SessionHandle,
    session_id: SessionId,
    events: mpsc::Receiver<SessionEvent>,
    transcript: Transcript,
    wrap: WrapCache,
    input: InputBox,
    scroll: usize,
    running_turn: bool,
    quit_armed: bool,
    dirty: bool,
    should_quit: bool,
}

impl App {
    fn new(
        handle: SessionHandle,
        session_id: SessionId,
        events: mpsc::Receiver<SessionEvent>,
    ) -> Self {
        Self {
            handle,
            session_id,
            events,
            transcript: Transcript::new(),
            wrap: WrapCache::new(),
            input: InputBox::new(),
            scroll: 0,
            running_turn: false,
            quit_armed: false,
            dirty: true,
            should_quit: false,
        }
    }

    fn view(&mut self) -> View<'_> {
        View {
            transcript: &self.transcript,
            wrap: &mut self.wrap,
            input: &self.input,
            scroll: self.scroll,
            running_turn: self.running_turn,
        }
    }

    fn on_session_event(&mut self, event: SessionEvent) {
        // Only one session exists at `M7` (no tabs yet), and only the root agent's events have
        // a producer before `M9`'s subagents — filtered defensively rather than assumed.
        if event.session != self.session_id || event.agent != AgentId::ROOT {
            return;
        }
        let evicted = match event.event {
            AgentEvent::Token(text) => self.transcript.push_token(&text),
            AgentEvent::ToolCallStarted { name } => self.transcript.push_tool_call(name),
            AgentEvent::ToolResult { name, ok, summary } => {
                self.transcript.resolve_tool_call(&name, ok, summary);
                None
            }
            AgentEvent::TurnComplete => {
                self.transcript.end_turn();
                self.running_turn = false;
                None
            }
            AgentEvent::Error(text) => {
                self.transcript.push_error(text);
                self.running_turn = false;
                None
            }
            // No producer yet at `M7`: approvals land at `M13`, subagent lifecycle at `M9`,
            // and `Usage` only matters once the context widget exists (`M12`).
            AgentEvent::ApprovalRequired { .. }
            | AgentEvent::SubagentSpawned { .. }
            | AgentEvent::SubagentFinished { .. }
            | AgentEvent::Usage(_) => None,
        };
        if let Some(id) = evicted {
            self.wrap.invalidate(id);
        }
        self.dirty = true;
    }

    /// `Ctrl+C` and `Ctrl+O` are intercepted here, ahead of the input box, since they're
    /// app-level commands rather than editing keys the textarea should ever see.
    async fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.dirty = true;

        let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'));
        if is_ctrl_c {
            if self.running_turn {
                let _ = self.handle.send(SessionCmd::Cancel).await;
            } else if self.quit_armed {
                self.should_quit = true;
            } else {
                self.quit_armed = true;
            }
            return;
        }
        self.quit_armed = false;

        let is_ctrl_o = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'));
        if is_ctrl_o {
            self.transcript.toggle_last_tool_expanded();
            return;
        }

        if let Some(prompt) = self.input.on_key(key) {
            // §5.2: one turn at a time — a prompt sent while another is in flight is dropped
            // by the session task anyway, so don't even send it, and keep the draft the user
            // typed instead of losing it.
            if !self.running_turn {
                self.transcript.push_user(prompt.clone());
                self.running_turn = true;
                let _ = self.handle.send(SessionCmd::Prompt(prompt)).await;
            }
        }
    }

    async fn on_term_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key).await,
            Event::Resize(_, _) => self.dirty = true,
            _ => {}
        }
    }
}

/// Runs the single-tab TUI (`M7`) to completion: initializes the terminal (raw mode, alternate
/// screen, and — via [`ratatui::try_init`] — the panic hook that restores it), drives the event
/// loop, and restores the terminal on the way out. `Shutdown` goes to the session regardless of
/// how the loop ended, so an in-flight turn never outlives the UI.
pub async fn run(
    handle: SessionHandle,
    session_id: SessionId,
    events: mpsc::Receiver<SessionEvent>,
) -> Result<(), TuiError> {
    let mut terminal = ratatui::try_init()?;
    let mut app = App::new(handle, session_id, events);
    let result = run_loop(&mut app, &mut terminal).await;
    ratatui::restore();
    let _ = app.handle.send(SessionCmd::Shutdown).await;
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
