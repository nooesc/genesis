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
    /// Terminal backend for shell command execution (local if None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_backend: Option<TerminalBackend>,
}

/// Configurable terminal backend for shell command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum TerminalBackend {
    /// Execute inside a Docker container.
    #[serde(rename = "docker")]
    Docker {
        container: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Execute on a remote host via SSH.
    #[serde(rename = "ssh")]
    Ssh {
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        identity_file: Option<String>,
    },
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

/// Callback for interactive tool approval. When a tool requires approval
/// (e.g., `ApprovalPolicy::Always`), the registry calls this handler to
/// ask the user whether to proceed.
pub trait ApprovalHandler: Send + Sync {
    fn request_approval(&self, tool_name: &str, arguments: &BTreeMap<String, String>) -> bool;
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
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
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

    /// Set an interactive approval handler for tools that require user confirmation.
    pub fn set_approval_handler(&mut self, handler: Arc<dyn ApprovalHandler>) {
        self.approval_handler = Some(handler);
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

        self.enforce_approval(
            &registration.definition.name,
            &registration.approval,
            &call.arguments,
            context,
        )?;
        registration.handler.run(call, context)
    }

    fn enforce_approval(
        &self,
        tool_name: &str,
        approval: &ApprovalPolicy,
        arguments: &BTreeMap<String, String>,
        context: &ToolContext,
    ) -> Result<(), ToolError> {
        match approval {
            ApprovalPolicy::Never => Ok(()),
            ApprovalPolicy::Destructive if context.allow_destructive_tools => Ok(()),
            ApprovalPolicy::Destructive => Err(ToolError::ApprovalDenied {
                tool: tool_name.to_owned(),
                reason: "destructive tools are disabled in the current runtime".to_owned(),
            }),
            ApprovalPolicy::Always => {
                if let Some(handler) = &self.approval_handler {
                    if handler.request_approval(tool_name, arguments) {
                        Ok(())
                    } else {
                        Err(ToolError::ApprovalDenied {
                            tool: tool_name.to_owned(),
                            reason: "user denied approval".to_owned(),
                        })
                    }
                } else {
                    Err(ToolError::ApprovalDenied {
                        tool: tool_name.to_owned(),
                        reason: "no approval handler configured".to_owned(),
                    })
                }
            }
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
                        "working_dir": { "type": "string", "description": "Optional working directory for the command." },
                        "timeout": { "type": "string", "description": "Timeout in seconds (default: 120). The command is killed if it exceeds this." }
                    },
                    "required": ["command"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::shell::ShellExecTool,
        )
        .register(
            ToolDefinition {
                name: "docker_exec".to_owned(),
                description:
                    "Executes a command inside a running Docker container via `docker exec`."
                        .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "container": { "type": "string", "description": "Name or ID of the running Docker container." },
                        "command": { "type": "string", "description": "The command to execute inside the container." },
                        "working_dir": { "type": "string", "description": "Optional working directory inside the container." },
                        "user": { "type": "string", "description": "Optional user to run the command as (e.g. 'root', '1000')." }
                    },
                    "required": ["container", "command"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::docker::DockerExecTool,
        )
        .register(
            ToolDefinition {
                name: "ssh_exec".to_owned(),
                description:
                    "Executes a command on a remote host via SSH."
                        .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "host": { "type": "string", "description": "Hostname or IP address of the remote machine." },
                        "command": { "type": "string", "description": "The command to execute on the remote host." },
                        "user": { "type": "string", "description": "SSH user (defaults to current user if omitted)." },
                        "port": { "type": "string", "description": "SSH port (defaults to 22)." },
                        "identity_file": { "type": "string", "description": "Path to the SSH private key file." }
                    },
                    "required": ["host", "command"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::ssh::SshExecTool,
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
        )
        .register(
            ToolDefinition {
                name: "search_files".to_owned(),
                description: "Searches file contents recursively using grep, returning matching lines with file paths and line numbers.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "The text pattern to search for (supports basic regex)." },
                        "path": { "type": "string", "description": "Directory to search in (defaults to current directory)." }
                    },
                    "required": ["pattern"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::search::SearchFilesTool,
        )
        .register(
            ToolDefinition {
                name: "web_request".to_owned(),
                description: "Makes an HTTP request to a URL and returns the response status, headers, and body.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The URL to request." },
                        "method": { "type": "string", "description": "HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD). Defaults to GET." },
                        "headers": { "type": "string", "description": "Optional JSON object of custom headers, e.g. {\"Authorization\": \"Bearer token\"}." },
                        "body": { "type": "string", "description": "Optional request body (sent as JSON content-type)." }
                    },
                    "required": ["url"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::web::WebRequestTool,
        )
        .register(
            ToolDefinition {
                name: "skill_create".to_owned(),
                description: "Creates or updates a reusable skill. Skills are persistent procedures that can be invoked later.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique name for the skill (e.g. 'code_review', 'deploy_app')." },
                        "description": { "type": "string", "description": "Short description of what the skill does." },
                        "instructions": { "type": "string", "description": "Step-by-step instructions for executing the skill." },
                        "trigger_hint": { "type": "string", "description": "When should this skill be triggered (e.g. 'when user asks to review code')." },
                        "tags": { "type": "string", "description": "Comma-separated tags for categorization (e.g. 'dev,review')." }
                    },
                    "required": ["name", "description", "instructions"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill::SkillCreateTool,
        )
        .register(
            ToolDefinition {
                name: "skill_list".to_owned(),
                description: "Lists all saved skills with their descriptions and version numbers.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill::SkillListTool,
        )
        .register(
            ToolDefinition {
                name: "skill_get".to_owned(),
                description: "Retrieves a specific skill's full instructions by name.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the skill to retrieve." }
                    },
                    "required": ["name"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill::SkillGetTool,
        )
        .register(
            ToolDefinition {
                name: "skill_delete".to_owned(),
                description: "Deletes a saved skill by name.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name of the skill to delete." }
                    },
                    "required": ["name"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::skill::SkillDeleteTool,
        )
        .register(
            ToolDefinition {
                name: "skill_record_usage".to_owned(),
                description: "Records that you used a skill and how effective it was. Call this after applying a skill to track its performance and enable self-improvement. If the outcome is not 'success', consider updating the skill's instructions with skill_create.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "skill_name": { "type": "string", "description": "Name of the skill that was used." },
                        "outcome": { "type": "string", "description": "How well the skill worked: 'success', 'partial', 'failure', or 'unknown'." },
                        "feedback": { "type": "string", "description": "What worked well, what didn't, and what could be improved in the skill's instructions." }
                    },
                    "required": ["skill_name", "outcome"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill::SkillRecordUsageTool,
        )
        .register(
            ToolDefinition {
                name: "user_observe".to_owned(),
                description: "Records an observation about the user's preferences, personality, communication style, goals, or expertise. Repeated observations increase confidence.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "trait_key": { "type": "string", "description": "Unique identifier for the trait (e.g. 'prefers_rust', 'likes_concise_answers')." },
                        "category": { "type": "string", "description": "Category: preference, personality, communication_style, goal, expertise, or context." },
                        "value": { "type": "string", "description": "Description of the observation about the user." }
                    },
                    "required": ["trait_key", "category", "value"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::user_model::UserObserveTool,
        )
        .register(
            ToolDefinition {
                name: "user_model".to_owned(),
                description: "Retrieves what is known about the user from accumulated observations. Can filter by category or minimum confidence.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "category": { "type": "string", "description": "Optional: filter by category (preference, personality, communication_style, goal, expertise, context)." },
                        "min_confidence": { "type": "number", "description": "Optional: minimum confidence threshold (0.0 to 1.0)." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::user_model::UserModelTool,
        )
        .register(
            ToolDefinition {
                name: "session_search".to_owned(),
                description: "Searches your past conversation sessions using full-text search. Returns matching session IDs and titles.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query to match against past conversation content." }
                    },
                    "required": ["query"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::session::SessionSearchTool,
        )
        .register(
            ToolDefinition {
                name: "session_history".to_owned(),
                description: "Loads recent messages from a specific past session. Use session_search first to find relevant sessions.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "ID of the session to load messages from." },
                        "limit": { "type": "integer", "description": "Maximum number of recent messages to return (default: 20)." }
                    },
                    "required": ["session_id"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::session::SessionHistoryTool,
        )
        .register(
            ToolDefinition {
                name: "patch".to_owned(),
                description: "Applies a targeted find-and-replace within a file. Tries exact match first; if that fails, falls back to fuzzy line-based matching (≥70% similarity) so small whitespace or indentation differences don't cause failures. More efficient than write_file for small edits.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file to patch." },
                        "old_text": { "type": "string", "description": "The text to find in the file. Exact match is tried first; if not found, a fuzzy match is attempted." },
                        "new_text": { "type": "string", "description": "The text to replace old_text with." },
                        "replace_all": { "type": "string", "description": "Set to 'true' to replace all occurrences. Default: replace only unique match." }
                    },
                    "required": ["path", "old_text", "new_text"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::patch::PatchTool,
        )
        .register(
            ToolDefinition {
                name: "todo".to_owned(),
                description: "In-memory task list for planning complex work. Use to decompose tasks, track progress, and report status. Actions: add (text), update (id, status), list, clear. Status values: pending, in_progress, done.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "Action to perform: add, update, list, or clear." },
                        "text": { "type": "string", "description": "Text of the todo item (required for add)." },
                        "id": { "type": "string", "description": "ID of the todo item (required for update)." },
                        "status": { "type": "string", "description": "New status: pending, in_progress, or done (required for update)." }
                    },
                    "required": ["action"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::todo::TodoTool,
        )
        .register(
            ToolDefinition {
                name: "spawn_subagent".to_owned(),
                description: "Spawns a subagent to work on a task concurrently. The subagent runs its own agent loop in the background and can use all available tools. Use check_subagent to monitor progress.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "The task for the subagent to accomplish. Be specific and include all relevant context." },
                        "name": { "type": "string", "description": "A short name for this subagent (e.g. 'researcher', 'coder', 'reviewer'). Defaults to 'subagent'." }
                    },
                    "required": ["task"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::subagent::SpawnSubagentTool,
        )
        .register(
            ToolDefinition {
                name: "check_subagent".to_owned(),
                description: "Checks the status and result of a previously spawned subagent.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "subagent_id": { "type": "string", "description": "The ID of the subagent to check." }
                    },
                    "required": ["subagent_id"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::subagent::CheckSubagentTool,
        )
        .register(
            ToolDefinition {
                name: "list_subagents".to_owned(),
                description: "Lists all subagents spawned in the current session with their status.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::subagent::ListSubagentsTool,
        )
        .register(
            ToolDefinition {
                name: "clarify".to_owned(),
                description: "Ask the user a clarifying question when you need more information before proceeding. Use this instead of guessing when requirements are ambiguous.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "The question to ask the user." },
                        "choices": { "type": "string", "description": "Optional comma-separated list of choices to present." }
                    },
                    "required": ["question"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::clarify::ClarifyTool,
        )
        .register(
            ToolDefinition {
                name: "web_search".to_owned(),
                description: "Searches the web and returns relevant results. Uses Brave Search API when BRAVE_API_KEY is set, otherwise falls back to DuckDuckGo.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query." },
                        "count": { "type": "integer", "description": "Number of results to return (default: 5, max: 10)." }
                    },
                    "required": ["query"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::web_search::WebSearchTool,
        )
        .register(
            ToolDefinition {
                name: "send_message".to_owned(),
                description: "Sends a message to a messaging platform (Slack, Telegram, Discord). Requires the corresponding API token environment variable to be set.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "platform": { "type": "string", "description": "Target platform: 'slack', 'telegram', or 'discord'." },
                        "channel": { "type": "string", "description": "Channel or chat ID to send the message to. Slack: channel ID (e.g. C04XXXXXXX). Telegram: chat ID (numeric). Discord: channel ID (numeric)." },
                        "message": { "type": "string", "description": "The message text to send." },
                        "thread_id": { "type": "string", "description": "Optional thread/reply ID. Slack: thread_ts. Telegram: reply_to_message_id." }
                    },
                    "required": ["platform", "channel", "message"]
                })),
            },
            ApprovalPolicy::Always,
            builtins::send_message::SendMessageTool,
        )
        .register(
            ToolDefinition {
                name: "browse".to_owned(),
                description: "Fetches a web page and extracts readable text content, stripping HTML tags, scripts, styles, and navigation. More useful than web_request for reading articles and documentation.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The URL of the web page to read." },
                        "selector": { "type": "string", "description": "Optional HTML tag to focus on (e.g. 'article', 'main', 'p'). Extracts only content within matching tags." }
                    },
                    "required": ["url"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browse::BrowseTool,
        )
        .register(
            ToolDefinition {
                name: "schedule_create".to_owned(),
                description: "Creates a recurring scheduled prompt that runs on a cron schedule. The prompt will be executed automatically at the specified interval.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "cron": { "type": "string", "description": "Cron expression (5 fields: minute hour day month weekday). Examples: '*/5 * * * *' (every 5 min), '0 9 * * *' (daily 9am)." },
                        "prompt": { "type": "string", "description": "The prompt to execute on each trigger." },
                        "destination": { "type": "string", "description": "Delivery destination: 'cli' (default), 'telegram', 'discord', 'slack'." }
                    },
                    "required": ["cron", "prompt"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::schedule::ScheduleCreateTool,
        )
        .register(
            ToolDefinition {
                name: "schedule_list".to_owned(),
                description: "Lists all scheduled prompts with their cron expressions, destinations, and status.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::schedule::ScheduleListTool,
        )
        .register(
            ToolDefinition {
                name: "schedule_delete".to_owned(),
                description: "Deletes a scheduled prompt by its ID.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "The schedule ID to delete." }
                    },
                    "required": ["id"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::schedule::ScheduleDeleteTool,
        )
        .register(
            ToolDefinition {
                name: "code_execution".to_owned(),
                description: "Executes code in a sandboxed subprocess with no access to host environment variables or secrets. Use for computation, data analysis, and scripting. Supports Python (default), Node.js, and Ruby.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "code": { "type": "string", "description": "The source code to execute." },
                        "language": { "type": "string", "description": "Programming language: 'python' (default), 'node', or 'ruby'." },
                        "timeout_secs": { "type": "string", "description": "Timeout in seconds (default: 30). The process is killed if it exceeds this." }
                    },
                    "required": ["code"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::code_execution::CodeExecutionTool,
        )
        .register(
            ToolDefinition {
                name: "session_export".to_owned(),
                description: "Exports a conversation session to Markdown or JSON format. Can write to a file or return the content directly.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "ID of the session to export. Defaults to current session." },
                        "format": { "type": "string", "description": "Export format: 'markdown' (default) or 'json'." },
                        "path": { "type": "string", "description": "Optional file path to write the export to. If omitted, content is returned directly." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::export::SessionExportTool,
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
            terminal_backend: None,
        }
    }

    #[test]
    fn default_registry_lists_builtin_tools() {
        let registry = default_registry();
        let definitions = registry.definitions();

        assert_eq!(definitions.len(), 35);
        assert!(definitions.iter().any(|tool| tool.name == "echo"));
        assert!(definitions.iter().any(|tool| tool.name == "session_info"));
        assert!(definitions.iter().any(|tool| tool.name == "shell_exec"));
        assert!(definitions.iter().any(|tool| tool.name == "read_file"));
        assert!(definitions.iter().any(|tool| tool.name == "write_file"));
        assert!(definitions.iter().any(|tool| tool.name == "list_dir"));
        assert!(definitions.iter().any(|tool| tool.name == "memory_store"));
        assert!(definitions.iter().any(|tool| tool.name == "memory_recall"));
        assert!(definitions.iter().any(|tool| tool.name == "search_files"));
        assert!(definitions.iter().any(|tool| tool.name == "web_request"));
        assert!(definitions.iter().any(|tool| tool.name == "skill_create"));
        assert!(definitions.iter().any(|tool| tool.name == "skill_list"));
        assert!(definitions.iter().any(|tool| tool.name == "skill_get"));
        assert!(definitions.iter().any(|tool| tool.name == "skill_delete"));
        assert!(definitions.iter().any(|tool| tool.name == "user_observe"));
        assert!(definitions.iter().any(|tool| tool.name == "user_model"));
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

    #[test]
    fn always_approval_denied_without_handler() {
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition {
                name: "guarded_tool".to_owned(),
                description: "always requires approval".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Always,
            DangerousTool,
        );

        let error = registry
            .execute(
                &ToolCall {
                    name: "guarded_tool".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &sample_context(),
            )
            .expect_err("should be denied without handler");

        assert_eq!(
            error,
            ToolError::ApprovalDenied {
                tool: "guarded_tool".to_owned(),
                reason: "no approval handler configured".to_owned(),
            }
        );
    }

    #[test]
    fn always_approval_granted_with_approving_handler() {
        use super::ApprovalHandler;
        use std::sync::Arc;

        struct AlwaysApprove;
        impl ApprovalHandler for AlwaysApprove {
            fn request_approval(&self, _: &str, _: &BTreeMap<String, String>) -> bool {
                true
            }
        }

        let mut registry = ToolRegistry::new();
        registry.set_approval_handler(Arc::new(AlwaysApprove));
        registry.register(
            ToolDefinition {
                name: "guarded_tool".to_owned(),
                description: "always requires approval".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Always,
            DangerousTool,
        );

        let result = registry.execute(
            &ToolCall {
                name: "guarded_tool".to_owned(),
                arguments: BTreeMap::new(),
            },
            &sample_context(),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "danger acknowledged");
    }

    #[test]
    fn always_approval_denied_by_handler() {
        use super::ApprovalHandler;
        use std::sync::Arc;

        struct AlwaysDeny;
        impl ApprovalHandler for AlwaysDeny {
            fn request_approval(&self, _: &str, _: &BTreeMap<String, String>) -> bool {
                false
            }
        }

        let mut registry = ToolRegistry::new();
        registry.set_approval_handler(Arc::new(AlwaysDeny));
        registry.register(
            ToolDefinition {
                name: "guarded_tool".to_owned(),
                description: "always requires approval".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Always,
            DangerousTool,
        );

        let error = registry
            .execute(
                &ToolCall {
                    name: "guarded_tool".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &sample_context(),
            )
            .expect_err("should be denied by handler");

        assert_eq!(
            error,
            ToolError::ApprovalDenied {
                tool: "guarded_tool".to_owned(),
                reason: "user denied approval".to_owned(),
            }
        );
    }
}
