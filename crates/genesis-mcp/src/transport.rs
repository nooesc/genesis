//! MCP transport implementations.
//!
//! Currently supports stdio transport (subprocess via stdin/stdout).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, warn};

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::McpError;

/// A running MCP transport that can send requests and receive responses.
#[allow(dead_code)]
pub struct StdioTransport {
    /// Channel to send outgoing messages to the writer task.
    outgoing_tx: mpsc::UnboundedSender<String>,
    /// Pending request callbacks indexed by request ID.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    /// Monotonically increasing request ID counter.
    next_id: AtomicU64,
    /// The child process handle (kept alive).
    _child: Arc<Mutex<Child>>,
}

impl StdioTransport {
    /// Spawn a subprocess and establish stdio transport.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| McpError::Transport(format!("failed to spawn `{command}`: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("failed to capture stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("failed to capture stdout".into()))?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Writer task: sends JSON-RPC messages to stdin
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<String>();
        let mut stdin = stdin;
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                    error!("mcp stdin write error: {e}");
                    break;
                }
                if let Err(e) = stdin.write_all(b"\n").await {
                    error!("mcp stdin newline error: {e}");
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    error!("mcp stdin flush error: {e}");
                    break;
                }
            }
        });

        // Reader task: reads JSON-RPC responses from stdout
        let pending_for_reader = Arc::clone(&pending);
        let reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim().to_owned();
                        if line.is_empty() {
                            continue;
                        }
                        debug!(raw = line.as_str(), "mcp stdout line");
                        match serde_json::from_str::<JsonRpcResponse>(&line) {
                            Ok(resp) => {
                                if let Some(id) = resp.id {
                                    let mut map = pending_for_reader.lock().await;
                                    if let Some(tx) = map.remove(&id) {
                                        let _ = tx.send(resp);
                                    }
                                }
                                // Notifications (no id) are silently dropped for now
                            }
                            Err(e) => {
                                debug!(error = %e, line = line.as_str(), "ignoring non-JSON-RPC line");
                            }
                        }
                    }
                    Ok(None) => {
                        warn!("mcp stdout stream closed");
                        break;
                    }
                    Err(e) => {
                        error!("mcp stdout read error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            outgoing_tx,
            pending,
            next_id: AtomicU64::new(1),
            _child: Arc::new(Mutex::new(child)),
        })
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: std::time::Duration,
    ) -> Result<JsonRpcResponse, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params);

        let json = serde_json::to_string(&request)
            .map_err(|e| McpError::Protocol(format!("failed to serialize request: {e}")))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, tx);
        }

        self.outgoing_tx
            .send(json)
            .map_err(|_| McpError::Transport("writer channel closed".into()))?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(McpError::Transport("response channel dropped".into())),
            Err(_) => {
                // Clean up pending entry on timeout
                let mut map = self.pending.lock().await;
                map.remove(&id);
                Err(McpError::Timeout)
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), McpError> {
        // Notifications use id: null, but we'll send without id field
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let json = serde_json::to_string(&msg)
            .map_err(|e| McpError::Protocol(format!("failed to serialize notification: {e}")))?;

        self.outgoing_tx
            .send(json)
            .map_err(|_| McpError::Transport("writer channel closed".into()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_fails_for_nonexistent_command() {
        let result = StdioTransport::spawn(
            "/nonexistent/binary/that/does/not/exist",
            &[],
            &HashMap::new(),
        )
        .await;
        assert!(result.is_err());
        match result {
            Err(McpError::Transport(_)) => {}
            _ => panic!("expected Transport error"),
        }
    }

    #[tokio::test]
    async fn spawn_cat_transport_succeeds() {
        // Use `cat` as a simple echo server to verify transport creation works
        let result = StdioTransport::spawn("cat", &[], &HashMap::new()).await;
        // Just verify the transport can be created — cat is available on macOS/Linux
        assert!(result.is_ok());
    }
}
