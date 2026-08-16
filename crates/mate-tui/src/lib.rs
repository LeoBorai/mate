//! The Ratatui frontend: a tab bar over one session per tab (`M8`), each with its own scrolling
//! transcript, `ratatui-textarea` input box (`Enter`/`Alt+Enter` routing, `M7-5`), and left
//! panel showing that tab's model and provider. `Ctrl+T` opens a new tab via [`build_spec`];
//! `Ctrl+W` closes the active one.

mod app;
mod input;
mod session_factory;
mod transcript;
mod ui;
mod wrap;

pub use app::{InitialSession, TuiError, run};
pub use session_factory::{SessionDefaults, build_spec, build_tool_ctx};
