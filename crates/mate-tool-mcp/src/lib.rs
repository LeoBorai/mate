//! MCP (Model Context Protocol) client support for `mate`, stdio transport only (`add-mcp-
//! support`'s scope). **Dependency choice**: this crate hand-rolls the JSON-RPC 2.0 / MCP stdio
//! wire protocol on top of `tokio::process`/`tokio::io` rather than pulling in a third-party MCP
//! client crate (e.g. `rmcp`). JSON-RPC 2.0 and MCP's stdio line-framing are small, stable,
//! publicly documented shapes; a third-party crate's Rust-level API (builder types, trait
//! shapes, exact feature flags) is exactly the kind of detail that can't be verified without
//! compiling, and this repo's hard rule against running `cargo` means there is no compiler
//! feedback loop to catch a mismatch before it ships. Hand-rolling the protocol itself — not the
//! crate's API surface — is the lower-risk choice under that constraint. Revisit this choice
//! once a candidate crate's API has been confirmed against a real build.
//!
//! - [`servers`] — [`McpServers`], the process-wide registry of configured servers' live
//!   sessions (or failure reasons), built once by [`McpServers::connect`].
//! - [`transport`] — [`StdioTransport`], one server's spawned child process and JSON-RPC session.
//! - [`protocol`] — the JSON-RPC/MCP message shapes both of the above use.
//! - [`proxy`] — [`McpProxy`], the single `PortableTool` the model calls, dispatching across
//!   every configured server (see that module's doc comment for why this is one tool, not one
//!   per server).

mod protocol;
mod proxy;
mod servers;
mod transport;

pub use protocol::McpToolInfo;
pub use proxy::{McpArgs, McpProxy};
pub use servers::{McpServers, ServerHandle, ServerSpec};
pub use transport::StdioTransport;
