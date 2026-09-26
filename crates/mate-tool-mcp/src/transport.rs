//! [`StdioTransport`]: one configured MCP server's live child process and JSON-RPC session.
//! MCP's stdio transport is newline-delimited JSON-RPC 2.0 over the child's own stdin/stdout —
//! no length prefix, no other framing. A background task owns the read half and demultiplexes
//! responses back to whichever call is waiting on them by request id; stderr is drained and
//! discarded (diagnostics only, never parsed as protocol, per this change's scope).
//!
//! This crate hand-rolls the wire protocol rather than pulling in a third-party MCP client
//! crate: JSON-RPC 2.0 and MCP's stdio framing are small, stable, documented shapes, while an
//! external crate's Rust-level API (builder types, trait shapes) is exactly the kind of detail
//! that can't be verified without compiling — and this repo's hard rule against running `cargo`
//! means there is no compiler feedback loop to catch a mismatch. Hand-rolling the protocol
//! itself is the lower-risk choice under that constraint; revisit if a maintained crate's API
//! is later confirmed against a real build.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use mate_tool_api::ToolFailure;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

use crate::protocol::{JsonRpcInbound, JsonRpcNotification, JsonRpcRequest};

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, ToolFailure>>>>>;

/// One server's live stdio session. Cheap to clone (everything shared is already behind an
/// `Arc`) — not that this crate ever clones one, but the shape matches [`crate::servers`]'s
/// registry, which holds one per configured server behind its own `Arc`.
pub struct StdioTransport {
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: PendingMap,
    next_id: AtomicU64,
    /// Flipped by the reader task the moment the child's stdout closes (process exited) — every
    /// call after that fails fast with a "server not running" error instead of hanging on a
    /// write to a dead process's stdin.
    dead: Arc<AtomicBool>,
    /// Kept alive so the process isn't dropped (and, via `kill_on_drop`, killed) out from under
    /// an in-flight session; `wait`/exit status isn't polled anywhere today. Read directly only
    /// by [`Self::kill`] (test support).
    child: Mutex<Child>,
}

impl StdioTransport {
    /// Spawns `command args` with `env` merged over the current process's own environment (the
    /// same inheritance `tokio::process::Command` gives by default), wires stdin/stdout as
    /// pipes, stderr as a pipe that's drained and dropped, and starts the background reader
    /// task. Spawn failure (bad command, missing binary) surfaces immediately; nothing about the
    /// MCP handshake happens here yet — see [`crate::servers`] for that.
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, ToolFailure> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!("spawn {command}: {err}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ToolFailure::Other(anyhow::anyhow!("{command}: no stdin handle")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolFailure::Other(anyhow::anyhow!("{command}: no stdout handle")))?;
        let stderr = child.stderr.take();

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let dead = Arc::new(AtomicBool::new(false));

        tokio::spawn(read_loop(stdout, pending.clone(), dead.clone()));
        if let Some(stderr) = stderr {
            tokio::spawn(drain_stderr(stderr));
        }

        Ok(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            dead,
            child: Mutex::new(child),
        })
    }

    /// Whether the child process's stdout has closed (process exited or otherwise stopped
    /// speaking to us). Checked before every call so a dead server fails fast rather than
    /// hanging on a write nothing will ever answer.
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Sends a JSON-RPC request and waits for its matched response, up to `timeout`. A response
    /// carrying a JSON-RPC error object surfaces as `ToolFailure::Other`; a timeout as
    /// `ToolFailure::Timeout`; the process having already died as `ToolFailure::Other` without
    /// attempting any I/O.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, ToolFailure> {
        if self.is_dead() {
            return Err(ToolFailure::Other(anyhow::anyhow!(
                "server process is no longer running"
            )));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let line = serde_json::to_string(&JsonRpcRequest::new(id, method, params))
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?;
        if let Err(err) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(err);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ToolFailure::Other(anyhow::anyhow!(
                "server closed the connection before answering"
            ))),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(ToolFailure::Timeout(timeout))
            }
        }
    }

    /// Sends a notification (`initialized`): no id, no response expected or waited on.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), ToolFailure> {
        let line = serde_json::to_string(&JsonRpcNotification::new(method, params))
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?;
        self.write_line(&line).await
    }

    /// Test-support: kills the child process directly, for crash-isolation tests that need to
    /// force `is_dead()` to flip without waiting on a real MCP server to exit on its own. Not
    /// `#[cfg(test)]` — see [`crate::servers::McpServers::test_with_ready`]'s doc comment for
    /// why plain `pub` test-support functions are this crate's convention.
    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }

    async fn write_line(&self, line: &str) -> Result<(), ToolFailure> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))?;
        stdin
            .flush()
            .await
            .map_err(|err| ToolFailure::Other(anyhow::anyhow!(err)))
    }
}

/// Reads the child's stdout line by line for as long as it stays open, matching each parsed
/// response to a pending request by `id` and completing that request's oneshot. On EOF (the
/// process exited or closed stdout), flips `dead` and fails every still-pending request with a
/// descriptive error instead of leaving its caller hanging forever.
async fn read_loop(stdout: tokio::process::ChildStdout, pending: PendingMap, dead: Arc<AtomicBool>) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(inbound) = serde_json::from_str::<JsonRpcInbound>(&line) else {
                    // A line mate can't parse as JSON-RPC is a confused server, not this
                    // client's problem to crash over — skip it and keep reading.
                    continue;
                };
                let Some(id) = inbound.id else {
                    continue;
                };
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let result = match (inbound.result, inbound.error) {
                        // JSON-RPC 2.0's standard "Invalid params" code — the one server-side
                        // error this client distinguishes, since `mate_tool_mcp::McpProxy`
                        // reports a malformed `arguments` value back to the model as
                        // `ToolFailure::InvalidArgs` rather than the generic `Other`.
                        (_, Some(err)) if err.code == -32602 => {
                            Err(ToolFailure::InvalidArgs(err.message))
                        }
                        (_, Some(err)) => Err(ToolFailure::Other(anyhow::anyhow!(
                            "mcp error {}: {}",
                            err.code,
                            err.message
                        ))),
                        (Some(value), None) => Ok(value),
                        (None, None) => Ok(Value::Null),
                    };
                    let _ = tx.send(result);
                }
            }
            Ok(None) | Err(_) => break,
        }
    }

    dead.store(true, Ordering::SeqCst);
    let mut map = pending.lock().await;
    for (_, tx) in map.drain() {
        let _ = tx.send(Err(ToolFailure::Other(anyhow::anyhow!(
            "server process exited before answering"
        ))));
    }
}

/// Drains stderr so the child never blocks on a full pipe; content is discarded (diagnostics
/// only — this change doesn't surface it anywhere, per `design.md`'s scope).
async fn drain_stderr(stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(_line)) = lines.next_line().await {}
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    /// `cat` echoes stdin straight back to stdout byte-for-byte, so a JSON-RPC request line
    /// sent to it comes back as that exact line — no MCP semantics involved, but it exercises
    /// the real spawn → write → read → id-matched-oneshot round trip against a real child
    /// process, which is what this test actually needs to prove. The echoed line has no
    /// `result`/`error` field, so `read_loop` resolves it to `Ok(Value::Null)` — proving the
    /// id-matching worked, not that this is a meaningful MCP response.
    #[tokio::test]
    async fn a_request_round_trips_through_a_real_child_process() {
        let transport =
            StdioTransport::spawn("cat", &[], &HashMap::new()).expect("cat must be spawnable");

        let result = transport
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("cat's echo has no error field, so this must resolve Ok");

        assert_eq!(
            result,
            Value::Null,
            "an echoed request (no result field) must resolve to Value::Null, proving the \
             response was matched back to the request that sent it by id"
        );
        assert!(
            !transport.is_dead(),
            "the process is still running after one round trip"
        );
    }

    #[tokio::test]
    async fn the_process_dying_mid_session_fails_a_pending_call_and_flips_is_dead() {
        let transport =
            StdioTransport::spawn("cat", &[], &HashMap::new()).expect("cat must be spawnable");
        assert!(!transport.is_dead());

        transport.kill().await;

        // No response will ever arrive now — `read_loop`'s EOF branch must fail this
        // immediately, not hang until the timeout.
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            transport.request("ping", None, Duration::from_secs(30)),
        )
        .await
        .expect("a call to a just-killed process must fail promptly, not hang on the timeout");

        assert!(result.is_err(), "a call to a dead process must fail");
        assert!(
            transport.is_dead(),
            "is_dead must flip once the reader task observes stdout EOF"
        );
    }

    /// A process that never writes anything back (`sleep`, which reads nothing and produces no
    /// stdout) exercises the actual timeout mechanism `crate::servers::connect_one` relies on
    /// for its handshake timeout, without needing a real MCP server that deliberately hangs.
    #[tokio::test]
    async fn a_call_that_never_gets_a_response_times_out_instead_of_hanging() {
        let transport = StdioTransport::spawn("sleep", &["30".to_string()], &HashMap::new())
            .expect("sleep must be spawnable");

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            transport.request("initialize", None, Duration::from_millis(200)),
        )
        .await
        .expect("the transport's own timeout must fire well before this outer guard does");

        assert!(
            matches!(result, Err(ToolFailure::Timeout(_))),
            "a server that never responds must time out as ToolFailure::Timeout, not hang \
             indefinitely or fail with some other error: {result:?}"
        );
    }

    #[tokio::test]
    async fn a_call_after_the_process_already_died_fails_without_attempting_any_io() {
        let transport =
            StdioTransport::spawn("cat", &[], &HashMap::new()).expect("cat must be spawnable");
        transport.kill().await;

        // Give the reader task a moment to observe EOF and flip `dead` before the next call —
        // otherwise this test would just be re-exercising the previous one's race-free path.
        for _ in 0..50 {
            if transport.is_dead() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(transport.is_dead(), "process must be observed dead by now");

        let result = tokio::time::timeout(
            Duration::from_millis(500),
            transport.request("ping", None, Duration::from_secs(30)),
        )
        .await
        .expect("a call against an already-dead transport must fail immediately");

        assert!(result.is_err());
    }
}
