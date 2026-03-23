use std::path::PathBuf;

use genesis_config::load;

use crate::{CliError, McpCommand};

pub(crate) struct RegistryMcpBackend {
    registry: genesis_tools::ToolRegistry,
    context: genesis_tools::ToolContext,
}

impl genesis_mcp::McpToolBackend for RegistryMcpBackend {
    fn list_tools(&self) -> Vec<genesis_mcp::McpServerToolDef> {
        self.registry
            .definitions()
            .into_iter()
            .map(|def| genesis_mcp::McpServerToolDef {
                name: def.name,
                description: Some(def.description),
                input_schema: def
                    .parameters
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}})),
            })
            .collect()
    }

    fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String, String> {
        // Convert JSON arguments to BTreeMap<String, String>
        let mut args = std::collections::BTreeMap::new();
        if let Some(obj) = arguments.as_object() {
            for (k, v) in obj {
                let s = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                args.insert(k.clone(), s);
            }
        }

        let call = genesis_tools::ToolCall {
            name: name.to_owned(),
            arguments: args,
        };

        match self.registry.execute(&call, &self.context) {
            Ok(output) => Ok(output.content),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub(crate) async fn run_mcp(
    config_path: Option<PathBuf>,
    command: McpCommand,
    json: bool,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;

    match command {
        McpCommand::List => {
            let servers = &loaded.config.mcp_servers;
            if servers.is_empty() {
                return Ok("no MCP servers configured".to_owned());
            }

            if json {
                return Ok(serde_json::to_string_pretty(servers)?);
            }

            let mut lines = Vec::new();
            for (name, cfg) in servers {
                let transport = if cfg.command.is_some() {
                    "stdio"
                } else if cfg.url.is_some() {
                    "http"
                } else {
                    "unknown"
                };

                let endpoint = cfg.command.as_deref().or(cfg.url.as_deref()).unwrap_or("-");

                let timeout = cfg.timeout.unwrap_or(120);
                let connect_timeout = cfg.connect_timeout.unwrap_or(60);

                lines.push(format!(
                    "{name}  [{transport}]  {endpoint}  timeout={timeout}s connect={connect_timeout}s"
                ));
            }
            Ok(lines.join("\n"))
        }
        McpCommand::Serve => {
            // Run Genesis as an MCP server on stdio
            let registry = genesis_tools::default_registry();
            let context = genesis_tools::ToolContext {
                session_id: format!("mcp-server-{}", std::process::id()),
                profile: loaded.config.profile.clone(),
                data_dir: loaded.config.storage.data_dir.to_string_lossy().to_string(),
                allow_destructive_tools: false,
                terminal_backend: None,
                default_working_dir: None,
                sandbox_manager: None,
                embedding_service: None,
                path_validator: Some(std::sync::Arc::new(
                    genesis_tools::sandbox::PathValidator::new(None),
                )),
                recalled_memory_ids: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                approval_mode: genesis_config::ApprovalMode::Auto,
            };

            let backend = std::sync::Arc::new(RegistryMcpBackend { registry, context });
            let config = genesis_mcp::McpServeConfig::default();

            genesis_mcp::run_stdio_server(config, backend)
                .await
                .map_err(|e| CliError::Other(format!("MCP server error: {e}")))?;

            Ok("MCP server exited".to_owned())
        }
        McpCommand::Test => {
            let servers = &loaded.config.mcp_servers;
            if servers.is_empty() {
                return Ok("no MCP servers configured".to_owned());
            }

            let configs = genesis_mcp::build_server_configs(servers);
            if configs.is_empty() {
                return Ok("no valid MCP server configs found".to_owned());
            }

            let mut lines = Vec::new();
            let manager = genesis_mcp::McpManager::connect_all(configs).await;
            let server_count = manager.server_count().await;
            let tool_count = manager.tool_count().await;

            if server_count == 0 {
                lines.push("no MCP servers responded".to_owned());
            } else {
                lines.push(format!(
                    "{server_count} server(s) connected, {tool_count} tool(s) available"
                ));

                let tool_defs = manager.tool_definitions().await;
                for tool in &tool_defs {
                    lines.push(format!("  - {}: {}", tool.name, tool.description));
                }
            }

            Ok(lines.join("\n"))
        }
    }
}
