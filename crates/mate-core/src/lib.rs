//! The heart of `mate`: builds Rig agents (root and subagent alike), owns the
//! session manager (one Tokio task per session, one tab each), and drives the
//! subagent runtime — depth/concurrency/turn limits, cancellation, and the
//! `SessionEvent` stream that both the transcript and the side panel read from.

pub mod agent;
pub mod backend;
pub mod config;
pub mod preamble;
pub mod provider_error;
pub mod streaming;
