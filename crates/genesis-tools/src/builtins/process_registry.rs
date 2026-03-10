use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

use super::shell::check_dangerous;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum size of the rolling output buffer per process (200 KB).
const MAX_OUTPUT_BUFFER: usize = 200 * 1024;

/// Maximum number of concurrent tracked processes.
const MAX_CONCURRENT_PROCESSES: usize = 64;

/// TTL for finished process records (30 minutes).
const FINISHED_TTL: Duration = Duration::from_secs(30 * 60);

/// Number of bytes to show in a poll preview.
const POLL_PREVIEW_BYTES: usize = 4096;

/// Default page size for log pagination.
const DEFAULT_PAGE_SIZE: usize = 16384;

/// Default wait timeout (60 seconds).
const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 60;

/// Shell startup noise patterns to strip from the beginning of output.
const NOISE_PATTERNS: &[&str] = &[
    "bash: cannot set terminal process group",
    "bash: no job control in this shell",
    "stdin: is not a tty",
    "mesg: ttyname failed",
    "The default interactive shell is now zsh",
    "To update your account to use zsh",
];

// ---------------------------------------------------------------------------
// Rolling output buffer
// ---------------------------------------------------------------------------

/// A rolling byte buffer that caps at a configurable size, discarding the
/// oldest bytes when new data would exceed the cap.
#[derive(Debug)]
struct RollingBuffer {
    data: Vec<u8>,
    cap: usize,
}

impl RollingBuffer {
    fn new(cap: usize) -> Self {
        Self {
            data: Vec::new(),
            cap,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        if self.data.len() > self.cap {
            let excess = self.data.len() - self.cap;
            self.data.drain(..excess);
        }
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    /// Return everything as a lossy UTF-8 string.
    fn as_str(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }

    /// Return the last `n` bytes as a lossy UTF-8 string.
    fn tail(&self, n: usize) -> String {
        if n >= self.data.len() {
            return self.as_str();
        }
        let start = self.data.len() - n;
        String::from_utf8_lossy(&self.data[start..]).into_owned()
    }

    /// Return a page of the output.
    /// `offset` is byte offset, `limit` is max bytes to return.
    fn page(&self, offset: usize, limit: usize) -> String {
        if offset >= self.data.len() {
            return String::new();
        }
        let end = (offset + limit).min(self.data.len());
        String::from_utf8_lossy(&self.data[offset..end]).into_owned()
    }
}

// ---------------------------------------------------------------------------
// Process entry
// ---------------------------------------------------------------------------

/// Status of a tracked process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Exited(i32),
    Killed,
    Unknown,
}

impl std::fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessStatus::Running => write!(f, "running"),
            ProcessStatus::Exited(code) => write!(f, "exited({code})"),
            ProcessStatus::Killed => write!(f, "killed"),
            ProcessStatus::Unknown => write!(f, "unknown"),
        }
    }
}

/// Internal entry holding process metadata, handles, and output.
struct ProcessEntry {
    /// Session-unique ID (proc_xxxxxxxxxxxx).
    id: String,
    /// The command that was launched.
    command: String,
    /// OS PID.
    pid: u32,
    /// Working directory at launch time.
    working_dir: String,
    /// When the process was started.
    start_time: SystemTime,
    /// Monotonic start for elapsed calculations.
    start_instant: Instant,
    /// Current status.
    status: ProcessStatus,
    /// Rolling output buffer (combined stdout + stderr), shared with reader threads.
    output: Arc<Mutex<RollingBuffer>>,
    /// Child stdin handle for write/submit.
    stdin: Option<std::process::ChildStdin>,
    /// When the process finished (for TTL expiry of finished records).
    finished_at: Option<Instant>,
}

impl ProcessEntry {
    fn elapsed_secs(&self) -> f64 {
        self.start_instant.elapsed().as_secs_f64()
    }

    fn is_finished(&self) -> bool {
        self.status != ProcessStatus::Running
    }

    fn is_expired(&self) -> bool {
        if let Some(finished) = self.finished_at {
            finished.elapsed() > FINISHED_TTL
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Reader thread handle
// ---------------------------------------------------------------------------

/// Tracks background reader threads for a process so we can join them.
struct ReaderHandles {
    stdout: Option<std::thread::JoinHandle<()>>,
    stderr: Option<std::thread::JoinHandle<()>>,
}

// ---------------------------------------------------------------------------
// ProcessRegistry
// ---------------------------------------------------------------------------

/// Thread-safe registry of background processes.
#[derive(Clone)]
pub struct ProcessRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

struct RegistryInner {
    /// Map from session ID to process entry.
    entries: BTreeMap<String, ProcessEntry>,
    /// Background reader threads (separate from entries to avoid lock contention).
    readers: BTreeMap<String, ReaderHandles>,
    /// Child handles that we need to wait on.
    children: BTreeMap<String, Child>,
}

impl Default for ProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner {
                entries: BTreeMap::new(),
                readers: BTreeMap::new(),
                children: BTreeMap::new(),
            })),
        }
    }

    /// Generate a unique process session ID: proc_xxxxxxxxxxxx (12 hex chars).
    fn generate_id() -> String {
        let uuid = uuid::Uuid::new_v4().to_string().replace('-', "");
        format!("proc_{}", &uuid[..12])
    }

    /// Spawn a background process and register it.
    /// Returns the session ID on success.
    pub fn spawn(
        &self,
        command: &str,
        working_dir: Option<&str>,
        backend: &Option<crate::TerminalBackend>,
    ) -> Result<String, String> {
        let mut inner = self.inner.lock().map_err(|e| format!("lock error: {e}"))?;

        // Enforce concurrency limit (count only running processes).
        let running_count = inner
            .entries
            .values()
            .filter(|e| e.status == ProcessStatus::Running)
            .count();
        if running_count >= MAX_CONCURRENT_PROCESSES {
            return Err(format!(
                "maximum concurrent background processes ({MAX_CONCURRENT_PROCESSES}) reached"
            ));
        }

        // Prune expired finished entries before spawning.
        let expired: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, e)| e.is_expired())
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            inner.entries.remove(&id);
            inner.readers.remove(&id);
            inner.children.remove(&id);
        }

        // Build and spawn the command.
        let wd_owned = working_dir.map(String::from);
        let mut cmd =
            super::shell::build_command(command, wd_owned.as_ref(), backend);

        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn: {e}"))?;

        let pid = child.id();
        let session_id = Self::generate_id();
        let stdin = child.stdin.take();

        // Create shared output buffer for live streaming.
        let output_buf = Arc::new(Mutex::new(RollingBuffer::new(MAX_OUTPUT_BUFFER)));

        // Take stdout/stderr for background reader threads.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_buf = Arc::clone(&output_buf);
        let stdout_handle = std::thread::spawn(move || {
            if let Some(mut pipe) = stdout_pipe {
                let mut chunk = [0u8; 4096];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(mut buf) = stdout_buf.lock() {
                                buf.push(&chunk[..n]);
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        let stderr_buf = Arc::clone(&output_buf);
        let stderr_handle = std::thread::spawn(move || {
            if let Some(mut pipe) = stderr_pipe {
                let mut chunk = [0u8; 4096];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(mut buf) = stderr_buf.lock() {
                                buf.push(&chunk[..n]);
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        let cwd = working_dir.unwrap_or(".").to_owned();

        let entry = ProcessEntry {
            id: session_id.clone(),
            command: command.to_owned(),
            pid,
            working_dir: cwd,
            start_time: SystemTime::now(),
            start_instant: Instant::now(),
            status: ProcessStatus::Running,
            output: output_buf,
            stdin,
            finished_at: None,
        };

        inner.entries.insert(session_id.clone(), entry);
        inner.readers.insert(
            session_id.clone(),
            ReaderHandles {
                stdout: Some(stdout_handle),
                stderr: Some(stderr_handle),
            },
        );
        inner.children.insert(session_id.clone(), child);

        Ok(session_id)
    }

    /// Check process status and join reader threads when done.
    /// Output is streamed live into the shared buffer, so no data transfer is needed here.
    fn collect_output(inner: &mut RegistryInner, id: &str) {
        // Check if process has exited.
        let child_done = if let Some(child) = inner.children.get_mut(id) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = status.code().unwrap_or(-1);
                    Some(exit_code)
                }
                Ok(None) => None,
                Err(_) => Some(-1),
            }
        } else {
            None
        };

        if let Some(exit_code) = child_done {
            // Process has exited. Join reader threads to ensure all output is flushed.
            if let Some(mut handles) = inner.readers.remove(id) {
                if let Some(h) = handles.stdout.take() {
                    let _ = h.join();
                }
                if let Some(h) = handles.stderr.take() {
                    let _ = h.join();
                }
            }
            if let Some(entry) = inner.entries.get_mut(id) {
                if entry.status == ProcessStatus::Running {
                    entry.status = ProcessStatus::Exited(exit_code);
                    entry.finished_at = Some(Instant::now());
                }
            }
            inner.children.remove(id);
        }
    }

    /// Strip shell startup noise from the beginning of output.
    fn strip_noise(output: &str) -> String {
        // Skip leading noise/empty lines using an iterator (O(n) — no shifting).
        let skip_count = output
            .lines()
            .take_while(|line| {
                let trimmed = line.trim();
                trimmed.is_empty()
                    || NOISE_PATTERNS.iter().any(|p| trimmed.starts_with(p))
            })
            .count();

        let mut result: String = output
            .lines()
            .skip(skip_count)
            .collect::<Vec<_>>()
            .join("\n");

        // Preserve trailing newline if the original had one.
        if output.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
        result
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    /// List all tracked processes.
    fn action_list(&self) -> Result<ToolOutput, ToolError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("lock error: {e}"),
            })?;

        // Prune expired entries.
        let expired: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, e)| e.is_expired())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            inner.entries.remove(id);
            inner.readers.remove(id);
            inner.children.remove(id);
        }

        // Collect output for running processes to update status.
        let ids: Vec<String> = inner.entries.keys().cloned().collect();
        for id in &ids {
            Self::collect_output(&mut inner, id);
        }

        if inner.entries.is_empty() {
            return Ok(ToolOutput {
                content: "No tracked background processes.".to_owned(),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), "process".to_owned()),
                    ("action".to_owned(), "list".to_owned()),
                    ("count".to_owned(), "0".to_owned()),
                ]),
            });
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "{:<18} {:<10} {:<8} {:<10} {:<10} {:<10} {}",
            "ID", "STATUS", "PID", "STARTED", "ELAPSED", "OUTPUT", "COMMAND"
        ));

        for entry in inner.entries.values() {
            let elapsed = format!("{:.1}s", entry.elapsed_secs());
            let output_len = entry.output.lock().map(|b| b.len()).unwrap_or(0);
            let output_size = if output_len > 1024 {
                format!("{}KB", output_len / 1024)
            } else {
                format!("{}B", output_len)
            };
            let cmd_display = if entry.command.len() > 50 {
                format!("{}...", &entry.command[..47])
            } else {
                entry.command.clone()
            };
            let start_str = match entry.start_time.duration_since(SystemTime::UNIX_EPOCH) {
                Ok(d) => {
                    let secs = d.as_secs();
                    // Simple HH:MM:SS format
                    let h = (secs / 3600) % 24;
                    let m = (secs / 60) % 60;
                    let s = secs % 60;
                    format!("{h:02}:{m:02}:{s:02}")
                }
                Err(_) => "unknown".to_owned(),
            };
            lines.push(format!(
                "{:<18} {:<10} {:<8} {:<10} {:<10} {:<10} {}",
                entry.id, entry.status, entry.pid, start_str, elapsed, output_size, cmd_display
            ));
        }

        let count = inner.entries.len();
        Ok(ToolOutput {
            content: lines.join("\n"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), "process".to_owned()),
                ("action".to_owned(), "list".to_owned()),
                ("count".to_owned(), count.to_string()),
            ]),
        })
    }

    /// Poll a process for status and output preview.
    fn action_poll(&self, id: &str) -> Result<ToolOutput, ToolError> {
        let mut inner = self.inner.lock().map_err(|e| ToolError::ExecutionFailed {
            tool: "process".to_owned(),
            reason: format!("lock error: {e}"),
        })?;

        Self::collect_output(&mut inner, id);

        let entry = inner
            .entries
            .get(id)
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("no process with id `{id}`"),
            })?;

        let (preview, total_bytes) = {
            let buf = entry.output.lock().map_err(|e| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("output lock error: {e}"),
            })?;
            (Self::strip_noise(&buf.tail(POLL_PREVIEW_BYTES)), buf.len())
        };
        let status = entry.status.to_string();
        let elapsed = format!("{:.1}s", entry.elapsed_secs());

        let mut content = format!(
            "Process: {}\nCommand: {}\nPID: {}\nWorking Dir: {}\nStatus: {status}\nElapsed: {elapsed}\nOutput ({total_bytes} bytes total, last {POLL_PREVIEW_BYTES}):\n",
            entry.id, entry.command, entry.pid, entry.working_dir
        );
        if preview.is_empty() {
            content.push_str("(no output yet)");
        } else {
            content.push_str(&preview);
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), "process".to_owned()),
                ("action".to_owned(), "poll".to_owned()),
                ("status".to_owned(), status),
                ("id".to_owned(), id.to_owned()),
            ]),
        })
    }

    /// Get full log output with pagination.
    fn action_log(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ToolOutput, ToolError> {
        let mut inner = self.inner.lock().map_err(|e| ToolError::ExecutionFailed {
            tool: "process".to_owned(),
            reason: format!("lock error: {e}"),
        })?;

        Self::collect_output(&mut inner, id);

        let entry = inner
            .entries
            .get(id)
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("no process with id `{id}`"),
            })?;

        let (page, total_bytes) = {
            let buf = entry.output.lock().map_err(|e| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("output lock error: {e}"),
            })?;
            (Self::strip_noise(&buf.page(offset, limit)), buf.len())
        };
        let has_more = offset + limit < total_bytes;

        let mut content = format!(
            "Log for {} (bytes {offset}..{}, total {total_bytes}):\n",
            entry.id,
            (offset + limit).min(total_bytes)
        );
        if page.is_empty() {
            content.push_str("(no output in this range)");
        } else {
            content.push_str(&page);
        }
        if has_more {
            content.push_str(&format!(
                "\n--- more output available (next offset: {}) ---",
                offset + limit
            ));
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), "process".to_owned()),
                ("action".to_owned(), "log".to_owned()),
                ("id".to_owned(), id.to_owned()),
                ("total_bytes".to_owned(), total_bytes.to_string()),
                ("has_more".to_owned(), has_more.to_string()),
            ]),
        })
    }

    /// Wait for a process to finish, with timeout.
    fn action_wait(&self, id: &str, timeout_secs: u64) -> Result<ToolOutput, ToolError> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);

        loop {
            {
                let mut inner =
                    self.inner.lock().map_err(|e| ToolError::ExecutionFailed {
                        tool: "process".to_owned(),
                        reason: format!("lock error: {e}"),
                    })?;

                Self::collect_output(&mut inner, id);

                let entry =
                    inner
                        .entries
                        .get(id)
                        .ok_or_else(|| ToolError::ExecutionFailed {
                            tool: "process".to_owned(),
                            reason: format!("no process with id `{id}`"),
                        })?;

                if entry.is_finished() {
                    let (preview, total_bytes) = {
                        let buf = entry.output.lock().map_err(|e| ToolError::ExecutionFailed {
                            tool: "process".to_owned(),
                            reason: format!("output lock error: {e}"),
                        })?;
                        (Self::strip_noise(&buf.tail(POLL_PREVIEW_BYTES)), buf.len())
                    };
                    let status = entry.status.to_string();
                    let elapsed = format!("{:.1}s", entry.elapsed_secs());

                    let mut content = format!(
                        "Process {} finished.\nStatus: {status}\nElapsed: {elapsed}\nOutput ({total_bytes} bytes, last {POLL_PREVIEW_BYTES}):\n",
                        entry.id
                    );
                    if preview.is_empty() {
                        content.push_str("(no output)");
                    } else {
                        content.push_str(&preview);
                    }

                    return Ok(ToolOutput {
                        content,
                        metadata: BTreeMap::from([
                            ("tool".to_owned(), "process".to_owned()),
                            ("action".to_owned(), "wait".to_owned()),
                            ("status".to_owned(), status),
                            ("id".to_owned(), id.to_owned()),
                        ]),
                    });
                }
            }

            if Instant::now() >= deadline {
                return Err(ToolError::ExecutionFailed {
                    tool: "process".to_owned(),
                    reason: format!(
                        "timed out waiting for process `{id}` after {timeout_secs}s. \
                         Use `poll` to check status or increase the timeout."
                    ),
                });
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Kill a process via SIGTERM.
    fn action_kill(&self, id: &str) -> Result<ToolOutput, ToolError> {
        let mut inner = self.inner.lock().map_err(|e| ToolError::ExecutionFailed {
            tool: "process".to_owned(),
            reason: format!("lock error: {e}"),
        })?;

        Self::collect_output(&mut inner, id);

        let entry = inner
            .entries
            .get_mut(id)
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("no process with id `{id}`"),
            })?;

        if entry.is_finished() {
            return Ok(ToolOutput {
                content: format!("Process {} is already finished (status: {})", id, entry.status),
                metadata: BTreeMap::from([
                    ("tool".to_owned(), "process".to_owned()),
                    ("action".to_owned(), "kill".to_owned()),
                    ("id".to_owned(), id.to_owned()),
                    ("status".to_owned(), entry.status.to_string()),
                ]),
            });
        }

        // Send termination signal.
        let pid = entry.pid;
        #[cfg(unix)]
        {
            // Use SIGTERM for graceful shutdown on Unix.
            let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                return Err(ToolError::ExecutionFailed {
                    tool: "process".to_owned(),
                    reason: format!("failed to send SIGTERM to process `{id}` (pid {pid}): {err}"),
                });
            }
        }
        #[cfg(not(unix))]
        {
            if let Some(child) = inner.children.get_mut(id) {
                child.kill().map_err(|e| ToolError::ExecutionFailed {
                    tool: "process".to_owned(),
                    reason: format!("failed to kill process `{id}`: {e}"),
                })?;
            }
        }

        // Update status.
        if let Some(entry) = inner.entries.get_mut(id) {
            entry.status = ProcessStatus::Killed;
            entry.finished_at = Some(Instant::now());
        }

        Ok(ToolOutput {
            content: format!("Sent SIGTERM to process {id}"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), "process".to_owned()),
                ("action".to_owned(), "kill".to_owned()),
                ("id".to_owned(), id.to_owned()),
            ]),
        })
    }

    /// Write raw data to process stdin.
    fn action_write(&self, id: &str, data: &str) -> Result<ToolOutput, ToolError> {
        let mut inner = self.inner.lock().map_err(|e| ToolError::ExecutionFailed {
            tool: "process".to_owned(),
            reason: format!("lock error: {e}"),
        })?;

        Self::collect_output(&mut inner, id);

        let entry = inner
            .entries
            .get_mut(id)
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("no process with id `{id}`"),
            })?;

        if entry.is_finished() {
            return Err(ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("process `{id}` is already finished"),
            });
        }

        let stdin = entry
            .stdin
            .as_mut()
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("stdin not available for process `{id}`"),
            })?;

        stdin
            .write_all(data.as_bytes())
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "process".to_owned(),
                reason: format!("failed to write to stdin of `{id}`: {e}"),
            })?;

        stdin.flush().map_err(|e| ToolError::ExecutionFailed {
            tool: "process".to_owned(),
            reason: format!("failed to flush stdin of `{id}`: {e}"),
        })?;

        Ok(ToolOutput {
            content: format!("Wrote {} bytes to stdin of {id}", data.len()),
            metadata: BTreeMap::from([
                ("tool".to_owned(), "process".to_owned()),
                ("action".to_owned(), "write".to_owned()),
                ("id".to_owned(), id.to_owned()),
                ("bytes_written".to_owned(), data.len().to_string()),
            ]),
        })
    }

    /// Submit data + newline to process stdin.
    fn action_submit(&self, id: &str, data: &str) -> Result<ToolOutput, ToolError> {
        let data_with_newline = format!("{data}\n");
        self.action_write(id, &data_with_newline)
            .map(|mut output| {
                // Fix up metadata to say "submit" not "write".
                output.metadata.insert("action".to_owned(), "submit".to_owned());
                output.content = format!("Submitted {} bytes (with newline) to stdin of {id}", data.len() + 1);
                output
            })
    }
}

// ---------------------------------------------------------------------------
// ProcessTool — ToolHandler that dispatches to ProcessRegistry actions
// ---------------------------------------------------------------------------

pub struct ProcessTool {
    pub registry: ProcessRegistry,
}

impl ToolHandler for ProcessTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = call
            .arguments
            .get("action")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "action",
            })?;

        match action.as_str() {
            "list" => self.registry.action_list(),
            "poll" => {
                let id = require_arg(call, "id")?;
                self.registry.action_poll(id)
            }
            "log" => {
                let id = require_arg(call, "id")?;
                let offset = call
                    .arguments
                    .get("offset")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let limit = call
                    .arguments
                    .get("limit")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_PAGE_SIZE);
                self.registry.action_log(id, offset, limit)
            }
            "wait" => {
                let id = require_arg(call, "id")?;
                let timeout = call
                    .arguments
                    .get("timeout")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS);
                self.registry.action_wait(id, timeout)
            }
            "kill" => {
                let id = require_arg(call, "id")?;
                self.registry.action_kill(id)
            }
            "write" => {
                let id = require_arg(call, "id")?;
                let data = require_arg(call, "data")?;
                self.registry.action_write(id, data)
            }
            "submit" => {
                let id = require_arg(call, "id")?;
                let data = require_arg(call, "data")?;
                self.registry.action_submit(id, data)
            }
            other => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "unknown action `{other}`. Valid actions: list, poll, log, wait, kill, write, submit"
                ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// BackgroundShellTool — wraps shell_exec with background: true
// ---------------------------------------------------------------------------

/// A version of ShellExecTool that supports a `background` parameter.
/// When `background` is "true", the command is spawned as a background process
/// registered in the ProcessRegistry, and the session ID is returned immediately.
pub struct BackgroundShellExecTool {
    pub registry: ProcessRegistry,
}

impl ToolHandler for BackgroundShellExecTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = call
            .arguments
            .get("command")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "command",
            })?;

        let background = call
            .arguments
            .get("background")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        if !background {
            // Delegate to the synchronous ShellExecTool.
            let sync_tool = super::shell::ShellExecTool;
            return sync_tool.run(call, context);
        }

        // Background mode: check for dangerous patterns first.
        // Skip the check for sandboxed backends — the container is the security boundary.
        if !super::shell::is_sandboxed_backend(&context.terminal_backend) {
            if let Some(danger) = check_dangerous(command) {
                return Err(ToolError::ApprovalDenied {
                    tool: call.name.clone(),
                    reason: format!(
                        "command blocked: {danger}. Command: `{}`",
                        if command.len() > 80 {
                            format!("{}...", &command[..77])
                        } else {
                            command.clone()
                        }
                    ),
                });
            }
        }

        let working_dir = call
            .arguments
            .get("working_dir")
            .or(context.default_working_dir.as_ref());

        let session_id = self
            .registry
            .spawn(command, working_dir.map(|s| s.as_str()), &context.terminal_backend)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: e,
            })?;

        Ok(ToolOutput {
            content: format!(
                "Background process started.\nSession ID: {session_id}\nCommand: {command}\n\n\
                 Use `process` tool with action `poll` and id `{session_id}` to check status and output."
            ),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("background".to_owned(), "true".to_owned()),
                ("session_id".to_owned(), session_id),
            ]),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_arg<'a>(call: &'a ToolCall, name: &'static str) -> Result<&'a str, ToolError> {
    call.arguments
        .get(name)
        .map(|s| s.as_str())
        .ok_or_else(|| ToolError::MissingArgument {
            tool: call.name.clone(),
            argument: name,
        })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    fn make_call(action: &str, extra: &[(&str, &str)]) -> ToolCall {
        let mut args = BTreeMap::from([("action".to_owned(), action.to_owned())]);
        for &(k, v) in extra {
            args.insert(k.to_owned(), v.to_owned());
        }
        ToolCall {
            name: "process".to_owned(),
            arguments: args,
        }
    }

    // -----------------------------------------------------------------------
    // RollingBuffer
    // -----------------------------------------------------------------------

    #[test]
    fn rolling_buffer_basic() {
        let mut buf = RollingBuffer::new(10);
        buf.push(b"hello");
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.as_str(), "hello");
    }

    #[test]
    fn rolling_buffer_rolls_over() {
        let mut buf = RollingBuffer::new(10);
        buf.push(b"abcdefghij"); // exactly at cap
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.as_str(), "abcdefghij");

        buf.push(b"XYZ"); // exceeds cap by 3
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.as_str(), "defghijXYZ");
    }

    #[test]
    fn rolling_buffer_tail() {
        let mut buf = RollingBuffer::new(100);
        buf.push(b"0123456789");
        assert_eq!(buf.tail(4), "6789");
        assert_eq!(buf.tail(100), "0123456789"); // more than available
    }

    #[test]
    fn rolling_buffer_page() {
        let mut buf = RollingBuffer::new(100);
        buf.push(b"abcdefghij");
        assert_eq!(buf.page(0, 5), "abcde");
        assert_eq!(buf.page(5, 5), "fghij");
        assert_eq!(buf.page(5, 100), "fghij");
        assert_eq!(buf.page(100, 5), "");
    }

    // -----------------------------------------------------------------------
    // Noise stripping
    // -----------------------------------------------------------------------

    #[test]
    fn strip_noise_removes_startup_lines() {
        let input = "bash: cannot set terminal process group (-1): Inappropriate ioctl for device\nbash: no job control in this shell\nhello world\n";
        let result = ProcessRegistry::strip_noise(input);
        assert_eq!(result, "hello world\n");
    }

    #[test]
    fn strip_noise_preserves_clean_output() {
        let input = "line1\nline2\nline3\n";
        let result = ProcessRegistry::strip_noise(input);
        assert_eq!(result, "line1\nline2\nline3\n");
    }

    #[test]
    fn strip_noise_handles_empty_input() {
        let result = ProcessRegistry::strip_noise("");
        assert_eq!(result, "");
    }

    // -----------------------------------------------------------------------
    // ProcessRegistry spawn + list + poll + wait + kill
    // -----------------------------------------------------------------------

    #[test]
    fn spawn_and_list() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("echo hello", None, &None)
            .expect("should spawn");

        assert!(id.starts_with("proc_"));
        assert_eq!(id.len(), 17); // "proc_" + 12 hex chars

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("list", &[]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains(&id));
    }

    #[test]
    fn spawn_and_poll() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("echo hello_poll", None, &None)
            .expect("should spawn");

        // Wait a bit for the process to finish.
        std::thread::sleep(Duration::from_millis(200));

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("poll", &[("id", &id)]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("hello_poll"));
        let status = output.metadata.get("status").unwrap();
        assert!(
            status == "exited(0)" || status == "running",
            "unexpected status: {status}"
        );
    }

    #[test]
    fn spawn_and_wait() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("echo hello_wait", None, &None)
            .expect("should spawn");

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("wait", &[("id", &id), ("timeout", "10")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("finished"));
        assert!(output.content.contains("hello_wait"));
        assert_eq!(output.metadata.get("status").unwrap(), "exited(0)");
    }

    #[test]
    fn spawn_and_kill() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("sleep 300", None, &None)
            .expect("should spawn");

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("kill", &[("id", &id)]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("SIGTERM"));
    }

    #[test]
    fn wait_timeout_returns_error() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("sleep 300", None, &None)
            .expect("should spawn");

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("wait", &[("id", &id), ("timeout", "1")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("timed out"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }

        // Clean up.
        let kill_call = make_call("kill", &[("id", &id)]);
        let _ = tool.run(&kill_call, &ctx());
    }

    #[test]
    fn log_with_pagination() {
        let registry = ProcessRegistry::new();
        // Generate some output.
        let id = registry
            .spawn("echo 'line1'; echo 'line2'; echo 'line3'", None, &None)
            .expect("should spawn");

        // Wait for completion.
        std::thread::sleep(Duration::from_millis(300));

        let tool = ProcessTool {
            registry: registry.clone(),
        };

        // Get full log.
        let call = make_call("log", &[("id", &id), ("offset", "0"), ("limit", "16384")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("line1"));
        assert!(output.content.contains("line2"));
        assert!(output.content.contains("line3"));
    }

    #[test]
    fn poll_nonexistent_process_returns_error() {
        let registry = ProcessRegistry::new();
        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("poll", &[("id", "proc_doesnotexist")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn invalid_action_returns_error() {
        let registry = ProcessRegistry::new();
        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("bogus", &[]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unknown action"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[test]
    fn missing_action_returns_error() {
        let registry = ProcessRegistry::new();
        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = ToolCall {
            name: "process".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    // -----------------------------------------------------------------------
    // Write / Submit
    // -----------------------------------------------------------------------

    #[test]
    fn write_to_running_process() {
        let registry = ProcessRegistry::new();
        // cat will read stdin and echo it back.
        let id = registry
            .spawn("cat", None, &None)
            .expect("should spawn");

        std::thread::sleep(Duration::from_millis(100));

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("write", &[("id", &id), ("data", "hello\n")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Wrote"));

        // Kill the cat process.
        let kill_call = make_call("kill", &[("id", &id)]);
        let _ = tool.run(&kill_call, &ctx());
    }

    #[test]
    fn submit_adds_newline() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("cat", None, &None)
            .expect("should spawn");

        std::thread::sleep(Duration::from_millis(100));

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("submit", &[("id", &id), ("data", "test_data")]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Submitted"));
        assert!(output.metadata.get("action").unwrap() == "submit");

        // Kill the cat process.
        let kill_call = make_call("kill", &[("id", &id)]);
        let _ = tool.run(&kill_call, &ctx());
    }

    #[test]
    fn write_to_finished_process_fails() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("echo done", None, &None)
            .expect("should spawn");

        // Wait for completion.
        std::thread::sleep(Duration::from_millis(300));

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("write", &[("id", &id), ("data", "hello")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("already finished"));
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // BackgroundShellExecTool
    // -----------------------------------------------------------------------

    #[test]
    fn background_shell_exec_foreground_mode() {
        let registry = ProcessRegistry::new();
        let tool = BackgroundShellExecTool {
            registry: registry.clone(),
        };
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::from([("command".to_owned(), "echo fg_mode".to_owned())]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("fg_mode"));
        // Should not have background metadata.
        assert!(!output.metadata.contains_key("session_id"));
    }

    #[test]
    fn background_shell_exec_background_mode() {
        let registry = ProcessRegistry::new();
        let tool = BackgroundShellExecTool {
            registry: registry.clone(),
        };
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::from([
                ("command".to_owned(), "echo bg_mode".to_owned()),
                ("background".to_owned(), "true".to_owned()),
            ]),
        };
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("Background process started"));
        assert!(output.metadata.contains_key("session_id"));
        let session_id = output.metadata.get("session_id").unwrap();
        assert!(session_id.starts_with("proc_"));
    }

    #[test]
    fn background_shell_exec_blocks_dangerous() {
        let registry = ProcessRegistry::new();
        let tool = BackgroundShellExecTool {
            registry: registry.clone(),
        };
        let call = ToolCall {
            name: "shell_exec".to_owned(),
            arguments: BTreeMap::from([
                ("command".to_owned(), "rm -rf /".to_owned()),
                ("background".to_owned(), "true".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ApprovalDenied { .. }));
    }

    // -----------------------------------------------------------------------
    // Concurrency limit
    // -----------------------------------------------------------------------

    #[test]
    fn concurrency_limit_enforced() {
        let registry = ProcessRegistry::new();

        // Spawn MAX_CONCURRENT_PROCESSES sleep commands.
        let mut ids = Vec::new();
        for _ in 0..MAX_CONCURRENT_PROCESSES {
            let id = registry
                .spawn("sleep 300", None, &None)
                .expect("should spawn");
            ids.push(id);
        }

        // The next one should fail.
        let err = registry.spawn("sleep 300", None, &None);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("maximum"));

        // Clean up: kill all.
        for id in &ids {
            let _ = registry.action_kill(id);
        }
    }

    // -----------------------------------------------------------------------
    // Generate ID format
    // -----------------------------------------------------------------------

    #[test]
    fn generate_id_format() {
        let id = ProcessRegistry::generate_id();
        assert!(id.starts_with("proc_"));
        assert_eq!(id.len(), 17); // proc_ (5) + 12 hex chars
    }

    // -----------------------------------------------------------------------
    // Kill already finished process
    // -----------------------------------------------------------------------

    #[test]
    fn kill_finished_process_is_noop() {
        let registry = ProcessRegistry::new();
        let id = registry
            .spawn("echo done", None, &None)
            .expect("should spawn");

        // Wait for completion.
        std::thread::sleep(Duration::from_millis(300));

        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("kill", &[("id", &id)]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("already finished"));
    }

    // -----------------------------------------------------------------------
    // List empty registry
    // -----------------------------------------------------------------------

    #[test]
    fn list_empty_registry() {
        let registry = ProcessRegistry::new();
        let tool = ProcessTool {
            registry: registry.clone(),
        };
        let call = make_call("list", &[]);
        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("No tracked background processes"));
        assert_eq!(output.metadata.get("count").unwrap(), "0");
    }
}
