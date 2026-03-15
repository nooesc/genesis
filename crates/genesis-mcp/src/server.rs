//! MCP Server — expose Genesis tools via the Model Context Protocol.
//!
//! Runs as a stdio JSON-RPC server that MCP clients (Claude Desktop, VS Code
//! extensions, other agents) can connect to and call Genesis tools.
//!
//! ## Supported methods
//!
//! - `initialize` — handshake with protocol version and capabilities
//! - `notifications/initialized` — client notification (ignored)
//! - `tools/list` — enumerate available tools
//! - `tools/call` — execute a tool and return the result

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

use crate::protocol::{
    Implementation, InitializeResult, JsonRpcError,
    ServerCapabilities, ToolsCapability,
};
use crate::read_limited_stdin_line;

const MAX_STDIN_FRAME_BYTES: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Server types
// ---------------------------------------------------------------------------

/// JSON-RPC request as received by the server (needs Deserialize, unlike the
/// client's request which uses Serialize).
#[derive(Debug, Clone, Deserialize)]
struct IncomingRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    id: Option<u64>,
    method: String,
    params: Option<Value>,
}

/// JSON-RPC response to send back to the client.
#[derive(Debug, Clone, Serialize)]
struct OutgoingResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

impl OutgoingResponse {
    fn success(id: Option<u64>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<u64>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Trait for the tool backend that the MCP server dispatches to.
///
/// This abstracts over `genesis_tools::ToolRegistry` so the server module
/// doesn't directly depend on genesis-tools.
pub trait McpToolBackend: Send + Sync {
    /// List all available tool definitions.
    fn list_tools(&self) -> Vec<McpServerToolDef>;

    /// Execute a tool call by name with JSON arguments.
    /// Returns the text content of the tool output, or an error message.
    fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<String, String>;
}

/// Tool definition as exposed by the MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerToolDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

// ---------------------------------------------------------------------------
// Server configuration
// ---------------------------------------------------------------------------

/// Configuration for the MCP server.
pub struct McpServeConfig {
    /// Server name reported during initialization.
    pub name: String,
    /// Server version reported during initialization.
    pub version: String,
}

impl Default for McpServeConfig {
    fn default() -> Self {
        Self {
            name: "genesis".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// Server main loop
// ---------------------------------------------------------------------------

/// Run the MCP server on stdin/stdout.
///
/// Reads newline-delimited JSON-RPC requests from stdin and writes responses
/// to stdout. Runs until stdin is closed (EOF) or an unrecoverable error occurs.
pub async fn run_stdio_server(
    config: McpServeConfig,
    backend: Arc<dyn McpToolBackend>,
) -> Result<(), crate::McpError> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    info!(
        name = %config.name,
        version = %config.version,
        "MCP server starting on stdio"
    );

    loop {
        line.clear();
        match read_limited_stdin_line(&mut reader, MAX_STDIN_FRAME_BYTES).await {
            Ok(Some(next_line)) => {
                line = next_line;
            }
            Ok(None) => {
                info!("MCP server: stdin closed, shutting down");
                break;
            }
            Err(e) => {
                error!(error = %e, "MCP server: invalid frame");
                return Err(crate::McpError::Protocol(format!("invalid frame: {e}")));
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: IncomingRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "MCP server: invalid JSON-RPC request");
                let resp = OutgoingResponse::error(None, -32700, format!("Parse error: {e}"));
                write_response(&mut stdout, &resp).await?;
                continue;
            }
        };

        debug!(method = %request.method, id = ?request.id, "MCP server: received request");

        let response = handle_request(&config, &*backend, &request);

        // Notifications (no id) don't get a response
        if request.id.is_none() {
            continue;
        }

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}

/// Handle a single JSON-RPC request and return a response.
fn handle_request(
    config: &McpServeConfig,
    backend: &dyn McpToolBackend,
    request: &IncomingRequest,
) -> OutgoingResponse {
    match request.method.as_str() {
        "initialize" => handle_initialize(config, request),
        "notifications/initialized" => {
            // Client notification that initialization is complete. No response needed.
            OutgoingResponse::success(request.id, Value::Null)
        }
        "tools/list" => handle_tools_list(backend, request),
        "tools/call" => handle_tools_call(backend, request),
        "ping" => OutgoingResponse::success(request.id, serde_json::json!({})),
        _ => {
            warn!(method = %request.method, "MCP server: unknown method");
            OutgoingResponse::error(
                request.id,
                -32601,
                format!("Method not found: {}", request.method),
            )
        }
    }
}

fn handle_initialize(
    config: &McpServeConfig,
    request: &IncomingRequest,
) -> OutgoingResponse {
    let result = InitializeResult {
        protocol_version: "2024-11-05".to_owned(),
        capabilities: ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(false),
            }),
            resources: None,
            prompts: None,
        },
        server_info: Some(Implementation {
            name: config.name.clone(),
            version: config.version.clone(),
        }),
    };

    match serde_json::to_value(&result) {
        Ok(v) => OutgoingResponse::success(request.id, v),
        Err(e) => OutgoingResponse::error(request.id, -32603, format!("Serialization error: {e}")),
    }
}

fn handle_tools_list(
    backend: &dyn McpToolBackend,
    request: &IncomingRequest,
) -> OutgoingResponse {
    let tools = backend.list_tools();
    OutgoingResponse::success(
        request.id,
        serde_json::json!({ "tools": tools }),
    )
}

fn handle_tools_call(
    backend: &dyn McpToolBackend,
    request: &IncomingRequest,
) -> OutgoingResponse {
    let params = match &request.params {
        Some(p) => p,
        None => {
            return OutgoingResponse::error(
                request.id,
                -32602,
                "Missing params for tools/call",
            );
        }
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return OutgoingResponse::error(
                request.id,
                -32602,
                "Missing 'name' in tools/call params",
            );
        }
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    match backend.call_tool(name, arguments) {
        Ok(content) => {
            let result = serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": content,
                }],
                "isError": false,
            });
            OutgoingResponse::success(request.id, result)
        }
        Err(error_msg) => {
            OutgoingResponse::error(request.id, -32603, error_msg)
        }
    }
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &OutgoingResponse,
) -> Result<(), crate::McpError> {
    let json = serde_json::to_string(response)
        .map_err(|e| crate::McpError::Protocol(format!("failed to serialize response: {e}")))?;

    writer
        .write_all(json.as_bytes())
        .await
        .map_err(|e| crate::McpError::Transport(format!("stdout write error: {e}")))?;

    writer
        .write_all(b"\n")
        .await
        .map_err(|e| crate::McpError::Transport(format!("stdout write error: {e}")))?;

    writer
        .flush()
        .await
        .map_err(|e| crate::McpError::Transport(format!("stdout flush error: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend {
        tools: Vec<McpServerToolDef>,
    }

    impl McpToolBackend for MockBackend {
        fn list_tools(&self) -> Vec<McpServerToolDef> {
            self.tools.clone()
        }

        fn call_tool(
            &self,
            name: &str,
            arguments: Value,
        ) -> Result<String, String> {
            match name {
                "echo" => {
                    let text = arguments
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(empty)");
                    Ok(format!("Echo: {text}"))
                }
                "fail" => Err("intentional failure".to_owned()),
                _ => Err(format!("unknown tool: {name}")),
            }
        }
    }

    fn mock_backend() -> MockBackend {
        MockBackend {
            tools: vec![
                McpServerToolDef {
                    name: "echo".to_owned(),
                    description: Some("Echoes text back".to_owned()),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "text": {"type": "string", "description": "Text to echo"}
                        },
                        "required": ["text"]
                    }),
                },
                McpServerToolDef {
                    name: "fail".to_owned(),
                    description: Some("Always fails".to_owned()),
                    input_schema: serde_json::json!({"type": "object", "properties": {}}),
                },
            ],
        }
    }

    fn make_request(id: u64, method: &str, params: Option<Value>) -> IncomingRequest {
        IncomingRequest {
            _jsonrpc: "2.0".to_owned(),
            id: Some(id),
            method: method.to_owned(),
            params,
        }
    }

    #[test]
    fn initialize_returns_capabilities() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(1, "initialize", Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        })));

        let resp = handle_request(&config, &backend, &req);
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "genesis");
    }

    #[test]
    fn tools_list_returns_tools() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(2, "tools/list", None);

        let resp = handle_request(&config, &backend, &req);
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "echo");
        assert_eq!(tools[1]["name"], "fail");
    }

    #[test]
    fn tools_call_success() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(3, "tools/call", Some(serde_json::json!({
            "name": "echo",
            "arguments": {"text": "hello world"}
        })));

        let resp = handle_request(&config, &backend, &req);
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], false);
        let content = result["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Echo: hello world");
    }

    #[test]
    fn tools_call_error() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(4, "tools/call", Some(serde_json::json!({
            "name": "fail",
            "arguments": {}
        })));

        let resp = handle_request(&config, &backend, &req);
        assert!(resp.error.is_some());
        let error = resp.error.unwrap();
        assert_eq!(error.code, -32603);
        assert!(error.message.contains("intentional failure"));
    }

    #[test]
    fn tools_call_missing_name() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(5, "tools/call", Some(serde_json::json!({
            "arguments": {"foo": "bar"}
        })));

        let resp = handle_request(&config, &backend, &req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn unknown_method_returns_error() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(6, "resources/list", None);

        let resp = handle_request(&config, &backend, &req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn ping_returns_empty() {
        let config = McpServeConfig::default();
        let backend = mock_backend();
        let req = make_request(7, "ping", None);

        let resp = handle_request(&config, &backend, &req);
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap(), serde_json::json!({}));
    }

    #[test]
    fn notification_has_no_id() {
        let req = IncomingRequest {
            _jsonrpc: "2.0".to_owned(),
            id: None,
            method: "notifications/initialized".to_owned(),
            params: None,
        };
        // Notifications should not produce a response sent back to the client
        assert!(req.id.is_none());
    }

    #[test]
    fn outgoing_response_serializes_cleanly() {
        let resp = OutgoingResponse::success(Some(1), serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(!json.contains("\"error\""));

        let resp = OutgoingResponse::error(Some(2), -32600, "bad request");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(!json.contains("\"result\""));
    }
}
