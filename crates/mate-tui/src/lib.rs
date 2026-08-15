//! The Ratatui frontend (`M7`): the default frontend, one tab and no side panel yet — tabs
//! land at `M8`, the panel at `M12`. A scrolling transcript over one session's `AgentEvent`
//! stream, and a `ratatui-textarea` input box with mate's own `Enter`/`Alt+Enter` routing.

mod app;
mod input;
mod transcript;
mod ui;
mod wrap;

pub use app::{TuiError, run};
