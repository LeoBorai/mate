//! `mate` binary: parses args, layers config (flags → env → project file →
//! user file → defaults), and picks a frontend — the tabbed TUI by default, or
//! `--plain` for a single-session stdout mode.

mod cli;
mod config;
mod error;
mod logging;

use clap::Parser;
use error::MateError;

fn main() -> std::process::ExitCode {
    match run() {
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

fn run() -> Result<(), MateError> {
    let _log_guard = logging::init().map_err(MateError::Io)?;

    let args = cli::Cli::parse();
    let config = config::load(&args).map_err(MateError::Config)?;
    tracing::debug!(model = %config.model, has_api_token = config::api_token().is_some(), "config loaded");
    tracing::info!("mate started");
    Ok(())
}
