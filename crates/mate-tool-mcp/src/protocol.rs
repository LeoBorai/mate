//! Minimal JSON-RPC 2.0 message shapes, plus the handful of MCP-specific request/response
//! payloads this crate actually sends: `initialize`, `tools/list`, `tools/call`. Not a full MCP
//! type library — just what a stdio client needs to complete the handshake and route calls.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One outgoing JSON-RPC request, serialized as a single newline-terminated line (MCP's stdio
/// framing: one JSON-RPC message per line, no length prefix, no other delimiter).
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

/// A fire-and-forget notification (no `id`, no response expected) — MCP's `initialized`
/// notification, sent once after the server's `initialize` response arrives.
#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

/// One line read back from the server's stdout: either a response to a request this client
/// sent (`id` present), or a notification/request initiated by the server (`id` absent) — this
/// client only ever reads the former; anything else is ignored by [`crate::transport`].
#[derive(Debug, Deserialize)]
pub struct JsonRpcInbound {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcErrorObject>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcErrorObject {
    pub code: i64,
    pub message: String,
}

/// `initialize` request params — the fixed identity this client presents to every server,
/// regardless of which one it's talking to.
pub fn initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {
            "name": "mate",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

/// One tool as a server's `tools/list` response describes it.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpToolInfo>,
}

pub fn call_tool_params(tool: &str, arguments: Value) -> Value {
    serde_json::json!({
        "name": tool,
        "arguments": arguments,
    })
}
