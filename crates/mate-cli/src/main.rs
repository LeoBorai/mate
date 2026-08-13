//! `mate` binary: parses args, layers config (flags → env → project file →
//! user file → defaults), and picks a frontend — the tabbed TUI by default, or
//! `--plain` for a single-session stdout mode.

mod cli;
mod config;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();
    let config = config::load(&args)?;
    tracing::debug!(model = %config.model, has_api_token = config::api_token().is_some(), "config loaded");
    println!("mate");
    Ok(())
}
