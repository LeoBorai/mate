//! `mate` binary: parses args, layers config (flags → env → project file →
//! user file → defaults), and picks a frontend — the tabbed TUI by default (`M7`/`M8`), or
//! `--plain`/`--print` for the plain frontend `M5` ships.

mod cli;
mod config;
mod error;
mod logging;
mod plain;
mod tui;

use std::io::IsTerminal;

use clap::Parser;
use error::MateError;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("mate: {err}");
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                eprintln!("caused by: {cause}");
                source = cause.source();
            }
            std::process::ExitCode::from(err.exit_code())
        }
    }
}

async fn run() -> Result<(), MateError> {
    let _log_guard = logging::init().map_err(MateError::Io)?;

    let args = cli::Cli::parse();
    let config = config::load(&args).map_err(MateError::Config)?;
    tracing::debug!(model = %config.model, has_api_token = config::api_token().is_some(), "config loaded");
    tracing::info!("mate started");

    let stdout_is_tty = std::io::stdout().is_terminal();
    if plain::wants_plain(&args, stdout_is_tty) {
        plain::run(&args, &config).await
    } else {
        tui::run(&args, &config).await
    }
}
