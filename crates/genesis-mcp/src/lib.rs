//! MCP (Model Context Protocol) client for Genesis.
//!
//! Connects to external MCP servers, discovers their tools, and bridges them
//! into the Genesis tool system.

pub mod client;
pub mod protocol;
pub mod transport;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use genesis_types::ToolDefinition;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{error, info};

pub use client::{McpClient, McpServerConfig};

#[derive(Debug, Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("tool call failed: {0}")]
    ToolCallFailed(String),
    #[error("timeout waiting for MCP server response")]
    Timeout,
    #[error("unknown MCP tool: {0}")]
    UnknownTool(String),
}

/// Manages connections to multiple MCP servers.
///
/// The manager connects to all configured servers, aggregates their tool
/// definitions, and routes tool calls to the appropriate server.
pub struct McpManager {
    clients: Arc<RwLock<HashMap<String, McpClient>>>,
}

impl McpManager {
    /// Create a new manager and connect to all configured servers.
    pub async fn connect_all(configs: Vec<McpServerConfig>) -> Self {
        let mut clients = HashMap::new();

        for config in configs {
            let name = config.name.clone();
            match McpClient::connect(config).await {
                Ok(client) => {
                    info!(
                        server = name.as_str(),
                        tools = client.tools().len(),
                        "MCP server connected"
                    );
                    clients.insert(name, client);
                }
                Err(e) => {
                    error!(
                        server = name.as_str(),
                        error = %e,
                        "failed to connect to MCP server"
                    );
                }
            }
        }

        Self {
            clients: Arc::new(RwLock::new(clients)),
        }
    }

    /// Returns all tool definitions from all connected servers.
    ///
    /// Tools are prefixed with `mcp_{server_name}_` to avoid naming collisions.
    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let clients = self.clients.read().await;
        clients
            .values()
            .flat_map(|c| c.tool_definitions())
            .collect()
    }

    /// Call a tool by its prefixed name (e.g., `mcp_filesystem_read_file`).
    ///
    /// Parses the prefix to route to the correct server.
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String, McpError> {
        let (server_name, tool_name) = parse_mcp_tool_name(prefixed_name)?;

        let clients = self.clients.read().await;
        let client = clients
            .get(server_name)
            .ok_or_else(|| McpError::UnknownTool(prefixed_name.to_owned()))?;

        client.call_tool(tool_name, arguments).await
    }

    /// Returns the number of connected servers.
    pub async fn server_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Returns the total number of tools across all servers.
    pub async fn tool_count(&self) -> usize {
        let clients = self.clients.read().await;
        clients.values().map(|c| c.tools().len()).sum()
    }
}

/// Parse a prefixed tool name like `mcp_filesystem_read_file` into
/// `("filesystem", "read_file")`.
fn parse_mcp_tool_name(prefixed: &str) -> Result<(&str, &str), McpError> {
    let rest = prefixed
        .strip_prefix("mcp_")
        .ok_or_else(|| McpError::UnknownTool(prefixed.to_owned()))?;

    let underscore_pos = rest
        .find('_')
        .ok_or_else(|| McpError::UnknownTool(prefixed.to_owned()))?;

    let server_name = &rest[..underscore_pos];
    let tool_name = &rest[underscore_pos + 1..];

    if server_name.is_empty() || tool_name.is_empty() {
        return Err(McpError::UnknownTool(prefixed.to_owned()));
    }

    Ok((server_name, tool_name))
}

/// Build MCP server configs from a map of name → raw config.
///
/// This is the bridge between genesis-config's `McpServerConfig` (serde) and
/// the runtime `McpServerConfig` used by the client.
pub fn build_server_configs(
    servers: &HashMap<String, McpServerEntry>,
) -> Vec<McpServerConfig> {
    servers
        .iter()
        .filter_map(|(name, entry)| {
            // Only stdio transport supported for now
            let command = entry.command.as_ref()?;
            Some(McpServerConfig {
                name: name.clone(),
                command: command.clone(),
                args: entry.args.clone().unwrap_or_default(),
                env: entry.env.clone().unwrap_or_default(),
                connect_timeout: Duration::from_secs(
                    entry.connect_timeout.unwrap_or(60),
                ),
                call_timeout: Duration::from_secs(entry.timeout.unwrap_or(120)),
            })
        })
        .collect()
}

/// Raw MCP server entry from config file (serde-friendly).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct McpServerEntry {
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_name_valid() {
        let (server, tool) = parse_mcp_tool_name("mcp_filesystem_read_file").unwrap();
        assert_eq!(server, "filesystem");
        assert_eq!(tool, "read_file");
    }

    #[test]
    fn parse_tool_name_with_underscores_in_tool() {
        let (server, tool) = parse_mcp_tool_name("mcp_github_create_pull_request").unwrap();
        assert_eq!(server, "github");
        assert_eq!(tool, "create_pull_request");
    }

    #[test]
    fn parse_tool_name_no_prefix() {
        assert!(parse_mcp_tool_name("read_file").is_err());
    }

    #[test]
    fn parse_tool_name_empty_parts() {
        assert!(parse_mcp_tool_name("mcp_").is_err());
        assert!(parse_mcp_tool_name("mcp__tool").is_err());
    }

    #[test]
    fn build_server_configs_filters_stdio_only() {
        let mut servers = HashMap::new();
        servers.insert(
            "fs".to_owned(),
            McpServerEntry {
                command: Some("npx".to_owned()),
                args: Some(vec!["-y".to_owned(), "@modelcontextprotocol/server-filesystem".to_owned()]),
                env: None,
                url: None,
                headers: None,
                timeout: Some(180),
                connect_timeout: None,
            },
        );
        servers.insert(
            "remote".to_owned(),
            McpServerEntry {
                command: None,
                args: None,
                env: None,
                url: Some("https://example.com/mcp".to_owned()),
                headers: None,
                timeout: None,
                connect_timeout: None,
            },
        );

        let configs = build_server_configs(&servers);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "fs");
        assert_eq!(configs[0].command, "npx");
        assert_eq!(configs[0].call_timeout, Duration::from_secs(180));
        assert_eq!(configs[0].connect_timeout, Duration::from_secs(60));
    }

    #[test]
    fn build_server_configs_defaults_timeouts() {
        let mut servers = HashMap::new();
        servers.insert(
            "test".to_owned(),
            McpServerEntry {
                command: Some("echo".to_owned()),
                args: None,
                env: None,
                url: None,
                headers: None,
                timeout: None,
                connect_timeout: None,
            },
        );

        let configs = build_server_configs(&servers);
        assert_eq!(configs[0].call_timeout, Duration::from_secs(120));
        assert_eq!(configs[0].connect_timeout, Duration::from_secs(60));
    }

    #[test]
    fn mcp_server_entry_deserializes_stdio() {
        let yaml = r#"
            command: npx
            args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
            env:
              TOKEN: abc123
            timeout: 180
        "#;
        let entry: McpServerEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.command.as_deref(), Some("npx"));
        assert_eq!(entry.args.as_ref().unwrap().len(), 3);
        assert_eq!(
            entry.env.as_ref().unwrap().get("TOKEN").unwrap(),
            "abc123"
        );
        assert_eq!(entry.timeout, Some(180));
    }

    #[test]
    fn mcp_server_entry_deserializes_http() {
        let yaml = r#"
            url: https://mcp.example.com/db
            headers:
              Authorization: Bearer sk-xxx
        "#;
        let entry: McpServerEntry = serde_yaml::from_str(yaml).unwrap();
        assert!(entry.command.is_none());
        assert_eq!(entry.url.as_deref(), Some("https://mcp.example.com/db"));
        assert!(entry.headers.is_some());
    }

    #[tokio::test]
    async fn manager_connect_all_handles_empty() {
        let manager = McpManager::connect_all(vec![]).await;
        assert_eq!(manager.server_count().await, 0);
        assert_eq!(manager.tool_count().await, 0);
        assert!(manager.tool_definitions().await.is_empty());
    }

    #[tokio::test]
    async fn manager_call_unknown_tool_errors() {
        let manager = McpManager::connect_all(vec![]).await;
        let result = manager.call_tool("mcp_nonexistent_tool", None).await;
        assert!(result.is_err());
    }
}
