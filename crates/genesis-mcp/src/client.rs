//! MCP client — connects to an MCP server, discovers tools, and calls them.

use std::collections::HashMap;
use std::time::Duration;

use genesis_types::ToolDefinition;
use serde_json::json;
use tracing::{debug, info};

use crate::protocol::{
    ClientCapabilities, Implementation, InitializeParams, InitializeResult, McpPromptDef,
    McpResourceDef, McpToolDef, PromptGetParams, PromptGetResult, PromptsListResult,
    ResourceReadParams, ResourceReadResult, ResourcesListResult, ToolCallParams, ToolCallResult,
    ToolsListResult,
};
use crate::transport::{HttpTransport, McpTransport, StdioTransport};
use crate::McpError;

const PROTOCOL_VERSION: &str = "2024-11-05";
const CLIENT_NAME: &str = "genesis";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Configuration for connecting to a single MCP server.
#[derive(Debug, Clone)]
pub enum McpServerConfig {
    /// Stdio transport: spawn a subprocess and communicate via stdin/stdout.
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        connect_timeout: Duration,
        call_timeout: Duration,
    },
    /// HTTP transport: communicate via JSON-RPC over HTTP POST.
    Http {
        name: String,
        url: String,
        headers: HashMap<String, String>,
        connect_timeout: Duration,
        call_timeout: Duration,
    },
}

impl McpServerConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio { name, .. } | Self::Http { name, .. } => name,
        }
    }

    pub fn connect_timeout(&self) -> Duration {
        match self {
            Self::Stdio {
                connect_timeout, ..
            }
            | Self::Http {
                connect_timeout, ..
            } => *connect_timeout,
        }
    }

    pub fn call_timeout(&self) -> Duration {
        match self {
            Self::Stdio { call_timeout, .. } | Self::Http { call_timeout, .. } => *call_timeout,
        }
    }
}

/// A connected MCP client for a single server.
pub struct McpClient {
    name: String,
    transport: Box<dyn McpTransport>,
    call_timeout: Duration,
    tools: Vec<McpToolDef>,
    resources: Vec<McpResourceDef>,
    prompts: Vec<McpPromptDef>,
}

impl McpClient {
    /// Connect to an MCP server, perform the initialize handshake, and discover tools.
    pub async fn connect(config: McpServerConfig) -> Result<Self, McpError> {
        let name = config.name().to_owned();
        let connect_timeout = config.connect_timeout();
        let call_timeout = config.call_timeout();

        let transport: Box<dyn McpTransport> = match &config {
            McpServerConfig::Stdio {
                command, args, env, ..
            } => {
                info!(
                    server = name.as_str(),
                    command = command.as_str(),
                    "connecting to MCP server via stdio"
                );
                Box::new(StdioTransport::spawn(command, args, env).await?)
            }
            McpServerConfig::Http { url, headers, .. } => {
                info!(
                    server = name.as_str(),
                    url = url.as_str(),
                    "connecting to MCP server via HTTP"
                );
                Box::new(HttpTransport::new(url, headers)?)
            }
        };

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
                connect_timeout,
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
            server = name.as_str(),
            protocol_version = init_result.protocol_version.as_str(),
            server_name = init_result.server_info.as_ref().map(|s| s.name.as_str()),
            "MCP server initialized"
        );

        // Send initialized notification
        transport.notify("notifications/initialized", None)?;

        // Discover tools
        let tools = if init_result.capabilities.tools.is_some() {
            let tools_resp = transport
                .request("tools/list", Some(json!({})), connect_timeout)
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
                server = name.as_str(),
                count = tools_result.tools.len(),
                "discovered MCP tools"
            );
            tools_result.tools
        } else {
            debug!(
                server = name.as_str(),
                "server does not advertise tool capabilities"
            );
            Vec::new()
        };

        // Discover resources
        let resources = if init_result.capabilities.resources.is_some() {
            let res_resp = transport
                .request("resources/list", Some(json!({})), connect_timeout)
                .await?;

            if let Some(err) = res_resp.error {
                debug!(
                    server = name.as_str(),
                    error = err.message.as_str(),
                    "resources/list returned error, skipping"
                );
                Vec::new()
            } else {
                let res_result: ResourcesListResult = res_resp
                    .result
                    .ok_or_else(|| McpError::Protocol("resources/list missing result".into()))
                    .and_then(|v| {
                        serde_json::from_value(v).map_err(|e| {
                            McpError::Protocol(format!("failed to parse resources: {e}"))
                        })
                    })?;

                info!(
                    server = name.as_str(),
                    count = res_result.resources.len(),
                    "discovered MCP resources"
                );
                res_result.resources
            }
        } else {
            Vec::new()
        };

        // Discover prompts
        let prompts = if init_result.capabilities.prompts.is_some() {
            let pr_resp = transport
                .request("prompts/list", Some(json!({})), connect_timeout)
                .await?;

            if let Some(err) = pr_resp.error {
                debug!(
                    server = name.as_str(),
                    error = err.message.as_str(),
                    "prompts/list returned error, skipping"
                );
                Vec::new()
            } else {
                let pr_result: PromptsListResult = pr_resp
                    .result
                    .ok_or_else(|| McpError::Protocol("prompts/list missing result".into()))
                    .and_then(|v| {
                        serde_json::from_value(v).map_err(|e| {
                            McpError::Protocol(format!("failed to parse prompts: {e}"))
                        })
                    })?;

                info!(
                    server = name.as_str(),
                    count = pr_result.prompts.len(),
                    "discovered MCP prompts"
                );
                pr_result.prompts
            }
        } else {
            Vec::new()
        };

        Ok(Self {
            name,
            transport,
            call_timeout,
            tools,
            resources,
            prompts,
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

    /// Convert MCP tools to Genesis ToolDefinitions, prefixed with `mcp_{server_name}__`.
    ///
    /// Uses a double-underscore separator between the server name and tool name
    /// so that server names containing underscores (e.g. `my_server`) can be
    /// unambiguously parsed back from the prefixed form.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: format!("mcp_{}__{}", self.name, t.name),
                description: t
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("MCP tool from {}", self.name)),
                parameters: t.input_schema.clone(),
            })
            .collect()
    }

    /// Returns the discovered MCP resource definitions.
    pub fn resources(&self) -> &[McpResourceDef] {
        &self.resources
    }

    /// Returns the discovered MCP prompt definitions.
    pub fn prompts(&self) -> &[McpPromptDef] {
        &self.prompts
    }

    /// Read a resource by URI from this MCP server.
    pub async fn read_resource(&self, uri: &str) -> Result<ResourceReadResult, McpError> {
        let params = ResourceReadParams {
            uri: uri.to_owned(),
        };

        let resp = self
            .transport
            .request(
                "resources/read",
                Some(serde_json::to_value(&params).map_err(|e| {
                    McpError::Protocol(format!("failed to serialize resource read: {e}"))
                })?),
                self.call_timeout,
            )
            .await?;

        if let Some(err) = resp.error {
            return Err(McpError::ToolCallFailed(format!(
                "resources/read {}: {} (code {})",
                uri, err.message, err.code
            )));
        }

        resp.result
            .ok_or_else(|| McpError::Protocol("resources/read missing result".into()))
            .and_then(|v| {
                serde_json::from_value(v).map_err(|e| {
                    McpError::Protocol(format!("failed to parse resource read result: {e}"))
                })
            })
    }

    /// Get a prompt by name from this MCP server, with optional arguments.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<HashMap<String, String>>,
    ) -> Result<PromptGetResult, McpError> {
        let params = PromptGetParams {
            name: name.to_owned(),
            arguments,
        };

        let resp = self
            .transport
            .request(
                "prompts/get",
                Some(serde_json::to_value(&params).map_err(|e| {
                    McpError::Protocol(format!("failed to serialize prompt get: {e}"))
                })?),
                self.call_timeout,
            )
            .await?;

        if let Some(err) = resp.error {
            return Err(McpError::ToolCallFailed(format!(
                "prompts/get {}: {} (code {})",
                name, err.message, err.code
            )));
        }

        resp.result
            .ok_or_else(|| McpError::Protocol("prompts/get missing result".into()))
            .and_then(|v| {
                serde_json::from_value(v).map_err(|e| {
                    McpError::Protocol(format!("failed to parse prompt get result: {e}"))
                })
            })
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

        let output = result
            .content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_error == Some(true) {
            return Err(McpError::ToolCallFailed(output));
        }

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
                name: format!("mcp_{server_name}__{}", t.name),
                description: t.description.clone().unwrap_or_default(),
                parameters: t.input_schema.clone(),
            })
            .collect();

        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "mcp_filesystem__read_file");
        assert_eq!(defs[0].description, "Read a file");
        assert!(defs[0].parameters.is_some());
    }

    #[test]
    fn stdio_config_accessors() {
        let config = McpServerConfig::Stdio {
            name: "fs".to_owned(),
            command: "npx".to_owned(),
            args: vec![],
            env: HashMap::new(),
            connect_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(60),
        };
        assert_eq!(config.name(), "fs");
        assert_eq!(config.connect_timeout(), Duration::from_secs(30));
        assert_eq!(config.call_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn http_config_accessors() {
        let config = McpServerConfig::Http {
            name: "remote".to_owned(),
            url: "https://example.com/mcp".to_owned(),
            headers: HashMap::new(),
            connect_timeout: Duration::from_secs(10),
            call_timeout: Duration::from_secs(120),
        };
        assert_eq!(config.name(), "remote");
        assert_eq!(config.connect_timeout(), Duration::from_secs(10));
        assert_eq!(config.call_timeout(), Duration::from_secs(120));
    }
}
