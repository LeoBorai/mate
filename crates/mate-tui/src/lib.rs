//! The Ratatui frontend (`M7`): the default frontend, one tab — tabs land at `M8`. A scrolling
//! transcript over one session's `AgentEvent` stream, a `ratatui-textarea` input box with
//! mate's own `Enter`/`Alt+Enter` routing, and a left panel showing the active model and
//! provider.

mod app;
mod input;
mod transcript;
mod ui;
mod wrap;

pub use app::{TuiError, run};
