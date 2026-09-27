//! The Ratatui frontend: a tab bar over one session per tab (`M8`), each with its own scrolling
//! transcript, `ratatui-textarea` input box (`Enter`/`Alt+Enter` routing, `M7-5`), a bottom
//! status bar showing that tab's provider, model, tokens, and cost, and a toggleable agent
//! status panel (`Ctrl+B`, `M12`) — model, context/cost, subagent roster, network log, and
//! documents log, with its own vertical-budget allocation and `Ctrl+P` row-level navigation
//! (§9). `Ctrl+T` opens a new tab via [`build_spec`]; `Ctrl+W` closes the active one.

mod app;
mod highlight;
mod input;
mod panel;
mod panel_widgets;
mod roster;
mod session_factory;
mod slash;
mod text;
mod transcript;
mod ui;
mod wrap;

pub use app::{InitialSession, TuiError, run};
pub use session_factory::{SessionDefaults, build_spec, build_tool_ctx};
