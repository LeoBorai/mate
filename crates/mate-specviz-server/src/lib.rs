//! `mate`'s spec viewer: an axum server rendering a workspace's `specs/` and
//! `openspec/` Markdown into a browsable, live-reloading web UI. The UI
//! itself (`mate-specviz-client`, Leptos/WASM) is embedded at build time, so
//! the `mate` binary is the only thing a release ships.
//!
//! Start it through [`viewer`]: [`viewer::run`] for the foreground
//! `mate specviz` subcommand, [`viewer::spawn`] for the TUI's `/specviz`.
//! Internally: a CQRS application layer over pure `domain` types and I/O
//! `infra` adapters, fronted by the `web` layer. See
//! `.agents/docs/specviz.md`.

pub mod application;
pub mod bootstrap;
pub mod domain;
pub mod infra;
pub mod viewer;
pub mod web;

pub use viewer::{DEFAULT_PORT, Viewer, ViewerError, bind, run, spawn, url_of};
