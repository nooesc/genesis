//! MCP protocol message types (JSON-RPC 2.0 based).

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize)]
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

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 notification (no id).
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
}

// --- MCP Initialize ---

#[derive(Debug, Clone, Serialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: Implementation,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ClientCapabilities {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: Option<Implementation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerCapabilities {
    pub tools: Option<ToolsCapability>,
    pub resources: Option<Value>,
    pub prompts: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: Option<bool>,
}

// --- MCP Tools ---

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<McpToolDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallParams {
    pub name: String,
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<ContentPart>,
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: Option<String>,
}

// --- MCP Resources ---

#[derive(Debug, Clone, Deserialize)]
pub struct ResourcesListResult {
    pub resources: Vec<McpResourceDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceDef {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceReadParams {
    pub uri: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub blob: Option<String>,
}

// --- MCP Prompts ---

#[derive(Debug, Clone, Deserialize)]
pub struct PromptsListResult {
    pub prompts: Vec<McpPromptDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptGetParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptGetResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: PromptContent,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_params() {
        let req = JsonRpcRequest::new(1, "initialize", Some(serde_json::json!({"key": "val"})));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("\"key\":\"val\""));
    }

    #[test]
    fn request_serializes_without_params() {
        let req = JsonRpcRequest::new(2, "notifications/initialized", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"params\""));
    }

    #[test]
    fn response_deserializes_result() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn response_deserializes_error() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "Invalid Request");
    }

    #[test]
    fn initialize_params_serializes() {
        let params = InitializeParams {
            protocol_version: "2024-11-05".to_owned(),
            capabilities: ClientCapabilities {},
            client_info: Implementation {
                name: "genesis".to_owned(),
                version: "0.1.0".to_owned(),
            },
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["protocolVersion"], "2024-11-05");
        assert_eq!(json["clientInfo"]["name"], "genesis");
    }

    #[test]
    fn tools_list_result_deserializes() {
        let json = r#"{
            "tools": [{
                "name": "read_file",
                "description": "Read a file",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }
            }]
        }"#;
        let result: ToolsListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "read_file");
        assert!(result.tools[0].input_schema.is_some());
    }

    #[test]
    fn tool_call_result_deserializes() {
        let json = r#"{
            "content": [{"type": "text", "text": "file contents here"}],
            "isError": false
        }"#;
        let result: ToolCallResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("file contents here"));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn tool_call_params_serializes() {
        let params = ToolCallParams {
            name: "read_file".to_owned(),
            arguments: Some(serde_json::json!({"path": "/tmp/test.txt"})),
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["name"], "read_file");
        assert_eq!(json["arguments"]["path"], "/tmp/test.txt");
    }

    #[test]
    fn resources_list_result_deserializes() {
        let json = r#"{
            "resources": [{
                "uri": "file:///etc/hosts",
                "name": "hosts",
                "description": "System hosts file",
                "mimeType": "text/plain"
            }]
        }"#;
        let result: ResourcesListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.resources.len(), 1);
        assert_eq!(result.resources[0].uri, "file:///etc/hosts");
        assert_eq!(result.resources[0].name, "hosts");
        assert_eq!(result.resources[0].mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn resource_read_result_deserializes() {
        let json = r#"{
            "contents": [{
                "uri": "file:///etc/hosts",
                "mimeType": "text/plain",
                "text": "127.0.0.1 localhost"
            }]
        }"#;
        let result: ResourceReadResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.contents.len(), 1);
        assert_eq!(result.contents[0].text.as_deref(), Some("127.0.0.1 localhost"));
    }

    #[test]
    fn prompts_list_result_deserializes() {
        let json = r#"{
            "prompts": [{
                "name": "code-review",
                "description": "Review code for bugs",
                "arguments": [{
                    "name": "language",
                    "description": "Programming language",
                    "required": true
                }]
            }]
        }"#;
        let result: PromptsListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.prompts.len(), 1);
        assert_eq!(result.prompts[0].name, "code-review");
        let args = result.prompts[0].arguments.as_ref().unwrap();
        assert_eq!(args[0].name, "language");
        assert_eq!(args[0].required, Some(true));
    }

    #[test]
    fn prompt_get_result_deserializes() {
        let json = r#"{
            "description": "A code review prompt",
            "messages": [{
                "role": "user",
                "content": {
                    "type": "text",
                    "text": "Please review this code"
                }
            }]
        }"#;
        let result: PromptGetResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.description.as_deref(), Some("A code review prompt"));
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[0].content.text.as_deref(), Some("Please review this code"));
    }

    #[test]
    fn prompt_get_params_serializes() {
        let params = PromptGetParams {
            name: "code-review".to_owned(),
            arguments: Some(HashMap::from([("language".to_owned(), "rust".to_owned())])),
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["name"], "code-review");
        assert_eq!(json["arguments"]["language"], "rust");
    }

    #[test]
    fn resource_read_params_serializes() {
        let params = ResourceReadParams {
            uri: "file:///etc/hosts".to_owned(),
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["uri"], "file:///etc/hosts");
    }
}
