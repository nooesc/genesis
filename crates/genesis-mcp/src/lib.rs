//! MCP (Model Context Protocol) client for Genesis.
//!
//! Connects to external MCP servers, discovers their tools, and bridges them
//! into the Genesis tool system.

pub mod client;
pub mod protocol;
pub mod server;
pub mod transport;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use genesis_types::ToolDefinition;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

pub use client::{McpClient, McpServerConfig};
pub use protocol::{McpPromptDef, McpResourceDef, PromptGetResult, ResourceReadResult};
pub use server::{run_stdio_server, McpServeConfig, McpServerToolDef, McpToolBackend};

/// Read a single newline-delimited line from an async reader, enforcing a
/// maximum byte limit. Returns `Ok(None)` on EOF.
///
/// Uses the `take(max_bytes + 1)` trick so that an oversized frame that
/// never sends a newline is detected without buffering unbounded data.
pub(crate) async fn read_limited_stdin_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<String>, McpError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    let mut limited = reader.take((max_bytes as u64).saturating_add(1));
    let bytes_read = limited
        .read_until(b'\n', &mut line)
        .await
        .map_err(|e| McpError::Transport(format!("line read error: {e}")))?;

    if bytes_read == 0 {
        return Ok(None);
    }

    if !line.ends_with(b"\n") && bytes_read > max_bytes {
        return Err(McpError::Protocol(format!(
            "incoming frame exceeds {max_bytes} bytes"
        )));
    }

    String::from_utf8(line)
        .map(Some)
        .map_err(|error| McpError::Protocol(format!("incoming frame was not valid UTF-8: {error}")))
}

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

impl McpError {
    /// Returns true for errors that indicate a broken connection worth reconnecting.
    fn is_retriable(&self) -> bool {
        matches!(self, McpError::Transport(_) | McpError::Timeout)
    }
}

/// Manages connections to multiple MCP servers with automatic reconnection.
///
/// The manager connects to all configured servers, aggregates their tool
/// definitions, and routes tool calls to the appropriate server. If a tool
/// call fails due to a transport error, the manager will attempt to reconnect
/// the server and retry the call once.
pub struct McpManager {
    clients: Arc<RwLock<HashMap<String, McpClient>>>,
    configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
}

impl McpManager {
    /// Create a new manager and connect to all configured servers.
    pub async fn connect_all(configs: Vec<McpServerConfig>) -> Self {
        let mut clients = HashMap::new();
        let mut stored_configs = HashMap::new();

        for config in configs {
            let name = config.name().to_owned();
            stored_configs.insert(name.clone(), config.clone());
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
            configs: Arc::new(RwLock::new(stored_configs)),
        }
    }

    /// Attempt to reconnect a server by name.
    ///
    /// Returns `true` if reconnection succeeded.
    async fn reconnect_server(&self, server_name: &str) -> bool {
        let config = {
            let configs = self.configs.read().await;
            configs.get(server_name).cloned()
        };

        let config = match config {
            Some(c) => c,
            None => return false,
        };

        info!(server = server_name, "attempting MCP server reconnection");

        match McpClient::connect(config).await {
            Ok(client) => {
                info!(
                    server = server_name,
                    tools = client.tools().len(),
                    "MCP server reconnected"
                );
                let mut clients = self.clients.write().await;
                clients.insert(server_name.to_owned(), client);
                true
            }
            Err(e) => {
                error!(
                    server = server_name,
                    error = %e,
                    "MCP server reconnection failed"
                );
                false
            }
        }
    }

    /// Returns all tool definitions from all connected servers.
    ///
    /// Tools are prefixed with `mcp_{server_name}__` to avoid naming collisions.
    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let clients = self.clients.read().await;
        clients
            .values()
            .flat_map(|c| c.tool_definitions())
            .collect()
    }

    /// Call a tool by its prefixed name (e.g., `mcp_filesystem__read_file`).
    ///
    /// Parses the prefix to route to the correct server. If the call fails
    /// with a transport error, attempts to reconnect and retry once.
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String, McpError> {
        let (server_name, tool_name) = parse_mcp_tool_name(prefixed_name)?;

        // First attempt.
        // Clone `arguments` because `call_tool` takes ownership, and we may
        // need the value again for the retry path below. Moving the clone to
        // the retry branch isn't feasible: the first call consumes `arguments`
        // inside this block, so we must preserve a copy before entering it.
        let first_err = {
            let clients = self.clients.read().await;
            let client = clients
                .get(server_name)
                .ok_or_else(|| McpError::UnknownTool(prefixed_name.to_owned()))?;

            match client.call_tool(tool_name, arguments.clone()).await {
                Ok(result) => return Ok(result),
                Err(e) => e,
            }
        };

        // Only retry on transport/timeout errors
        if !first_err.is_retriable() {
            return Err(first_err);
        }

        warn!(
            server = server_name,
            tool = tool_name,
            error = %first_err,
            "tool call failed, attempting reconnect"
        );

        // Reconnect and retry once
        if !self.reconnect_server(server_name).await {
            return Err(first_err);
        }

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

    /// Returns all resource definitions from all connected servers.
    ///
    /// Resources are returned with their server name for routing.
    pub async fn resource_definitions(&self) -> Vec<(String, McpResourceDef)> {
        let clients = self.clients.read().await;
        clients
            .iter()
            .flat_map(|(name, c)| c.resources().iter().map(move |r| (name.clone(), r.clone())))
            .collect()
    }

    /// Returns all prompt definitions from all connected servers.
    ///
    /// Prompts are returned with their server name for routing.
    pub async fn prompt_definitions(&self) -> Vec<(String, McpPromptDef)> {
        let clients = self.clients.read().await;
        clients
            .iter()
            .flat_map(|(name, c)| c.prompts().iter().map(move |p| (name.clone(), p.clone())))
            .collect()
    }

    /// Read a resource from a specific server.
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<ResourceReadResult, McpError> {
        let first_err = {
            let clients = self.clients.read().await;
            let client = clients
                .get(server_name)
                .ok_or_else(|| McpError::UnknownTool(format!("server {server_name}")))?;
            match client.read_resource(uri).await {
                Ok(result) => return Ok(result),
                Err(e) => e,
            }
        };

        if !first_err.is_retriable() {
            return Err(first_err);
        }

        if !self.reconnect_server(server_name).await {
            return Err(first_err);
        }

        let clients = self.clients.read().await;
        let client = clients
            .get(server_name)
            .ok_or_else(|| McpError::UnknownTool(format!("server {server_name}")))?;
        client.read_resource(uri).await
    }

    /// Get a prompt from a specific server.
    pub async fn get_prompt(
        &self,
        server_name: &str,
        prompt_name: &str,
        arguments: Option<HashMap<String, String>>,
    ) -> Result<PromptGetResult, McpError> {
        let first_err = {
            let clients = self.clients.read().await;
            let client = clients
                .get(server_name)
                .ok_or_else(|| McpError::UnknownTool(format!("server {server_name}")))?;
            match client.get_prompt(prompt_name, arguments.clone()).await {
                Ok(result) => return Ok(result),
                Err(e) => e,
            }
        };

        if !first_err.is_retriable() {
            return Err(first_err);
        }

        if !self.reconnect_server(server_name).await {
            return Err(first_err);
        }

        let clients = self.clients.read().await;
        let client = clients
            .get(server_name)
            .ok_or_else(|| McpError::UnknownTool(format!("server {server_name}")))?;
        client.get_prompt(prompt_name, arguments).await
    }

    /// Returns the names of all configured servers and whether they are connected.
    pub async fn server_status(&self) -> Vec<(String, bool)> {
        let configs = self.configs.read().await;
        let clients = self.clients.read().await;
        configs
            .keys()
            .map(|name| (name.clone(), clients.contains_key(name)))
            .collect()
    }

    /// Attempt to reconnect all servers that failed initial connection.
    pub async fn reconnect_failed(&self) -> usize {
        let disconnected: Vec<String> = {
            let configs = self.configs.read().await;
            let clients = self.clients.read().await;
            configs
                .keys()
                .filter(|name| !clients.contains_key(*name))
                .cloned()
                .collect()
        };

        let mut reconnected = 0;
        for name in disconnected {
            if self.reconnect_server(&name).await {
                reconnected += 1;
            }
        }
        reconnected
    }
}

/// Parse a prefixed tool name like `mcp_filesystem__read_file` into
/// `("filesystem", "read_file")`.
///
/// Uses a double-underscore (`__`) separator between the server name and
/// tool name so that server names containing underscores (e.g. `my_server`)
/// are parsed unambiguously.
fn parse_mcp_tool_name(prefixed: &str) -> Result<(&str, &str), McpError> {
    let rest = prefixed
        .strip_prefix("mcp_")
        .ok_or_else(|| McpError::UnknownTool(prefixed.to_owned()))?;

    let sep_pos = rest
        .find("__")
        .ok_or_else(|| McpError::UnknownTool(prefixed.to_owned()))?;

    let server_name = &rest[..sep_pos];
    let tool_name = &rest[sep_pos + 2..];

    if server_name.is_empty() || tool_name.is_empty() {
        return Err(McpError::UnknownTool(prefixed.to_owned()));
    }

    Ok((server_name, tool_name))
}

/// Build MCP server configs from a map of name → raw config.
///
/// This is the bridge between genesis-config's serde-friendly `McpServerConfig`
/// and the runtime `McpServerConfig` enum used by the client. Supports both
/// stdio (command-based) and HTTP (url-based) transports.
pub fn build_server_configs(
    servers: &HashMap<String, genesis_config::McpServerConfig>,
) -> Vec<McpServerConfig> {
    servers
        .iter()
        .filter_map(|(name, entry)| {
            let connect_timeout = Duration::from_secs(entry.connect_timeout.unwrap_or(60));
            let call_timeout = Duration::from_secs(entry.timeout.unwrap_or(120));

            if let Some(command) = &entry.command {
                // Stdio transport
                Some(McpServerConfig::Stdio {
                    name: name.clone(),
                    command: command.clone(),
                    args: entry.args.clone().unwrap_or_default(),
                    env: entry.env.clone().unwrap_or_default(),
                    connect_timeout,
                    call_timeout,
                })
            } else {
                entry.url.as_ref().map(|url| McpServerConfig::Http {
                    name: name.clone(),
                    url: url.clone(),
                    headers: entry.headers.clone().unwrap_or_default(),
                    connect_timeout,
                    call_timeout,
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_name_valid() {
        let (server, tool) = parse_mcp_tool_name("mcp_filesystem__read_file").unwrap();
        assert_eq!(server, "filesystem");
        assert_eq!(tool, "read_file");
    }

    #[test]
    fn parse_tool_name_with_underscores_in_tool() {
        let (server, tool) = parse_mcp_tool_name("mcp_github__create_pull_request").unwrap();
        assert_eq!(server, "github");
        assert_eq!(tool, "create_pull_request");
    }

    #[test]
    fn parse_tool_name_with_underscores_in_server() {
        let (server, tool) = parse_mcp_tool_name("mcp_my_server__read_file").unwrap();
        assert_eq!(server, "my_server");
        assert_eq!(tool, "read_file");
    }

    #[test]
    fn parse_tool_name_no_prefix() {
        assert!(parse_mcp_tool_name("read_file").is_err());
    }

    #[test]
    fn parse_tool_name_no_double_underscore() {
        // Single underscore should fail now
        assert!(parse_mcp_tool_name("mcp_filesystem_read_file").is_err());
    }

    #[test]
    fn parse_tool_name_empty_parts() {
        assert!(parse_mcp_tool_name("mcp_").is_err());
        assert!(parse_mcp_tool_name("mcp___tool").is_err()); // empty server name
        assert!(parse_mcp_tool_name("mcp_server__").is_err()); // empty tool name
    }

    #[test]
    fn build_server_configs_handles_both_transports() {
        let mut servers = HashMap::new();
        servers.insert(
            "fs".to_owned(),
            genesis_config::McpServerConfig {
                command: Some("npx".to_owned()),
                args: Some(vec![
                    "-y".to_owned(),
                    "@modelcontextprotocol/server-filesystem".to_owned(),
                ]),
                env: None,
                url: None,
                headers: None,
                timeout: Some(180),
                connect_timeout: None,
            },
        );
        servers.insert(
            "remote".to_owned(),
            genesis_config::McpServerConfig {
                command: None,
                args: None,
                env: None,
                url: Some("https://example.com/mcp".to_owned()),
                headers: Some(HashMap::from([(
                    "Authorization".to_owned(),
                    "Bearer sk-xxx".to_owned(),
                )])),
                timeout: None,
                connect_timeout: None,
            },
        );

        let configs = build_server_configs(&servers);
        assert_eq!(configs.len(), 2);

        // Find stdio config
        let stdio = configs.iter().find(|c| c.name() == "fs").unwrap();
        assert_eq!(stdio.call_timeout(), Duration::from_secs(180));
        assert_eq!(stdio.connect_timeout(), Duration::from_secs(60));
        assert!(matches!(stdio, McpServerConfig::Stdio { .. }));

        // Find HTTP config
        let http = configs.iter().find(|c| c.name() == "remote").unwrap();
        assert_eq!(http.call_timeout(), Duration::from_secs(120));
        assert!(matches!(http, McpServerConfig::Http { .. }));
    }

    #[test]
    fn build_server_configs_skips_entries_without_command_or_url() {
        let mut servers = HashMap::new();
        servers.insert(
            "empty".to_owned(),
            genesis_config::McpServerConfig {
                command: None,
                args: None,
                env: None,
                url: None,
                headers: None,
                timeout: None,
                connect_timeout: None,
            },
        );

        let configs = build_server_configs(&servers);
        assert!(configs.is_empty());
    }

    #[test]
    fn build_server_configs_defaults_timeouts() {
        let mut servers = HashMap::new();
        servers.insert(
            "test".to_owned(),
            genesis_config::McpServerConfig {
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
        assert_eq!(configs[0].call_timeout(), Duration::from_secs(120));
        assert_eq!(configs[0].connect_timeout(), Duration::from_secs(60));
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
        let result = manager.call_tool("mcp_nonexistent__tool", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn manager_server_status_shows_configured_and_connected() {
        // With no configs, status is empty
        let manager = McpManager::connect_all(vec![]).await;
        assert!(manager.server_status().await.is_empty());
    }

    #[tokio::test]
    async fn manager_reconnect_failed_with_no_servers() {
        let manager = McpManager::connect_all(vec![]).await;
        assert_eq!(manager.reconnect_failed().await, 0);
    }

    #[test]
    fn transport_errors_are_retriable() {
        assert!(McpError::Transport("broken".into()).is_retriable());
        assert!(McpError::Timeout.is_retriable());
        assert!(!McpError::ToolCallFailed("bad args".into()).is_retriable());
        assert!(!McpError::UnknownTool("x".into()).is_retriable());
        assert!(!McpError::Protocol("parse fail".into()).is_retriable());
    }

    #[tokio::test]
    async fn manager_stores_configs_for_reconnection() {
        // Connect with a config that will fail (nonexistent command)
        let config = McpServerConfig::Stdio {
            name: "broken".to_owned(),
            command: "/nonexistent/binary".to_owned(),
            args: vec![],
            env: HashMap::new(),
            connect_timeout: Duration::from_secs(5),
            call_timeout: Duration::from_secs(5),
        };
        let manager = McpManager::connect_all(vec![config]).await;

        // Server is configured but not connected
        let status = manager.server_status().await;
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].0, "broken");
        assert!(!status[0].1); // not connected

        // Reconnect attempt should fail (still nonexistent)
        assert!(!manager.reconnect_server("broken").await);

        // reconnect_failed should try and fail
        assert_eq!(manager.reconnect_failed().await, 0);
    }
}
