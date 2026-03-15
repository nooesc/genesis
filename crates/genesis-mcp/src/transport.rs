//! MCP transport implementations.
//!
//! Supports stdio transport (subprocess via stdin/stdout) and HTTP transport
//! (Streamable HTTP via JSON-RPC over POST requests).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, warn};

use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::{read_limited_stdin_line, McpError};

const MAX_STDIN_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Trait for MCP transport implementations.
///
/// Both stdio and HTTP transports implement this, allowing the client to be
/// transport-agnostic.
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and wait for the response.
    fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<JsonRpcResponse, McpError>> + Send + '_>,
    >;

    /// Send a JSON-RPC notification (no response expected).
    fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<(), McpError>;
}

/// Stdio transport — spawns a subprocess and communicates via stdin/stdout.
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
                    error!(error = %e, "mcp stdin write error");
                    break;
                }
                if let Err(e) = stdin.write_all(b"\n").await {
                    error!(error = %e, "mcp stdin newline error");
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    error!(error = %e, "mcp stdin flush error");
                    break;
                }
            }
        });

        // Reader task: reads JSON-RPC responses from stdout
        let pending_for_reader = Arc::clone(&pending);
        let reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut reader = reader;
            loop {
                match read_limited_stdin_line(&mut reader, MAX_STDIN_FRAME_BYTES).await {
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
                        error!(error = %e, "mcp stdout read error");
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
}

impl McpTransport for StdioTransport {
    fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<JsonRpcResponse, McpError>> + Send + '_>,
    > {
        let method = method.to_owned();
        Box::pin(async move {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let request = JsonRpcRequest::new(id, &method, params);

            let json = serde_json::to_string(&request)
                .map_err(|e| McpError::Protocol(format!("failed to serialize request: {e}")))?;
            if json.len() > MAX_STDIN_FRAME_BYTES {
                return Err(McpError::Protocol("JSON-RPC request too large".to_owned()));
            }

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
                    let mut map = self.pending.lock().await;
                    map.remove(&id);
                    Err(McpError::Timeout)
                }
            }
        })
    }

    fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<(), McpError> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let json = serde_json::to_string(&msg)
            .map_err(|e| McpError::Protocol(format!("failed to serialize notification: {e}")))?;
        if json.len() > MAX_STDIN_FRAME_BYTES {
            return Err(McpError::Protocol(
                "JSON-RPC notification too large".to_owned(),
            ));
        }

        self.outgoing_tx
            .send(json)
            .map_err(|_| McpError::Transport("writer channel closed".into()))?;

        Ok(())
    }
}

/// HTTP transport — communicates with MCP servers via HTTP POST (Streamable HTTP).
///
/// Each JSON-RPC request is sent as a POST to the server URL with
/// `Content-Type: application/json`. The server responds with a JSON-RPC
/// response in the body.
pub struct HttpTransport {
    http: reqwest::Client,
    url: String,
    next_id: AtomicU64,
}

impl HttpTransport {
    /// Create a new HTTP transport for the given MCP server URL.
    pub fn new(url: &str, headers: &HashMap<String, String>) -> Result<Self, McpError> {
        let mut header_map = reqwest::header::HeaderMap::new();
        header_map.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json"
                .parse()
                .map_err(|e| McpError::Transport(format!("invalid content-type header: {e}")))?,
        );
        for (key, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| McpError::Transport(format!("invalid header name `{key}`: {e}")))?;
            let val = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                McpError::Transport(format!("invalid header value for `{key}`: {e}"))
            })?;
            header_map.insert(name, val);
        }

        let http = reqwest::Client::builder()
            .default_headers(header_map)
            .user_agent("genesis-mcp/0.1")
            .build()
            .map_err(|e| McpError::Transport(format!("failed to create HTTP client: {e}")))?;

        Ok(Self {
            http,
            url: url.to_owned(),
            next_id: AtomicU64::new(1),
        })
    }
}

async fn read_http_body_bytes(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, McpError> {
    let mut body = Vec::new();
    let mut downloaded = 0usize;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            McpError::Protocol(format!("failed to read HTTP response body: {error}"))
        })?;
        if downloaded.saturating_add(chunk.len()) > max_bytes {
            return Err(McpError::Protocol(format!(
                "HTTP response body exceeds {max_bytes} bytes"
            )));
        }
        downloaded = downloaded.saturating_add(chunk.len());
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

async fn read_http_body_text(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<String, McpError> {
    let bytes = read_http_body_bytes(response, max_bytes).await?;
    String::from_utf8(bytes).map_err(|error| {
        McpError::Protocol(format!("HTTP response body was not valid UTF-8: {error}"))
    })
}

async fn read_http_response_json(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<JsonRpcResponse, McpError> {
    let body = read_http_body_bytes(response, max_bytes).await?;
    serde_json::from_slice(&body)
        .map_err(|e| McpError::Protocol(format!("failed to parse JSON-RPC response: {e}")))
}

impl McpTransport for HttpTransport {
    fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<JsonRpcResponse, McpError>> + Send + '_>,
    > {
        let method = method.to_owned();
        Box::pin(async move {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let request = JsonRpcRequest::new(id, &method, params);

            let response = self
                .http
                .post(&self.url)
                .timeout(timeout)
                .json(&request)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        McpError::Timeout
                    } else {
                        McpError::Transport(format!("HTTP request failed: {e}"))
                    }
                })?;

            let status = response.status();
            if !status.is_success() {
                let body = read_http_body_text(response, MAX_HTTP_RESPONSE_BYTES)
                    .await
                    .unwrap_or_else(|error| format!("[unreadable response body: {error}]"));
                return Err(McpError::Transport(format!("HTTP {status}: {body}")));
            }

            read_http_response_json(response, MAX_HTTP_RESPONSE_BYTES).await
        })
    }

    fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<(), McpError> {
        // For HTTP transport, notifications are fire-and-forget POSTs.
        // We spawn a background task since the trait method is sync.
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let http = self.http.clone();
        let url = self.url.clone();
        tokio::spawn(async move {
            let _ = http
                .post(&url)
                .json(&msg)
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        });

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

    #[test]
    fn http_transport_creates_with_empty_headers() {
        let transport = HttpTransport::new("http://localhost:8080/mcp", &HashMap::new());
        assert!(transport.is_ok());
    }

    #[test]
    fn http_transport_creates_with_custom_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_owned(), "Bearer sk-test".to_owned());
        let transport = HttpTransport::new("http://localhost:8080/mcp", &headers);
        assert!(transport.is_ok());
    }

    #[test]
    fn http_transport_rejects_invalid_header_name() {
        let mut headers = HashMap::new();
        headers.insert("invalid header\x00name".to_owned(), "value".to_owned());
        let transport = HttpTransport::new("http://localhost:8080/mcp", &headers);
        assert!(transport.is_err());
    }
}
