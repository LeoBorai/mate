//! Docker-backed sandbox sessions for running Claude Code in a hardened
//! container, driven from the `mate` TUI.
//!
//! All Docker access sits behind this crate; `bollard` does not leak into
//! `mate-core`.

pub mod attach;
pub mod claude;
pub mod docker;
pub mod error;
pub mod policy;
pub mod reconcile;
pub mod report;
pub mod session;
pub mod store;

#[cfg(test)]
pub(crate) mod fake;
pub mod spec;
