pub mod builtins;
pub mod cache;
pub mod http;
pub mod url_safety;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use genesis_types::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

/// Returns true if the tool is read-only (safe to auto-approve in Smart mode).
///
/// Note: `web_search` is intentionally excluded — it makes outbound HTTP requests.
pub fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_dir"
            | "glob"
            | "tree"
            | "search_files"
            | "grep"
            | "find_tools"
            | "session_info"
            | "echo"
            | "think"
            | "clarify"
            | "memory_recall"
            | "skill_search"
    )
}

/// Directories that are typically noise and should be skipped by file traversal tools.
pub const NOISE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    "dist",
    "build",
    ".hg",
    ".svn",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".nuxt",
];

/// Maximum output size for tool results (64 KiB).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Truncate a string to at most `limit` bytes on a valid UTF-8 boundary,
/// appending `suffix` if truncation occurred.
pub fn truncate_at(s: &str, limit: usize, suffix: &str) -> String {
    if s.len() <= limit {
        return s.to_owned();
    }
    // Walk back to char boundary
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = s[..end].to_owned();
    result.push_str(suffix);
    result
}

/// Truncate output to MAX_OUTPUT_BYTES, cutting at a newline boundary
/// and ensuring safe UTF-8 boundaries.
pub fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_owned();
    }

    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }

    if let Some(last_nl) = output[..end].rfind('\n') {
        let mut truncated = output[..=last_nl].to_string();
        truncated.push_str("... (output truncated)");
        truncated
    } else {
        let mut truncated = output[..end].to_string();
        truncated.push_str("\n... (output truncated)");
        truncated
    }
}

/// Truncate raw byte output (e.g. from `std::process::Output`) to
/// MAX_OUTPUT_BYTES after lossy UTF-8 conversion, respecting UTF-8
/// character boundaries.
pub fn truncate_output_bytes(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    truncate_at(&s, MAX_OUTPUT_BYTES, "\n... (output truncated)")
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolContext {
    pub session_id: String,
    pub profile: String,
    pub data_dir: String,
    pub allow_destructive_tools: bool,
    /// Terminal backend for shell command execution (local if None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_backend: Option<TerminalBackend>,
    /// Default working directory for shell commands. When set, local shell
    /// commands use this as the current directory unless overridden by the
    /// tool call's `working_dir` argument. Used by worktree isolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_working_dir: Option<String>,
    /// Sandbox executor for lifecycle-managed backends (Singularity, Modal, Daytona).
    /// When set, the shell tool delegates command execution to this instead of
    /// spawning CLI processes directly.
    #[serde(skip)]
    pub sandbox_manager: Option<Arc<dyn SandboxExecutor>>,
    /// Tool approval mode controlling when tools require interactive confirmation.
    #[serde(default)]
    pub approval_mode: genesis_config::ApprovalMode,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("session_id", &self.session_id)
            .field("profile", &self.profile)
            .field("data_dir", &self.data_dir)
            .field("allow_destructive_tools", &self.allow_destructive_tools)
            .field("terminal_backend", &self.terminal_backend)
            .field("default_working_dir", &self.default_working_dir)
            .field(
                "sandbox_manager",
                &self.sandbox_manager.as_ref().map(|_| ".."),
            )
            .field("approval_mode", &self.approval_mode)
            .finish()
    }
}

impl PartialEq for ToolContext {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.profile == other.profile
            && self.data_dir == other.data_dir
            && self.allow_destructive_tools == other.allow_destructive_tools
            && self.terminal_backend == other.terminal_backend
            && self.default_working_dir == other.default_working_dir
            && self.approval_mode == other.approval_mode
        // sandbox_manager intentionally excluded from equality comparison
    }
}

impl ToolContext {
    /// Return the path to the SQLite database for this context's data directory.
    pub fn db_path(&self) -> PathBuf {
        PathBuf::from(&self.data_dir).join("genesis.db")
    }
}

/// Trait for lifecycle-managed sandbox command execution.
///
/// Implemented in genesis-core to bridge the async `SandboxManager` into the
/// sync `ToolHandler` interface. When present in `ToolContext`, the shell tool
/// delegates to this instead of spawning CLI processes directly.
pub trait SandboxExecutor: Send + Sync {
    fn execute_in_sandbox(
        &self,
        command: &str,
        working_dir: Option<&str>,
        timeout_secs: u64,
    ) -> Result<(String, i32), String>;
}

/// Configurable terminal backend for shell command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Execute inside a Singularity/Apptainer container (HPC environments).
    #[serde(rename = "singularity")]
    Singularity {
        image: String,
        cpu: f32,
        memory_mb: u32,
        persistent: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        bind: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Execute via Modal cloud sandbox (`modal shell --cmd ...`).
    #[serde(rename = "modal")]
    Modal {
        /// Docker image to use for the sandbox.
        #[serde(skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        cpu: f32,
        memory_mb: u32,
        disk_mb: u32,
        persistent: bool,
        /// GPU type to request (e.g. "T4", "A10G").
        #[serde(skip_serializing_if = "Option::is_none")]
        gpu: Option<String>,
        /// Modal app or sandbox name.
        #[serde(skip_serializing_if = "Option::is_none")]
        app: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Execute in a Daytona workspace (`daytona exec ...`).
    #[serde(rename = "daytona")]
    Daytona {
        /// Docker image to use for the workspace.
        #[serde(skip_serializing_if = "Option::is_none")]
        image: Option<String>,
        cpu: f32,
        memory_mb: u32,
        disk_mb: u32,
        persistent: bool,
        /// Daytona target (runner/region).
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        /// Daytona API URL override.
        #[serde(skip_serializing_if = "Option::is_none")]
        api_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
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

/// Summary of a tool for discovery (name + description, no schema).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
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
    cache_policy: cache::CachePolicy,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolRegistration>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    cache: cache::ToolCache,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("missing required argument `{argument}` for tool `{tool}`")]
    MissingArgument {
        tool: String,
        argument: &'static str,
    },
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
                cache_policy: cache::CachePolicy::NotCacheable,
            },
        );
        self
    }

    /// Register a tool with a specific cache policy.
    pub fn register_cached<H>(
        &mut self,
        definition: ToolDefinition,
        approval: ApprovalPolicy,
        handler: H,
        cache_policy: cache::CachePolicy,
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
                cache_policy,
            },
        );
        self
    }

    /// Remove tools whose names are not in the given set.
    pub fn retain(&mut self, names: &std::collections::HashSet<String>) {
        self.tools.retain(|name, _| names.contains(name));
    }

    /// Return the number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Return true if no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|registration| registration.definition.clone())
            .collect()
    }

    /// Search registered tools by name or description (case-insensitive).
    pub fn search_tools(&self, query: &str) -> Vec<ToolSummary> {
        let q = query.to_lowercase();
        let mut results: Vec<(bool, ToolSummary)> = self
            .tools
            .values()
            .filter(|reg| {
                reg.definition.name.to_lowercase().contains(&q)
                    || reg.definition.description.to_lowercase().contains(&q)
            })
            .map(|reg| {
                let name_match = reg.definition.name.to_lowercase().contains(&q);
                (
                    name_match,
                    ToolSummary {
                        name: reg.definition.name.clone(),
                        description: reg.definition.description.clone(),
                    },
                )
            })
            .collect();
        results.sort_by(|a, b| b.0.cmp(&a.0));
        results.into_iter().map(|(_, s)| s).collect()
    }

    pub fn execute(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
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

        // Check cache for cacheable tools.
        if let cache::CachePolicy::Cacheable(ttl) = &registration.cache_policy {
            if let Some((content, metadata)) = self.cache.get(&call.name, &call.arguments) {
                return Ok(ToolOutput { content, metadata });
            }

            let ttl = *ttl;
            let result = registration.handler.run(call, context)?;
            let tracked_path = cache::extract_tracked_path(&call.name, &call.arguments);
            self.cache.insert(
                &call.name,
                &call.arguments,
                result.content.clone(),
                result.metadata.clone(),
                ttl,
                tracked_path,
            );
            return Ok(result);
        }

        // For invalidating tools, execute and then invalidate cache.
        if let cache::CachePolicy::Invalidates = &registration.cache_policy {
            let result = registration.handler.run(call, context)?;
            if let Some(path) = cache::extract_tracked_path(&call.name, &call.arguments) {
                self.cache.invalidate_path(&path);
            }
            return Ok(result);
        }

        registration.handler.run(call, context)
    }

    /// Return the tool result cache.
    pub fn cache(&self) -> &cache::ToolCache {
        &self.cache
    }

    fn enforce_approval(
        &self,
        tool_name: &str,
        policy: &ApprovalPolicy,
        arguments: &BTreeMap<String, String>,
        context: &ToolContext,
    ) -> Result<(), ToolError> {
        use genesis_config::ApprovalMode;

        // ApprovalPolicy::Never tools are ALWAYS exempt regardless of mode.
        if matches!(policy, ApprovalPolicy::Never) {
            return Ok(());
        }

        // Destructive tools blocked by global flag — enforced in ALL modes.
        if matches!(policy, ApprovalPolicy::Destructive) && !context.allow_destructive_tools {
            return Err(ToolError::ApprovalDenied {
                tool: tool_name.to_owned(),
                reason: "destructive tools are disabled".to_owned(),
            });
        }

        match context.approval_mode {
            ApprovalMode::Auto => {
                // Original behavior: Destructive tools allowed (flag checked above),
                // Always-policy tools go through handler.
                if matches!(policy, ApprovalPolicy::Always) {
                    return self.request_or_deny(tool_name, arguments);
                }
                Ok(())
            }
            ApprovalMode::Smart => {
                // Read-only tools auto-approved.
                if is_read_only_tool(tool_name) {
                    return Ok(());
                }
                // Everything else needs approval.
                self.request_or_deny(tool_name, arguments)
            }
            ApprovalMode::Manual => {
                // Every non-Never tool needs approval.
                self.request_or_deny(tool_name, arguments)
            }
        }
    }

    /// Ask the configured approval handler for permission, or deny if no
    /// handler is configured (fail-closed).
    fn request_or_deny(
        &self,
        tool_name: &str,
        arguments: &BTreeMap<String, String>,
    ) -> Result<(), ToolError> {
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
            // No handler configured — deny by default (fail-closed).
            Err(ToolError::ApprovalDenied {
                tool: tool_name.to_owned(),
                reason: "no approval handler configured".to_owned(),
            })
        }
    }
}

pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Shared process registry for background shell commands.
    let process_registry = builtins::process_registry::ProcessRegistry::new();

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
                description: "Executes a shell command and returns its stdout/stderr output. Set background=true to run the command as a background process and get a session ID for later monitoring via the `process` tool."
                    .to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The shell command to execute." },
                        "working_dir": { "type": "string", "description": "Optional working directory for the command." },
                        "timeout": { "type": "string", "description": "Timeout in seconds (default: 120). The command is killed if it exceeds this. Only applies to foreground execution." },
                        "background": { "type": "string", "description": "Set to 'true' to run the command in the background. Returns a session ID immediately instead of waiting for completion." }
                    },
                    "required": ["command"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::process_registry::BackgroundShellExecTool { registry: process_registry.clone() },
        )
        .register(
            ToolDefinition {
                name: "process".to_owned(),
                description: "Manage background processes. Actions: list (show all tracked processes), poll (check status and get output preview), log (full output with pagination), wait (block until completion), kill (terminate process), write (send raw data to stdin), submit (send data + newline to stdin).".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "Action to perform: list, poll, log, wait, kill, write, submit." },
                        "id": { "type": "string", "description": "Process session ID (e.g. proc_xxxxxxxxxxxx). Required for all actions except list." },
                        "timeout": { "type": "string", "description": "Timeout in seconds for wait action (default: 60)." },
                        "offset": { "type": "string", "description": "Byte offset for log pagination (default: 0)." },
                        "limit": { "type": "string", "description": "Max bytes to return for log pagination (default: 16384)." },
                        "data": { "type": "string", "description": "Data to send for write/submit actions." }
                    },
                    "required": ["action"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::process_registry::ProcessTool { registry: process_registry },
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
        .register_cached(
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
            cache::CachePolicy::Cacheable(std::time::Duration::from_secs(30)),
        )
        .register_cached(
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
            cache::CachePolicy::Invalidates,
        )
        .register_cached(
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
            cache::CachePolicy::Cacheable(std::time::Duration::from_secs(60)),
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
                description: "Searches stored memories using graph-aware recall, which may expand to linked notes and updates access metrics on returned results."
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
        .register_cached(
            ToolDefinition {
                name: "search_files".to_owned(),
                description: "Searches file contents recursively using ripgrep (rg) with grep fallback, returning matching lines with file paths and line numbers.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "The text pattern to search for (supports regex)." },
                        "path": { "type": "string", "description": "Directory to search in (defaults to current directory)." },
                        "case_insensitive": { "type": "string", "description": "Set to \"true\" for case-insensitive search." },
                        "file_type": { "type": "string", "description": "Restrict search to a file type (e.g. \"rust\", \"py\", \"js\"). Only supported with ripgrep." },
                        "glob": { "type": "string", "description": "Glob pattern to filter files (e.g. \"*.rs\", \"src/**/*.ts\"). Only supported with ripgrep." },
                        "context_lines": { "type": "string", "description": "Number of context lines to show around each match (e.g. \"3\"). Only supported with ripgrep." }
                    },
                    "required": ["pattern"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::search::SearchFilesTool,
            cache::CachePolicy::Cacheable(std::time::Duration::from_secs(60)),
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
                name: "skill_view_file".to_owned(),
                description: "Reads a supporting file attached to a skill, such as a reference doc or example file.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "skill_name": { "type": "string", "description": "Name of the skill owning the supporting file." },
                        "file_path": { "type": "string", "description": "Relative file path stored under the skill, e.g. 'references/api.md'." }
                    },
                    "required": ["skill_name", "file_path"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill_file::SkillViewFileTool,
        )
        .register(
            ToolDefinition {
                name: "skill_store_file".to_owned(),
                description: "Stores a supporting file for a skill, such as a reference document or example. Overwrites if the file already exists.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "skill_name": { "type": "string", "description": "Name of the skill to attach the file to." },
                        "file_path": { "type": "string", "description": "Relative path for the file, e.g. 'references/api.md'." },
                        "content": { "type": "string", "description": "The file content to store." }
                    },
                    "required": ["skill_name", "file_path", "content"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill_file::SkillStoreFileTool,
        )
        .register(
            ToolDefinition {
                name: "skill_list_files".to_owned(),
                description: "Lists all supporting files attached to a skill.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "skill_name": { "type": "string", "description": "Name of the skill to list files for." }
                    },
                    "required": ["skill_name"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::skill_file::SkillListFilesTool,
        )
        .register(
            ToolDefinition {
                name: "skill_delete_file".to_owned(),
                description: "Deletes a supporting file from a skill.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "skill_name": { "type": "string", "description": "Name of the skill." },
                        "file_path": { "type": "string", "description": "Relative path of the file to delete." }
                    },
                    "required": ["skill_name", "file_path"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::skill_file::SkillDeleteFileTool,
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
        .register_cached(
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
            cache::CachePolicy::Invalidates,
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
                description: "Sends a message to a messaging platform (Slack, Telegram, Discord, WhatsApp, Home Assistant). Requires the corresponding API token environment variable to be set. Use list_channels first to discover available channel IDs.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "platform": { "type": "string", "description": "Target platform: 'slack', 'telegram', 'discord', 'whatsapp', or 'homeassistant'." },
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
                name: "list_channels".to_owned(),
                description: "Lists available channels on configured messaging platforms (Slack, Discord, WhatsApp, Home Assistant). Fetches from platform APIs and caches results. Use this to discover channel IDs before using send_message.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "platform": { "type": "string", "description": "Optional: filter to a specific platform ('slack', 'telegram', 'discord'). Omit to list all configured platforms." },
                        "refresh": { "type": "string", "description": "Set to 'true' to force refresh the cache, bypassing the 5-minute TTL." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::channel_directory::ListChannelsTool,
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
                        "destination": { "type": "string", "description": "Delivery destination: 'cli' (default), 'telegram', 'discord', 'slack'." },
                        "timezone": { "type": "string", "description": "IANA timezone name (e.g. 'America/New_York', 'Asia/Tokyo'). Defaults to UTC." }
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
                name: "execute_code".to_owned(),
                description: "Run a Python script that can call Genesis tools programmatically via RPC. \
                    Use when you need 3+ tool calls with processing logic between them, \
                    need to filter/reduce large outputs before they enter your context, \
                    need conditional branching, or need to loop (fetch N pages, process N files). \
                    Use normal tool calls instead for single calls or when you need to reason about the full result.\n\n\
                    Available via `from genesis_tools import ...`:\n\
                    terminal(command, timeout=None, workdir=None) \u{2014} run shell commands\n\
                    read_file(path, offset=1, limit=500) \u{2014} read file contents\n\
                    write_file(path, content) \u{2014} write to a file\n\
                    search_files(pattern, target=\"content\", path=\".\") \u{2014} search files\n\
                    patch(path, old_string, new_string, replace_all=False) \u{2014} find-and-replace\n\
                    web_search(query, limit=5) \u{2014} search the web\n\
                    browse(url) \u{2014} extract web page content\n\n\
                    Also available: json_parse(text), shell_quote(s), retry(fn, max_attempts=3)\n\
                    Limits: 5min timeout, 50KB stdout, max 50 tool calls per script.\n\
                    Print your final result to stdout.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "code": {
                            "type": "string",
                            "description": "Python code to execute. Import tools with `from genesis_tools import terminal, read_file, ...` and print your final result to stdout."
                        }
                    },
                    "required": ["code"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::code_execution::CodeExecutionTool,
        )
        .register(
            ToolDefinition {
                name: "text_to_speech".to_owned(),
                description: "Generates speech audio from text using edge-tts and writes an MP3 file.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "Text to synthesize into speech." },
                        "voice": { "type": "string", "description": "Optional voice name (default: en-US-AriaNeural)." },
                        "output_path": { "type": "string", "description": "Path to the output MP3 file." },
                        "rate": { "type": "string", "description": "Optional speech rate adjustment like '+20%' or '-10%'." }
                    },
                    "required": ["text", "output_path"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::tts::TextToSpeechTool,
        )
        .register(
            ToolDefinition {
                name: "session_export".to_owned(),
                description: "Exports a conversation session to Markdown, JSON, or ChatML format. Can write to a file or return the content directly.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "ID of the session to export. Defaults to current session." },
                        "format": { "type": "string", "description": "Export format: 'markdown' (default), 'json', or 'chatml'." },
                        "path": { "type": "string", "description": "Optional file path to write the export to. If omitted, content is returned directly." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::export::SessionExportTool,
        )
        .register(
            ToolDefinition {
                name: "reason_with_model".to_owned(),
                description: "Queries a secondary LLM for a second opinion or specialized reasoning. Use when you want to cross-check your work, get a different perspective, or leverage a model that may be better at a specific task (e.g., math, code review, creative writing).".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "The prompt to send to the secondary model." },
                        "model": { "type": "string", "description": "Model identifier (e.g., 'gpt-4.1', 'claude-sonnet-4-6', 'llama-3')." },
                        "backend": { "type": "string", "description": "Provider backend: 'openai' (default), 'anthropic', 'openrouter', etc." },
                        "system": { "type": "string", "description": "Optional system prompt for the secondary model." },
                        "temperature": { "type": "string", "description": "Temperature (0.0-2.0). Default: model default." },
                        "max_tokens": { "type": "string", "description": "Maximum tokens in the response." }
                    },
                    "required": ["prompt", "model"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::reason::ReasonWithModelTool,
        )
        .register(
            ToolDefinition {
                name: "mixture_of_agents".to_owned(),
                description: "Queries multiple LLMs in parallel with the same prompt and synthesizes their responses. Use for critical decisions, complex reasoning, or when you want higher confidence through model consensus.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "The question or task to send to all models." },
                        "models": { "type": "string", "description": "Comma-separated model specs as 'backend/model' (e.g., 'openai/gpt-4o, anthropic/claude-sonnet-4-20250514'). Defaults to gpt-4o, gpt-4o-mini, claude-sonnet-4-20250514." },
                        "system": { "type": "string", "description": "Optional system prompt shared by all models." },
                        "synthesize": { "type": "string", "description": "Whether to synthesize responses into one answer ('true'/'false'). Default: true." },
                        "synthesis_model": { "type": "string", "description": "Model to use for synthesis. Default: gpt-4o." },
                        "synthesis_backend": { "type": "string", "description": "Backend for synthesis model. Default: openai." }
                    },
                    "required": ["prompt"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::mixture::MixtureOfAgentsTool,
        )
        .register_cached(
            ToolDefinition {
                name: "list_tree".to_owned(),
                description: "Recursively lists a directory as an indented tree. Skips noise directories (.git, node_modules, target, etc.) by default. Supports depth limiting, hidden files, and glob pattern filtering.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory to list." },
                        "max_depth": { "type": "string", "description": "Maximum recursion depth (default: 3)." },
                        "show_hidden": { "type": "string", "description": "Set to \"true\" to include hidden files (starting with '.')." },
                        "pattern": { "type": "string", "description": "Glob-like suffix filter (e.g. \"*.rs\") to only show matching files." }
                    },
                    "required": ["path"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::tree::ListTreeTool,
            cache::CachePolicy::Cacheable(std::time::Duration::from_secs(60)),
        )
        .register(
            ToolDefinition {
                name: "git_status".to_owned(),
                description: "Shows the working tree status of a git repository. Returns branch info, staged/unstaged changes, and untracked files.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the git repository (defaults to current directory)." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::git::GitStatusTool,
        )
        .register(
            ToolDefinition {
                name: "git_diff".to_owned(),
                description: "Shows differences between commits, the working tree, and the index. Supports staged diffs, file filtering, name-only mode, and commit ranges.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the git repository (defaults to current directory)." },
                        "staged": { "type": "string", "description": "Set to \"true\" to show staged changes (--cached)." },
                        "name_only": { "type": "string", "description": "Set to \"true\" to only show file names." },
                        "commit_range": { "type": "string", "description": "Commit range to diff (e.g. \"HEAD~3..HEAD\", \"main..feature\")." },
                        "file_paths": { "type": "string", "description": "Space-separated file paths to restrict the diff to." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::git::GitDiffTool,
        )
        .register(
            ToolDefinition {
                name: "git_log".to_owned(),
                description: "Shows commit history. Returns one-line summaries with configurable count, path, author, and date filtering.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the git repository (defaults to current directory)." },
                        "max_count": { "type": "string", "description": "Maximum number of commits to show (default: 20)." },
                        "file_path": { "type": "string", "description": "Show only commits affecting this file path." },
                        "author": { "type": "string", "description": "Filter commits by author name or email." },
                        "since": { "type": "string", "description": "Show commits after this date (e.g. \"2024-01-01\", \"2 weeks ago\")." },
                        "until": { "type": "string", "description": "Show commits before this date." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::git::GitLogTool,
        )
        .register(
            ToolDefinition {
                name: "git_commit".to_owned(),
                description: "Creates a git commit with the given message. By default commits only staged changes; set all to \"true\" to auto-stage tracked files.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the git repository (defaults to current directory)." },
                        "message": { "type": "string", "description": "Commit message." },
                        "all": { "type": "string", "description": "Set to \"true\" to auto-stage all modified tracked files (-a flag)." }
                    },
                    "required": ["message"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::git::GitCommitTool,
        )
        .register(
            ToolDefinition {
                name: "git_branch".to_owned(),
                description: "Manages git branches. Actions: list (default), create, switch, delete.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the git repository (defaults to current directory)." },
                        "action": { "type": "string", "description": "Action: 'list' (default), 'create', 'switch', or 'delete'." },
                        "name": { "type": "string", "description": "Branch name (required for create, switch, delete)." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::git::GitBranchTool,
        )
        .register(
            ToolDefinition {
                name: "trajectory".to_owned(),
                description: "Manages trajectory recording for agent training data. Actions: export (returns trajectory JSON, ShareGPT, or ChatML format), status (check recording state), tag (add a tag), set_outcome (mark success/failure/abandoned).".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "Action: 'export', 'status', 'tag', or 'set_outcome'." },
                        "format": { "type": "string", "description": "Export format: 'json' (default), 'sharegpt', or 'chatml'. Only used with export action." },
                        "tag": { "type": "string", "description": "Tag to add (used with tag action)." },
                        "outcome": { "type": "string", "description": "Outcome: 'success', 'failure', or 'abandoned' (used with set_outcome action)." },
                        "reason": { "type": "string", "description": "Failure reason (optional, used with set_outcome when outcome is 'failure')." }
                    },
                    "required": ["action"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::trajectory::TrajectoryTool,
        )
        .register(
            ToolDefinition {
                name: "list_processes".to_owned(),
                description: "Lists running processes with optional filtering by name pattern, user, and sorting by CPU/memory/PID usage.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Filter processes by name/command (case-insensitive substring match)." },
                        "user": { "type": "string", "description": "Filter by process owner username." },
                        "sort": { "type": "string", "description": "Sort by: 'cpu' (default), 'mem', or 'pid'." },
                        "limit": { "type": "string", "description": "Maximum number of processes to return (default: 20)." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::process::ListProcessesTool,
        )
        .register(
            ToolDefinition {
                name: "system_info".to_owned(),
                description: "Returns system resource information including CPU load, memory usage, disk space, and network interfaces.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "section": { "type": "string", "description": "Section to report: 'all' (default), 'cpu', 'memory', 'disk', or 'network'." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::process::SystemInfoTool,
        )
        .register(
            ToolDefinition {
                name: "kill_process".to_owned(),
                description: "Sends a signal to a process by PID. Use list_processes to find the PID first. Default signal is TERM (graceful shutdown); use KILL for force termination.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "pid": { "type": "string", "description": "Process ID to signal." },
                        "signal": { "type": "string", "description": "Signal to send: TERM (default), KILL, INT, HUP, QUIT, USR1, USR2, STOP, CONT." }
                    },
                    "required": ["pid"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::process::KillProcessTool,
        )
        .register(
            ToolDefinition {
                name: "glob_search".to_owned(),
                description: "Finds files matching a glob pattern. Supports recursive patterns like **/*.rs. Skips hidden files and noise directories (.git, node_modules, target) by default.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob pattern to match (e.g. \"**/*.rs\", \"src/**/*.ts\", \"*.json\")." },
                        "path": { "type": "string", "description": "Base directory to search from (defaults to current directory)." },
                        "type": { "type": "string", "description": "Filter by type: 'file', 'dir', or 'any' (default)." },
                        "limit": { "type": "string", "description": "Maximum number of results (default: 100)." }
                    },
                    "required": ["pattern"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::glob::GlobSearchTool,
        )
        .register(
            ToolDefinition {
                name: "transcribe".to_owned(),
                description: "Transcribes audio files to text using OpenAI-compatible Whisper API. Supports mp3, mp4, mpeg, mpga, m4a, wav, webm, ogg, and flac formats up to 25 MB.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path to the audio file to transcribe." },
                        "model": { "type": "string", "description": "Whisper model to use (default: whisper-1)." },
                        "language": { "type": "string", "description": "ISO-639-1 language code (e.g. 'en', 'es'). Auto-detected if omitted." },
                        "prompt": { "type": "string", "description": "Optional context to guide transcription accuracy." },
                        "api_base": { "type": "string", "description": "API base URL (default: https://api.openai.com/v1 or OPENAI_API_BASE env)." }
                    },
                    "required": ["file_path"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::transcribe::TranscribeTool,
        )
        .register(
            ToolDefinition {
                name: "image_generation".to_owned(),
                description: "Generates images from text prompts using OpenAI-compatible DALL-E API. Saves the generated image to a file. Supports size, quality, and style options.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "Text description of the image to generate." },
                        "output_path": { "type": "string", "description": "File path to save the generated image." },
                        "model": { "type": "string", "description": "Model to use (default: dall-e-3)." },
                        "size": { "type": "string", "description": "Image size: 256x256, 512x512, 1024x1024, 1024x1792, 1792x1024 (default: 1024x1024)." },
                        "quality": { "type": "string", "description": "Quality: 'standard' or 'hd' (default: standard)." },
                        "style": { "type": "string", "description": "Style: 'vivid' or 'natural' (default: vivid)." },
                        "api_base": { "type": "string", "description": "API base URL (default: https://api.openai.com/v1 or OPENAI_API_BASE env)." }
                    },
                    "required": ["prompt", "output_path"]
                })),
            },
            ApprovalPolicy::Always,
            builtins::image_gen::ImageGenerationTool,
        )
        .register(
            ToolDefinition {
                name: "vision".to_owned(),
                description: "Analyzes images using multimodal LLM APIs. Send a local image file or URL to get a detailed description, text transcription, diagram analysis, or answer questions about visual content.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path to a local image file (png, jpg, gif, webp, etc.)." },
                        "image_url": { "type": "string", "description": "URL of an image to analyze (alternative to file_path)." },
                        "question": { "type": "string", "description": "What to analyze or ask about the image (default: describe in detail)." },
                        "model": { "type": "string", "description": "Vision model to use (default: gpt-4o)." },
                        "max_tokens": { "type": "string", "description": "Maximum response tokens (default: 1024)." },
                        "api_base": { "type": "string", "description": "API base URL (default: https://api.openai.com/v1 or OPENAI_API_BASE env)." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::vision::VisionTool,
        )
        .register(
            ToolDefinition {
                name: "ha_list_entities".to_owned(),
                description: "List Home Assistant entities. Optionally filter by domain (light, switch, climate, sensor, etc.) or by area name (living room, kitchen, etc.).".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "domain": { "type": "string", "description": "Entity domain filter (e.g. 'light', 'switch', 'climate', 'sensor', 'binary_sensor', 'cover', 'fan', 'media_player')." },
                        "area": { "type": "string", "description": "Area/room name filter (e.g. 'living room', 'kitchen'). Matches against friendly names." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::homeassistant::HaListEntitiesTool,
        )
        .register(
            ToolDefinition {
                name: "ha_get_state".to_owned(),
                description: "Get the detailed state of a single Home Assistant entity, including all attributes (brightness, color, temperature setpoint, sensor readings, etc.).".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "entity_id": { "type": "string", "description": "The entity ID to query (e.g. 'light.living_room', 'climate.thermostat', 'sensor.temperature')." }
                    },
                    "required": ["entity_id"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::homeassistant::HaGetStateTool,
        )
        .register(
            ToolDefinition {
                name: "ha_list_services".to_owned(),
                description: "List available Home Assistant services (actions) for device control. Shows what actions can be performed on each device type and their parameters.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "domain": { "type": "string", "description": "Filter by domain (e.g. 'light', 'climate', 'switch'). Omit to list all." }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Never,
            builtins::homeassistant::HaListServicesTool,
        )
        .register(
            ToolDefinition {
                name: "ha_call_service".to_owned(),
                description: "Call a Home Assistant service to control a device. Use ha_list_services to discover available services and parameters.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "domain": { "type": "string", "description": "Service domain (e.g. 'light', 'switch', 'climate', 'cover', 'media_player', 'fan', 'scene', 'script')." },
                        "service": { "type": "string", "description": "Service name (e.g. 'turn_on', 'turn_off', 'toggle', 'set_temperature')." },
                        "entity_id": { "type": "string", "description": "Target entity ID (e.g. 'light.living_room'). Some services may not need this." },
                        "data": { "type": "string", "description": "Additional service data as JSON string. Examples: '{\"brightness\": 255}' for lights, '{\"temperature\": 22}' for climate." }
                    },
                    "required": ["domain", "service"]
                })),
            },
            ApprovalPolicy::Always,
            builtins::homeassistant::HaCallServiceTool,
        );

    // ── Browser automation tools ──────────────────────────────────────
    let browser_mgr = std::sync::Arc::new(builtins::browser::BrowserManager::new());

    registry
        .register(
            ToolDefinition {
                name: "browser_navigate".to_owned(),
                description: "Navigate to a URL in the browser. Initializes the session and loads the page. Must be called before other browser tools. For simple information retrieval, prefer web_search or browse (faster, cheaper). Use browser tools when you need to interact with a page (click, fill forms, dynamic content).".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The URL to navigate to (e.g., 'https://example.com')" }
                    },
                    "required": ["url"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserNavigate { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_snapshot".to_owned(),
                description: "Get a text-based snapshot of the current page's accessibility tree. Returns interactive elements with ref IDs (like @e1, @e2) for browser_click and browser_type. full=false (default): compact view with interactive elements. full=true: complete page content. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "full": { "type": "boolean", "description": "If true, returns complete page content. If false (default), returns compact view.", "default": false }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserSnapshot { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_click".to_owned(),
                description: "Click on an element identified by its ref ID from the snapshot (e.g., '@e5'). Requires browser_navigate and browser_snapshot first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string", "description": "The element reference from the snapshot (e.g., '@e5', '@e12')" }
                    },
                    "required": ["ref"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserClick { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_type".to_owned(),
                description: "Type text into an input field identified by its ref ID. Clears the field first, then types the new text. Requires browser_navigate and browser_snapshot first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string", "description": "The element reference from the snapshot (e.g., '@e3')" },
                        "text": { "type": "string", "description": "The text to type into the field" }
                    },
                    "required": ["ref", "text"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserType { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_scroll".to_owned(),
                description: "Scroll the page in a direction. Use this to reveal more content that may be below or above the current viewport. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "direction": { "type": "string", "enum": ["up", "down"], "description": "Direction to scroll" }
                    },
                    "required": ["direction"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserScroll { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_back".to_owned(),
                description: "Navigate back to the previous page in browser history. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserBack { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_press".to_owned(),
                description: "Press a keyboard key. Useful for submitting forms (Enter), navigating (Tab), or keyboard shortcuts. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "key": { "type": "string", "description": "Key to press (e.g., 'Enter', 'Tab', 'Escape', 'ArrowDown')" }
                    },
                    "required": ["key"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserPress { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_close".to_owned(),
                description: "Close the browser session and release resources. Call when done with browser tasks.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserClose { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_get_images".to_owned(),
                description: "Get a list of all images on the current page with their URLs and alt text. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserGetImages { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_vision".to_owned(),
                description: "Take a screenshot and analyze it with vision AI. Useful for CAPTCHAs, visual verification, complex layouts, or when text snapshot doesn't capture visual information. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "What you want to know about the page visually. Be specific." },
                        "annotate": { "type": "boolean", "default": false, "description": "If true, overlay numbered labels on interactive elements." }
                    },
                    "required": ["question"]
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserVision { manager: browser_mgr.clone() },
        )
        .register(
            ToolDefinition {
                name: "browser_console".to_owned(),
                description: "Get browser console output and JavaScript errors from the current page. Returns console.log/warn/error/info messages and uncaught JS exceptions. Requires browser_navigate first.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "clear": { "type": "boolean", "default": false, "description": "If true, clear the message buffers after reading" }
                    },
                    "required": []
                })),
            },
            ApprovalPolicy::Destructive,
            builtins::browser::BrowserConsole { manager: browser_mgr },
        )
        .register(
            ToolDefinition {
                name: "find_tools".to_owned(),
                description: "Search for available tools by name or description. Returns matching tools with descriptions. Use to discover what tools are available for a task.".to_owned(),
                parameters: Some(json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query to match tool names/descriptions" }
                    },
                    "required": ["query"]
                })),
            },
            ApprovalPolicy::Never,
            builtins::find_tools::FindToolsTool,
        );

    registry
}

struct EchoTool;

impl ToolHandler for EchoTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let content =
            call.arguments
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
pub mod test_utils {
    use super::ToolContext;

    /// Create a test `ToolContext` with standard defaults.
    ///
    /// Uses `allow_destructive_tools: false`. For tests requiring destructive
    /// tool access, use [`test_ctx_destructive`] instead.
    pub fn test_ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
            approval_mode: genesis_config::ApprovalMode::Auto,
        }
    }

    /// Create a test `ToolContext` with destructive tool access enabled.
    pub fn test_ctx_destructive() -> ToolContext {
        ToolContext {
            allow_destructive_tools: true,
            ..test_ctx()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cache, default_registry, ApprovalPolicy, ToolCall, ToolContext, ToolError, ToolHandler,
        ToolOutput, ToolRegistry,
    };
    use genesis_types::ToolDefinition;
    use std::collections::BTreeMap;

    struct DangerousTool;

    impl ToolHandler for DangerousTool {
        fn run(&self, _call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                content: "danger acknowledged".to_owned(),
                metadata: BTreeMap::new(),
            })
        }
    }

    fn sample_context() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    #[test]
    fn default_registry_lists_builtin_tools() {
        let registry = default_registry();
        let definitions = registry.definitions();

        assert_eq!(definitions.len(), 74);
        assert!(definitions.iter().any(|tool| tool.name == "echo"));
        assert!(definitions.iter().any(|tool| tool.name == "session_info"));
        assert!(definitions.iter().any(|tool| tool.name == "shell_exec"));
        assert!(definitions.iter().any(|tool| tool.name == "process"));
        assert!(definitions.iter().any(|tool| tool.name == "text_to_speech"));
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
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "skill_view_file"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "skill_store_file"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "skill_list_files"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "skill_delete_file"));
        assert!(definitions.iter().any(|tool| tool.name == "skill_delete"));
        assert!(definitions.iter().any(|tool| tool.name == "user_observe"));
        assert!(definitions.iter().any(|tool| tool.name == "user_model"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "browser_navigate"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "browser_snapshot"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_click"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_type"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_scroll"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_back"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_press"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_close"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "browser_get_images"));
        assert!(definitions.iter().any(|tool| tool.name == "browser_vision"));
        assert!(definitions
            .iter()
            .any(|tool| tool.name == "browser_console"));
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
                reason: "destructive tools are disabled".to_owned(),
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
    fn terminal_backend_modal_round_trips() {
        let backend = super::TerminalBackend::Modal {
            app: Some("my-app".to_owned()),
            gpu: Some("A10G".to_owned()),
            image: None,
            cpu: 1.0,
            memory_mb: 5120,
            disk_mb: 51200,
            persistent: true,
            working_dir: None,
        };
        let json = serde_json::to_string(&backend).expect("serialize");
        assert!(json.contains("\"type\":\"modal\""));
        let decoded: super::TerminalBackend = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, backend);
    }

    #[test]
    fn terminal_backend_daytona_round_trips() {
        let backend = super::TerminalBackend::Daytona {
            image: None,
            cpu: 1.0,
            memory_mb: 5120,
            disk_mb: 10240,
            persistent: true,
            target: Some("us".to_owned()),
            api_url: None,
            working_dir: None,
        };
        let json = serde_json::to_string(&backend).expect("serialize");
        assert!(json.contains("\"type\":\"daytona\""));
        let decoded: super::TerminalBackend = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, backend);
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

    /// A tool that counts how many times it has been called.
    struct CountingTool {
        call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl ToolHandler for CountingTool {
        fn run(&self, _call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolOutput {
                content: format!("call #{}", n + 1),
                metadata: BTreeMap::new(),
            })
        }
    }

    #[test]
    fn cacheable_tool_returns_cached_result_on_second_call() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut registry = ToolRegistry::new();
        registry.register_cached(
            ToolDefinition {
                name: "cached_tool".to_owned(),
                description: "a cacheable tool".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Never,
            CountingTool {
                call_count: call_count.clone(),
            },
            cache::CachePolicy::Cacheable(std::time::Duration::from_secs(60)),
        );

        let call = ToolCall {
            name: "cached_tool".to_owned(),
            arguments: BTreeMap::new(),
        };
        let ctx = sample_context();

        let r1 = registry.execute(&call, &ctx).unwrap();
        assert_eq!(r1.content, "call #1");
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Second call should return cached result, NOT incrementing the counter.
        let r2 = registry.execute(&call, &ctx).unwrap();
        assert_eq!(r2.content, "call #1");
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        let stats = registry.cache().stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn invalidating_tool_clears_cache_for_same_path() {
        let read_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let mut registry = ToolRegistry::new();

        // Register a cacheable reader.
        registry.register_cached(
            ToolDefinition {
                name: "read_file".to_owned(),
                description: "reads a file".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Never,
            CountingTool {
                call_count: read_count.clone(),
            },
            cache::CachePolicy::Cacheable(std::time::Duration::from_secs(60)),
        );

        // Register an invalidating writer.
        struct WriteTool;
        impl ToolHandler for WriteTool {
            fn run(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput {
                    content: "written".to_owned(),
                    metadata: BTreeMap::new(),
                })
            }
        }
        registry.register_cached(
            ToolDefinition {
                name: "write_file".to_owned(),
                description: "writes a file".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Never,
            WriteTool,
            cache::CachePolicy::Invalidates,
        );

        let ctx = sample_context();
        let path_str = file_path.to_string_lossy().to_string();

        let mut read_args = BTreeMap::new();
        read_args.insert("path".to_owned(), path_str.clone());

        let read_call = ToolCall {
            name: "read_file".to_owned(),
            arguments: read_args,
        };

        // First read → miss → handler runs.
        registry.execute(&read_call, &ctx).unwrap();
        assert_eq!(read_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Second read → cache hit → handler NOT called.
        registry.execute(&read_call, &ctx).unwrap();
        assert_eq!(read_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Write → invalidates the read cache.
        let mut write_args = BTreeMap::new();
        write_args.insert("path".to_owned(), path_str);
        let write_call = ToolCall {
            name: "write_file".to_owned(),
            arguments: write_args,
        };
        registry.execute(&write_call, &ctx).unwrap();

        // Third read → miss (invalidated) → handler runs again.
        registry.execute(&read_call, &ctx).unwrap();
        assert_eq!(read_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn non_cacheable_tool_always_executes_handler() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition {
                name: "uncached_tool".to_owned(),
                description: "a non-cacheable tool".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Never,
            CountingTool {
                call_count: call_count.clone(),
            },
        );

        let call = ToolCall {
            name: "uncached_tool".to_owned(),
            arguments: BTreeMap::new(),
        };
        let ctx = sample_context();

        registry.execute(&call, &ctx).unwrap();
        registry.execute(&call, &ctx).unwrap();
        registry.execute(&call, &ctx).unwrap();

        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn search_tools_finds_by_name() {
        let registry = super::default_registry();
        let results = registry.search_tools("echo");
        assert!(!results.is_empty(), "should find echo tool");
        assert!(results.iter().any(|t| t.name == "echo"));
    }

    #[test]
    fn search_tools_finds_by_description() {
        let registry = super::default_registry();
        let results = registry.search_tools("shell");
        assert!(!results.is_empty(), "should find shell-related tools");
    }

    #[test]
    fn search_tools_returns_empty_for_no_match() {
        let registry = super::default_registry();
        let results = registry.search_tools("zzzznonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn search_tools_name_matches_ranked_first() {
        let registry = super::default_registry();
        let results = registry.search_tools("find_tools");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "find_tools");
    }

    // ---- is_read_only_tool classification tests ----

    #[test]
    fn is_read_only_classifies_correctly() {
        assert!(super::is_read_only_tool("read_file"));
        assert!(super::is_read_only_tool("list_dir"));
        assert!(super::is_read_only_tool("glob"));
        assert!(super::is_read_only_tool("tree"));
        assert!(super::is_read_only_tool("search_files"));
        assert!(super::is_read_only_tool("grep"));
        assert!(super::is_read_only_tool("find_tools"));
        assert!(super::is_read_only_tool("session_info"));
        assert!(super::is_read_only_tool("echo"));
        assert!(super::is_read_only_tool("think"));
        assert!(super::is_read_only_tool("clarify"));
        assert!(super::is_read_only_tool("memory_recall"));
        assert!(super::is_read_only_tool("skill_search"));

        // NOT read-only:
        assert!(!super::is_read_only_tool("shell_exec"));
        assert!(!super::is_read_only_tool("write_file"));
        assert!(!super::is_read_only_tool("web_search")); // makes outbound HTTP
        assert!(!super::is_read_only_tool("memory_store"));
        assert!(!super::is_read_only_tool("skill_create"));
    }

    // ---- approval mode enforcement tests ----

    fn context_with_mode(mode: genesis_config::ApprovalMode) -> ToolContext {
        ToolContext {
            approval_mode: mode,
            ..crate::test_utils::test_ctx()
        }
    }

    fn context_with_mode_destructive(mode: genesis_config::ApprovalMode) -> ToolContext {
        ToolContext {
            approval_mode: mode,
            ..crate::test_utils::test_ctx_destructive()
        }
    }

    #[test]
    fn never_policy_exempt_in_all_modes() {
        use genesis_config::ApprovalMode;

        for mode in [
            ApprovalMode::Auto,
            ApprovalMode::Smart,
            ApprovalMode::Manual,
        ] {
            let mut registry = ToolRegistry::new();
            // No approval handler — so if approval were requested it would fail.
            registry.register(
                ToolDefinition {
                    name: "safe_tool".to_owned(),
                    description: "never needs approval".to_owned(),
                    parameters: None,
                },
                ApprovalPolicy::Never,
                DangerousTool,
            );

            let result = registry.execute(
                &ToolCall {
                    name: "safe_tool".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context_with_mode(mode),
            );
            assert!(
                result.is_ok(),
                "Never-policy tool should pass in {:?} mode",
                mode,
            );
        }
    }

    #[test]
    fn smart_auto_approves_read_only() {
        let mut registry = ToolRegistry::new();
        // No handler configured — read-only should still pass in Smart mode.
        registry.register(
            ToolDefinition {
                name: "read_file".to_owned(),
                description: "reads a file".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Destructive,
            DangerousTool,
        );

        let result = registry.execute(
            &ToolCall {
                name: "read_file".to_owned(),
                arguments: BTreeMap::new(),
            },
            &context_with_mode_destructive(genesis_config::ApprovalMode::Smart),
        );
        assert!(
            result.is_ok(),
            "read_file should auto-approve in Smart mode"
        );
    }

    #[test]
    fn smart_requires_approval_for_write() {
        let mut registry = ToolRegistry::new();
        // No handler configured — non-read-only should be denied in Smart mode.
        registry.register(
            ToolDefinition {
                name: "write_file".to_owned(),
                description: "writes a file".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Destructive,
            DangerousTool,
        );

        let err = registry
            .execute(
                &ToolCall {
                    name: "write_file".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context_with_mode_destructive(genesis_config::ApprovalMode::Smart),
            )
            .expect_err("write_file should require approval in Smart mode");

        assert_eq!(
            err,
            ToolError::ApprovalDenied {
                tool: "write_file".to_owned(),
                reason: "no approval handler configured".to_owned(),
            }
        );
    }

    #[test]
    fn manual_requires_approval_for_read() {
        let mut registry = ToolRegistry::new();
        // No handler — even read-only tools should be denied in Manual mode.
        registry.register(
            ToolDefinition {
                name: "read_file".to_owned(),
                description: "reads a file".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Destructive,
            DangerousTool,
        );

        let err = registry
            .execute(
                &ToolCall {
                    name: "read_file".to_owned(),
                    arguments: BTreeMap::new(),
                },
                &context_with_mode_destructive(genesis_config::ApprovalMode::Manual),
            )
            .expect_err("read_file should require approval in Manual mode");

        assert_eq!(
            err,
            ToolError::ApprovalDenied {
                tool: "read_file".to_owned(),
                reason: "no approval handler configured".to_owned(),
            }
        );
    }

    #[test]
    fn destructive_blocked_in_all_modes_when_disallowed() {
        use genesis_config::ApprovalMode;

        for mode in [
            ApprovalMode::Auto,
            ApprovalMode::Smart,
            ApprovalMode::Manual,
        ] {
            let mut registry = ToolRegistry::new();
            registry.register(
                ToolDefinition {
                    name: "rm_tool".to_owned(),
                    description: "destructive".to_owned(),
                    parameters: None,
                },
                ApprovalPolicy::Destructive,
                DangerousTool,
            );

            // allow_destructive_tools = false
            let err = registry
                .execute(
                    &ToolCall {
                        name: "rm_tool".to_owned(),
                        arguments: BTreeMap::new(),
                    },
                    &context_with_mode(mode),
                )
                .expect_err("destructive tool should be blocked");

            assert_eq!(
                err,
                ToolError::ApprovalDenied {
                    tool: "rm_tool".to_owned(),
                    reason: "destructive tools are disabled".to_owned(),
                },
                "destructive should be blocked in {:?} mode",
                mode,
            );
        }
    }

    #[test]
    fn smart_mode_with_handler_approves_non_read_only() {
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
                name: "shell_exec".to_owned(),
                description: "shell".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Destructive,
            DangerousTool,
        );

        let result = registry.execute(
            &ToolCall {
                name: "shell_exec".to_owned(),
                arguments: BTreeMap::new(),
            },
            &context_with_mode_destructive(genesis_config::ApprovalMode::Smart),
        );
        assert!(
            result.is_ok(),
            "shell_exec should pass in Smart mode when handler approves"
        );
    }

    #[test]
    fn auto_mode_allows_destructive_when_flag_set() {
        let mut registry = ToolRegistry::new();
        registry.register(
            ToolDefinition {
                name: "rm_tool".to_owned(),
                description: "destructive".to_owned(),
                parameters: None,
            },
            ApprovalPolicy::Destructive,
            DangerousTool,
        );

        // Auto mode with allow_destructive_tools = true
        let result = registry.execute(
            &ToolCall {
                name: "rm_tool".to_owned(),
                arguments: BTreeMap::new(),
            },
            &context_with_mode_destructive(genesis_config::ApprovalMode::Auto),
        );
        assert!(
            result.is_ok(),
            "Destructive tool should pass in Auto mode with allow_destructive_tools=true"
        );
    }
}
