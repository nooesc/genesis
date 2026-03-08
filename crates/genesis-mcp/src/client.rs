//! MCP client — connects to an MCP server, discovers tools, and calls them.

use std::collections::HashMap;
use std::time::Duration;

use genesis_types::ToolDefinition;
use serde_json::json;
use tracing::{debug, info};

use crate::protocol::{
    InitializeParams, InitializeResult, ClientCapabilities, Implementation,
    McpToolDef, ToolCallParams, ToolCallResult, ToolsListResult,
};
use crate::transport::StdioTransport;
use crate::McpError;

const PROTOCOL_VERSION: &str = "2024-11-05";
const CLIENT_NAME: &str = "genesis";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Configuration for connecting to a single MCP server.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub connect_timeout: Duration,
    pub call_timeout: Duration,
}

/// A connected MCP client for a single server.
pub struct McpClient {
    name: String,
    transport: StdioTransport,
    call_timeout: Duration,
    tools: Vec<McpToolDef>,
}

impl McpClient {
    /// Connect to an MCP server, perform the initialize handshake, and discover tools.
    pub async fn connect(config: McpServerConfig) -> Result<Self, McpError> {
        info!(
            server = config.name.as_str(),
            command = config.command.as_str(),
            "connecting to MCP server"
        );

        let transport = StdioTransport::spawn(&config.command, &config.args, &config.env).await?;

        // Initialize handshake
        let init_params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: ClientCapabilities {},
            client_info: Implementation {
                name: CLIENT_NAME.to_owned(),
                version: CLIENT_VERSION.to_owned(),
            },
        };

        let resp = transport
            .request(
                "initialize",
                Some(serde_json::to_value(&init_params).map_err(|e| {
                    McpError::Protocol(format!("failed to serialize init params: {e}"))
                })?),
                config.connect_timeout,
            )
            .await?;

        if let Some(err) = resp.error {
            return Err(McpError::Protocol(format!(
                "server returned error on initialize: {} (code {})",
                err.message, err.code
            )));
        }

        let init_result: InitializeResult = resp
            .result
            .ok_or_else(|| McpError::Protocol("initialize response missing result".into()))
            .and_then(|v| {
                serde_json::from_value(v)
                    .map_err(|e| McpError::Protocol(format!("failed to parse init result: {e}")))
            })?;

        debug!(
            server = config.name.as_str(),
            protocol_version = init_result.protocol_version.as_str(),
            server_name = init_result.server_info.as_ref().map(|s| s.name.as_str()),
            "MCP server initialized"
        );

        // Send initialized notification
        transport.notify("notifications/initialized", None)?;

        // Discover tools
        let tools = if init_result.capabilities.tools.is_some() {
            let tools_resp = transport
                .request("tools/list", Some(json!({})), config.connect_timeout)
                .await?;

            if let Some(err) = tools_resp.error {
                return Err(McpError::Protocol(format!(
                    "tools/list error: {} (code {})",
                    err.message, err.code
                )));
            }

            let tools_result: ToolsListResult = tools_resp
                .result
                .ok_or_else(|| McpError::Protocol("tools/list missing result".into()))
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| McpError::Protocol(format!("failed to parse tools: {e}")))
                })?;

            info!(
                server = config.name.as_str(),
                count = tools_result.tools.len(),
                "discovered MCP tools"
            );
            tools_result.tools
        } else {
            debug!(
                server = config.name.as_str(),
                "server does not advertise tool capabilities"
            );
            Vec::new()
        };

        Ok(Self {
            name: config.name,
            transport,
            call_timeout: config.call_timeout,
            tools,
        })
    }

    /// Returns the server name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the discovered MCP tool definitions.
    pub fn tools(&self) -> &[McpToolDef] {
        &self.tools
    }

    /// Convert MCP tools to Genesis ToolDefinitions, prefixed with `mcp_{server_name}_`.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: format!("mcp_{}_{}", self.name, t.name),
                description: t
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("MCP tool from {}", self.name)),
                parameters: t.input_schema.clone(),
            })
            .collect()
    }

    /// Call a tool on this MCP server.
    ///
    /// `tool_name` should be the raw MCP tool name (without the `mcp_{server}_` prefix).
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String, McpError> {
        let params = ToolCallParams {
            name: tool_name.to_owned(),
            arguments,
        };

        let resp = self
            .transport
            .request(
                "tools/call",
                Some(serde_json::to_value(&params).map_err(|e| {
                    McpError::Protocol(format!("failed to serialize tool call: {e}"))
                })?),
                self.call_timeout,
            )
            .await?;

        if let Some(err) = resp.error {
            return Err(McpError::ToolCallFailed(format!(
                "{}: {} (code {})",
                tool_name, err.message, err.code
            )));
        }

        let result: ToolCallResult = resp
            .result
            .ok_or_else(|| McpError::Protocol("tools/call missing result".into()))
            .and_then(|v| {
                serde_json::from_value(v).map_err(|e| {
                    McpError::Protocol(format!("failed to parse tool call result: {e}"))
                })
            })?;

        if result.is_error == Some(true) {
            let text = result
                .content
                .iter()
                .filter_map(|c| c.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(McpError::ToolCallFailed(text));
        }

        let output = result
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_prefixed_with_server_name() {
        let tools = vec![McpToolDef {
            name: "read_file".to_owned(),
            description: Some("Read a file".to_owned()),
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            })),
        }];

        // Can't construct McpClient without a real transport, so test the naming logic directly
        let server_name = "filesystem";
        let defs: Vec<ToolDefinition> = tools
            .iter()
            .map(|t| ToolDefinition {
                name: format!("mcp_{server_name}_{}", t.name),
                description: t.description.clone().unwrap_or_default(),
                parameters: t.input_schema.clone(),
            })
            .collect();

        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "mcp_filesystem_read_file");
        assert_eq!(defs[0].description, "Read a file");
        assert!(defs[0].parameters.is_some());
    }
}
