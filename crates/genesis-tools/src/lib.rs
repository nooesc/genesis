pub mod builtins;

use std::collections::BTreeMap;
use std::sync::Arc;

use genesis_types::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolContext {
    pub session_id: String,
    pub profile: String,
    pub data_dir: String,
    pub allow_destructive_tools: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    pub arguments: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Never,
    Destructive,
    Always,
}

pub trait ToolHandler: Send + Sync {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError>;
}

#[derive(Clone)]
struct ToolRegistration {
    definition: ToolDefinition,
    approval: ApprovalPolicy,
    handler: Arc<dyn ToolHandler>,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolRegistration>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("missing required argument `{argument}` for tool `{tool}`")]
    MissingArgument { tool: String, argument: &'static str },
    #[error("tool `{tool}` requires approval: {reason}")]
    ApprovalDenied { tool: String, reason: String },
    #[error("tool `{tool}` execution failed: {reason}")]
    ExecutionFailed { tool: String, reason: String },
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<H>(
        &mut self,
        definition: ToolDefinition,
        approval: ApprovalPolicy,
        handler: H,
    ) -> &mut Self
    where
        H: ToolHandler + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            ToolRegistration {
                definition,
                approval,
                handler: Arc::new(handler),
            },
        );
        self
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|registration| registration.definition.clone())
            .collect()
    }

    pub fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let registration = self
            .tools
            .get(&call.name)
            .ok_or_else(|| ToolError::ToolNotFound(call.name.clone()))?;

        self.enforce_approval(&registration.definition.name, &registration.approval, context)?;
        registration.handler.run(call, context)
    }

    fn enforce_approval(
        &self,
        tool_name: &str,
        approval: &ApprovalPolicy,
        context: &ToolContext,
    ) -> Result<(), ToolError> {
        match approval {
            ApprovalPolicy::Never => Ok(()),
            ApprovalPolicy::Destructive if context.allow_destructive_tools => Ok(()),
            ApprovalPolicy::Destructive => Err(ToolError::ApprovalDenied {
                tool: tool_name.to_owned(),
                reason: "destructive tools are disabled in the current runtime".to_owned(),
            }),
            ApprovalPolicy::Always => Err(ToolError::ApprovalDenied {
                tool: tool_name.to_owned(),
                reason: "interactive approval flow is not implemented yet".to_owned(),
            }),
        }
    }
}

pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(
            ToolDefinition {
                name: "echo".to_owned(),
                description: "Echoes a message back into the runtime for local tool testing."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string", "description": "The message to echo back." }
                    },
                    "required": ["message"]
                })),
            },
            ApprovalPolicy::Never,
            EchoTool,
        )
        .register(
            ToolDefinition {
                name: "session_info".to_owned(),
                description: "Returns the current session id, profile, and data directory."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            SessionInfoTool,
        )
        .register(
            ToolDefinition {
                name: "shell_exec".to_owned(),
                description: "Executes a shell command and returns its stdout/stderr output."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The shell command to execute." },
                        "working_dir": { "type": "string", "description": "Optional working directory for the command." }
                    },
                    "required": ["command"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::shell::ShellExecTool,
        )
        .register(
            ToolDefinition {
                name: "read_file".to_owned(),
                description: "Reads the contents of a file at the given path.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute or relative path to the file to read." }
                    },
                    "required": ["path"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::fs::ReadFileTool,
        )
        .register(
            ToolDefinition {
                name: "write_file".to_owned(),
                description:
                    "Writes content to a file at the given path, creating parent directories as needed."
                        .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute or relative path to the file to write." },
                        "content": { "type": "string", "description": "The content to write to the file." }
                    },
                    "required": ["path", "content"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::fs::WriteFileTool,
        )
        .register(
            ToolDefinition {
                name: "list_dir".to_owned(),
                description: "Lists entries in a directory, marking subdirectories with a trailing slash."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the directory to list." }
                    },
                    "required": ["path"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::fs::ListDirTool,
        )
        .register(
            ToolDefinition {
                name: "memory_store".to_owned(),
                description: "Stores a durable memory as a key and value pair in sqlite."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "key": { "type": "string", "description": "A short label categorizing this memory (e.g. 'user_preference', 'project_goal')." },
                        "value": { "type": "string", "description": "The content of the memory to store." }
                    },
                    "required": ["key", "value"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::memory::MemoryStoreTool,
        )
        .register(
            ToolDefinition {
                name: "memory_recall".to_owned(),
                description: "Searches stored memories using sqlite full-text search."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query to match against stored memories." },
                        "limit": { "type": "integer", "description": "Maximum number of results to return (default: 5)." }
                    },
                    "required": ["query"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::memory::MemoryRecallTool,
        );
    registry
}

struct EchoTool;

impl ToolHandler for EchoTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let content = call
            .arguments
            .get("message")
            .cloned()
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "message",
            })?;

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
        })
    }
}

struct SessionInfoTool;

impl ToolHandler for SessionInfoTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: format!(
                "session={} profile={} data_dir={}",
                context.session_id, context.profile, context.data_dir
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("session_id".to_owned(), context.session_id.clone()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        default_registry, ApprovalPolicy, ToolCall, ToolContext, ToolError, ToolHandler,
        ToolOutput, ToolRegistry,
    };
    use genesis_types::ToolDefinition;
    use std::collections::BTreeMap;

    struct DangerousTool;

    impl ToolHandler for DangerousTool {
        fn run(
            &self,
            _call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                content: "danger acknowledged".to_owned(),
                metadata: BTreeMap::new(),
            })
        }
    }

    fn sample_context() -> ToolContext {
        ToolContext {
            session_id: "session-42".to_owned(),
            profile: "operator".to_owned(),
            data_dir: "/tmp/genesis".to_owned(),
            allow_destructive_tools: false,
        }
    }

    #[test]
    fn default_registry_lists_builtin_tools() {
        let registry = default_registry();
        let definitions = registry.definitions();

        assert_eq!(definitions.len(), 8);
        assert!(definitions.iter().any(|tool| tool.name == "echo"));
        assert!(definitions.iter().any(|tool| tool.name == "session_info"));
        assert!(definitions.iter().any(|tool| tool.name == "shell_exec"));
        assert!(definitions.iter().any(|tool| tool.name == "read_file"));
        assert!(definitions.iter().any(|tool| tool.name == "write_file"));
        assert!(definitions.iter().any(|tool| tool.name == "list_dir"));
        assert!(definitions.iter().any(|tool| tool.name == "memory_store"));
        assert!(definitions.iter().any(|tool| tool.name == "memory_recall"));
    }

    #[test]
    fn echo_tool_requires_message_argument() {
        let registry = default_registry();
        let error = registry
            .execute(
                &ToolCall {
                    name: "echo".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &sample_context(),
            )
            .expect_err("echo should require a message argument");

        assert_eq!(
            error,
            ToolError::MissingArgument {
                tool: "echo".to_owned(),
                argument: "message",
            }
        );
    }

    #[test]
    fn destructive_tools_are_blocked_when_runtime_disallows_them() {
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition {
                name: "dangerous_tool".to_owned(),
                description: "A placeholder destructive tool".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Destructive,
            DangerousTool,
        );

        let error = registry
            .execute(
                &ToolCall {
                    name: "dangerous_tool".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &sample_context(),
            )
            .expect_err("destructive tool should be rejected");

        assert_eq!(
            error,
            ToolError::ApprovalDenied {
                tool: "dangerous_tool".to_owned(),
                reason: "destructive tools are disabled in the current runtime".to_owned(),
            }
        );
    }
}
