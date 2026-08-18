//! Tracing setup (M0-4, §11): file-only logging to
//! `$XDG_STATE_HOME/mate/mate.log` (or `~/.local/state/mate/mate.log`), level gated by
//! `RUST_LOG` (default `info`). Never writes to stdout/stderr — the TUI owns the terminal,
//! and `--plain` output must stay script-clean.
//!
//! `SessionId` / `AgentId` don't exist yet (they land with the session manager, §5), but every
//! span opened from here forward is expected to carry them as fields once they do — cheaper to
//! start that habit now than to retrofit every call site later.

use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// `$XDG_STATE_HOME/mate` or `~/.local/state/mate`. `pub(crate)` so `crate::firstrun` (`M13-5`)
/// can put its one-time marker file alongside `mate.log` rather than inventing a second
/// state-directory resolver.
pub(crate) fn state_dir() -> anyhow::Result<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(xdg).join("mate"));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local/state/mate"))
}

/// Initializes the global subscriber, writing exclusively to `mate.log`. Returns a guard that
/// must be held for the process lifetime — dropping it early can lose buffered log lines,
/// since the writer is non-blocking.
pub fn init() -> anyhow::Result<WorkerGuard> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating log directory {}", dir.display()))?;

    let file_appender = tracing_appender::rolling::never(&dir, "mate.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .init();

    Ok(guard)
}

#[cfg(test)]
// `Jail::expect_with` fixes its closure's error to `figment::Error`, which clippy flags as
// large; not ours to box, since `Jail` decides the closure signature.
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;

    #[test]
    fn prefers_xdg_state_home() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let home = jail.directory().display().to_string();
            jail.set_env("HOME", home);
            jail.set_env("XDG_STATE_HOME", "/custom/state");

            assert_eq!(state_dir().unwrap(), PathBuf::from("/custom/state/mate"));
            Ok(())
        });
    }

    #[test]
    fn falls_back_to_home_dot_local_state() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("HOME", "/home/tester");

            assert_eq!(
                state_dir().unwrap(),
                PathBuf::from("/home/tester/.local/state/mate")
            );
            Ok(())
        });
    }

    #[test]
    fn errors_without_home_or_xdg_state_home() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            assert!(state_dir().is_err());
            Ok(())
        });
    }
}
