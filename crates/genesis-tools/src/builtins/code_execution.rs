use std::collections::BTreeMap;
use std::io::Read;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use crate::{truncate_output_bytes, ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Tools allowed inside the PTC sandbox. Maps Python function name to Genesis tool name.
const SANDBOX_TOOLS: &[(&str, &str)] = &[
    ("terminal", "shell_exec"),
    ("read_file", "read_file"),
    ("write_file", "write_file"),
    ("search_files", "search_files"),
    ("patch", "patch"),
    ("web_search", "web_search"),
    ("browse", "browse"),
];

/// Resource limits for PTC execution.
const PTC_TIMEOUT_SECS: u64 = 300; // 5 minutes
const PTC_MAX_TOOL_CALLS: usize = 50;
const PTC_MAX_STDOUT: usize = 50_000; // 50 KB
const PTC_MAX_STDERR: usize = 10_000; // 10 KB

/// Sandboxed Python code execution tool (basic mode).
///
/// Runs Python code in a subprocess with a stripped environment (no access to
/// secrets or host env vars). The agent can use this for computation, data
/// analysis, and scripting without resorting to raw shell access.
///
/// For PTC (Programmatic Tool Calling) mode with tool RPC access, use
/// [`execute_code_ptc`] which is dispatched via `ToolRuntime::execute_async`.
pub struct CodeExecutionTool;

/// Environment variables allowed into the sandboxed Python process.
const ALLOWED_ENV: &[&str] = &["PATH", "HOME", "LANG", "TERM", "TMPDIR"];

fn is_no_process_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(3)
}

impl ToolHandler for CodeExecutionTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let code = call
            .arguments
            .get("code")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "code",
            })?;

        let timeout_secs: u64 = call
            .arguments
            .get("timeout_secs")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let language = call
            .arguments
            .get("language")
            .map(|s| s.as_str())
            .unwrap_or("python");

        let (program, args) = match language {
            "python" | "python3" => ("python3", vec!["-c", code.as_str()]),
            "node" | "javascript" | "js" => ("node", vec!["-e", code.as_str()]),
            "ruby" => ("ruby", vec!["-e", code.as_str()]),
            other => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!(
                        "unsupported language '{other}'. Supported: python, node, ruby"
                    ),
                });
            }
        };

        let mut cmd = Command::new(program);
        cmd.args(&args);

        // Sandbox: clear env, only pass safe vars through
        cmd.env_clear();
        for key in ALLOWED_ENV {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        // Ensure Python doesn't buffer output
        cmd.env("PYTHONUNBUFFERED", "1");

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to spawn {program}: {e}"),
            })?;

        let tool_name = call.name.clone();
        let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let stdout_handle = {
            let stdout = child.stdout.take();
            let stdout_buf = stdout_buf.clone();
            std::thread::spawn(move || {
                if let Some(mut stdout) = stdout {
                    if let Ok(mut buf) = stdout_buf.lock() {
                        let _ = stdout.read_to_end(&mut buf);
                    }
                }
            })
        };

        let stderr_handle = {
            let stderr = child.stderr.take();
            let stderr_buf = stderr_buf.clone();
            std::thread::spawn(move || {
                if let Some(mut stderr) = stderr {
                    if let Ok(mut buf) = stderr_buf.lock() {
                        let _ = stderr.read_to_end(&mut buf);
                    }
                }
            })
        };

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let mut timed_out = false;
        let mut status: Option<std::process::ExitStatus> = None;
        loop {
            match child.try_wait() {
                Ok(Some(child_status)) => {
                    status = Some(child_status);
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;

                        if let Ok(Some(child_status)) = child.try_wait() {
                            status = Some(child_status);
                            timed_out = false;
                            break;
                        }

                        if let Err(err) = child.kill() {
                            if !is_no_process_error(&err) {
                                let _ = child.wait();
                                return Err(ToolError::ExecutionFailed {
                                    tool: tool_name,
                                    reason: format!("failed to kill timed-out process: {err}"),
                                });
                            }
                        }

                        if let Ok(child_status) = child.wait() {
                            status = Some(child_status);
                        }
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    return Err(ToolError::ExecutionFailed {
                        tool: tool_name,
                        reason: format!("error checking code execution status: {err}"),
                    });
                }
            }
        }

        let _ = stdout_handle.join();
        let _ = stderr_handle.join();

        if timed_out {
            return Err(ToolError::ExecutionFailed {
                tool: tool_name,
                reason: format!(
                    "code execution timed out after {timeout_secs}s. \
                     Use the `timeout_secs` argument to increase the limit."
                ),
            });
        }

        let output_status = status.ok_or_else(|| ToolError::ExecutionFailed {
            tool: tool_name.clone(),
            reason: "code execution exited without a status".to_owned(),
        })?;
        let stdout = stdout_buf
            .lock()
            .map(|buf| truncate_output_bytes(&buf))
            .unwrap_or_default();
        let stderr = stderr_buf
            .lock()
            .map(|buf| truncate_output_bytes(&buf))
            .unwrap_or_default();
        let exit_code = output_status.code().unwrap_or(-1);

        let mut content = String::new();
        if !stdout.is_empty() {
            content.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("[stderr]\n");
            content.push_str(&stderr);
        }
        if content.is_empty() {
            content = format!("(no output, exit code {exit_code})");
        }

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("language".to_owned(), language.to_owned()),
                ("exit_code".to_owned(), exit_code.to_string()),
            ]),
        })
    }
}

// ===========================================================================
// PTC (Programmatic Tool Calling) — execute Python with tool RPC access
// ===========================================================================

/// Execute Python code with PTC (Programmatic Tool Calling).
///
/// Creates a sandbox with `genesis_tools.py` stubs, a Unix domain socket
/// for RPC, and runs the script in a child process. Tool calls from the
/// script are dispatched through the provided `ToolRegistry`.
///
/// This is called from `ToolRuntime::execute_async` in genesis-core, which
/// intercepts `execute_code` and routes it here with the registry and context.
pub fn execute_code_ptc(
    code: &str,
    registry: &crate::ToolRegistry,
    context: &ToolContext,
) -> ToolOutput {
    let exec_start = std::time::Instant::now();
    let call_count = Arc::new(AtomicUsize::new(0));

    // Create temp directory for the script and stubs
    let tmpdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => return ptc_error(&format!("failed to create temp dir: {e}"), 0, exec_start),
    };

    // Write genesis_tools.py stub module
    let tools_src = generate_genesis_tools_module();
    if let Err(e) = std::fs::write(tmpdir.path().join("genesis_tools.py"), &tools_src) {
        return ptc_error(
            &format!("failed to write genesis_tools.py: {e}"),
            0,
            exec_start,
        );
    }

    // Write the user's script
    if let Err(e) = std::fs::write(tmpdir.path().join("script.py"), code) {
        return ptc_error(&format!("failed to write script.py: {e}"), 0, exec_start);
    }

    // Create UDS socket
    let sock_path = std::env::temp_dir().join(format!("genesis-ptc-{}.sock", uuid::Uuid::new_v4()));

    let listener = match std::os::unix::net::UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => return ptc_error(&format!("failed to bind UDS: {e}"), 0, exec_start),
    };

    // Spawn RPC server thread
    let rpc_registry = registry.clone();
    let rpc_context = context.clone();
    let rpc_call_count = call_count.clone();
    let rpc_thread = std::thread::spawn(move || {
        rpc_server_loop(
            listener,
            &rpc_registry,
            &rpc_context,
            rpc_call_count,
            PTC_MAX_TOOL_CALLS,
        );
    });

    // Build sandboxed environment for child process
    let child_env = build_sandbox_env(&sock_path);

    // Spawn child process
    let mut child = match Command::new("python3")
        .arg("script.py")
        .current_dir(tmpdir.path())
        .env_clear()
        .envs(child_env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&sock_path);
            return ptc_error(&format!("failed to spawn python3: {e}"), 0, exec_start);
        }
    };

    // Drain stdout/stderr in background threads to avoid pipe deadlocks
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let stdout_handle = {
        let buf = stdout_buf.clone();
        std::thread::spawn(move || drain_pipe(stdout_pipe, buf, PTC_MAX_STDOUT))
    };
    let stderr_handle = {
        let buf = stderr_buf.clone();
        std::thread::spawn(move || drain_pipe(stderr_pipe, buf, PTC_MAX_STDERR))
    };

    // Poll child process with timeout
    let deadline = std::time::Instant::now() + Duration::from_secs(PTC_TIMEOUT_SECS);
    let mut status = "success";

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    status = "timeout";
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => break,
        }
    }

    // Wait for drainer threads
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    let stdout_bytes = stdout_buf.lock().expect("stdout buffer lock poisoned");
    let stderr_bytes = stderr_buf.lock().expect("stderr buffer lock poisoned");
    let mut stdout_text = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).to_string();

    if stdout_text.len() >= PTC_MAX_STDOUT {
        stdout_text.truncate(PTC_MAX_STDOUT);
        stdout_text.push_str("\n[output truncated at 50KB]");
    }

    let exit_code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    let duration = exec_start.elapsed().as_secs_f64();
    let tool_calls_made = call_count.load(Ordering::Relaxed);

    // Cleanup: remove socket, RPC thread will exit when listener is dropped
    let _ = std::fs::remove_file(&sock_path);
    let _ = rpc_thread.join();

    // Build output
    let mut content = String::new();
    match status {
        "timeout" => {
            content.push_str(&format!(
                "Script timed out after {PTC_TIMEOUT_SECS}s and was killed.\n"
            ));
            if !stdout_text.is_empty() {
                content.push_str(&stdout_text);
            }
        }
        _ if exit_code != 0 => {
            status = "error";
            content.push_str(&stdout_text);
            if !stderr_text.is_empty() {
                content.push_str("\n--- stderr ---\n");
                content.push_str(&stderr_text);
            }
        }
        _ => {
            content.push_str(&stdout_text);
        }
    }

    if content.is_empty() {
        content = format!("(no output, exit code {exit_code})");
    }

    ToolOutput {
        content,
        metadata: BTreeMap::from([
            ("tool".to_owned(), "execute_code".to_owned()),
            ("status".to_owned(), status.to_owned()),
            ("exit_code".to_owned(), exit_code.to_string()),
            ("tool_calls_made".to_owned(), tool_calls_made.to_string()),
            ("duration_seconds".to_owned(), format!("{duration:.2}")),
        ]),
    }
}

// ---------------------------------------------------------------------------
// RPC server
// ---------------------------------------------------------------------------

/// Run the RPC server loop. Accepts one client connection and dispatches
/// tool calls until the client disconnects or the call limit is reached.
///
/// Wire protocol: newline-delimited JSON.
/// Request: `{"tool": "shell_exec", "args": {"command": "ls"}}\n`
/// Response: `<tool output content>\n`
fn rpc_server_loop(
    listener: std::os::unix::net::UnixListener,
    registry: &crate::ToolRegistry,
    context: &ToolContext,
    call_count: Arc<AtomicUsize>,
    max_calls: usize,
) {
    use std::io::{BufRead, Write};

    // Accept one connection with a timeout
    let stream = match accept_with_timeout(&listener, Duration::from_secs(10)) {
        Some(s) => s,
        None => return,
    };
    stream.set_read_timeout(Some(Duration::from_secs(300))).ok();

    let mut reader = std::io::BufReader::new(&stream);
    let mut writer = &stream;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or error
            Ok(_) => {}
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse the RPC request
        let request: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let resp = serde_json::json!({"error": format!("Invalid RPC request: {e}")});
                let _ = writeln!(writer, "{resp}");
                continue;
            }
        };

        let tool_name = request["tool"].as_str().unwrap_or("").to_owned();
        let args_value = request
            .get("args")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Check sandbox allow-list
        let allowed = SANDBOX_TOOLS
            .iter()
            .any(|(_, genesis)| *genesis == tool_name);
        if !allowed {
            let available: Vec<&str> = SANDBOX_TOOLS.iter().map(|(py, _)| *py).collect();
            let resp = serde_json::json!({
                "error": format!(
                    "Tool '{}' is not available in execute_code. Available: {}",
                    tool_name,
                    available.join(", ")
                )
            });
            let _ = writeln!(writer, "{resp}");
            continue;
        }

        // Enforce call limit
        let current = call_count.load(Ordering::Relaxed);
        if current >= max_calls {
            let resp = serde_json::json!({
                "error": format!("Tool call limit reached ({max_calls}). No more calls allowed.")
            });
            let _ = writeln!(writer, "{resp}");
            continue;
        }

        // Convert JSON args to BTreeMap<String, String>
        let arguments = flatten_args(&args_value);

        // Dispatch through the tool registry
        let call = ToolCall {
            name: tool_name,
            arguments,
        };

        let result = match registry.execute(&call, context) {
            Ok(output) => {
                // Try to parse as JSON first; if not valid JSON, wrap as string
                if serde_json::from_str::<serde_json::Value>(&output.content).is_ok() {
                    output.content
                } else {
                    serde_json::to_string(&output.content).unwrap_or(output.content)
                }
            }
            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
        };

        call_count.fetch_add(1, Ordering::Relaxed);
        let _ = writeln!(writer, "{result}");
    }
}

/// Accept a connection with a timeout by polling in a loop.
fn accept_with_timeout(
    listener: &std::os::unix::net::UnixListener,
    timeout: Duration,
) -> Option<std::os::unix::net::UnixStream> {
    listener.set_nonblocking(true).ok();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

/// Flatten a JSON object's values to strings for ToolCall arguments.
fn flatten_args(value: &serde_json::Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => continue,
                other => other.to_string(),
            };
            map.insert(k.clone(), s);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Python stub generator
// ---------------------------------------------------------------------------

/// Generate the `genesis_tools.py` Python stub module.
///
/// Provides typed Python functions that communicate with the parent
/// process via a Unix domain socket. The child process imports these stubs and
/// calls them like regular functions; the calls are routed back to Genesis tools.
pub fn generate_genesis_tools_module() -> String {
    String::from(
        r#""""Auto-generated Genesis tools RPC stubs."""
import json, os, socket, shlex, time

_sock = None

def json_parse(text: str):
    """Parse JSON tolerant of control characters (strict=False)."""
    return json.loads(text, strict=False)

def shell_quote(s: str) -> str:
    """Shell-escape a string for safe interpolation into commands."""
    return shlex.quote(s)

def retry(fn, max_attempts=3, delay=2):
    """Retry a function up to max_attempts times with exponential backoff."""
    last_err = None
    for attempt in range(max_attempts):
        try:
            return fn()
        except Exception as e:
            last_err = e
            if attempt < max_attempts - 1:
                time.sleep(delay * (2 ** attempt))
    raise last_err

def _connect():
    global _sock
    if _sock is None:
        _sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        _sock.connect(os.environ["GENESIS_RPC_SOCKET"])
        _sock.settimeout(300)
    return _sock

def _call(tool_name, args):
    """Send a tool call to the parent process and return the parsed result."""
    conn = _connect()
    # Filter out None values
    clean_args = {k: v for k, v in args.items() if v is not None}
    request = json.dumps({"tool": tool_name, "args": clean_args}) + "\n"
    conn.sendall(request.encode())
    buf = b""
    while True:
        chunk = conn.recv(65536)
        if not chunk:
            raise RuntimeError("Agent process disconnected")
        buf += chunk
        if buf.endswith(b"\n"):
            break
    raw = buf.decode().strip()
    result = json.loads(raw)
    if isinstance(result, str):
        try:
            return json.loads(result)
        except (json.JSONDecodeError, TypeError):
            return result
    return result

def terminal(command: str, timeout: int = None, workdir: str = None):
    """Run a shell command. Returns dict with "output" and "exit_code"."""
    return _call("shell_exec", {"command": command, "timeout": str(timeout) if timeout else None, "workdir": workdir})

def read_file(path: str, offset: int = 1, limit: int = 500):
    """Read a file (1-indexed lines). Returns dict with "content" and "total_lines"."""
    return _call("read_file", {"path": path, "offset": str(offset), "limit": str(limit)})

def write_file(path: str, content: str):
    """Write content to a file."""
    return _call("write_file", {"path": path, "content": content})

def search_files(pattern: str, target: str = "content", path: str = ".", file_glob: str = None, limit: int = 50):
    """Search file contents or find files by name."""
    return _call("search_files", {"pattern": pattern, "target": target, "path": path, "file_glob": file_glob, "limit": str(limit)})

def patch(path: str, old_string: str, new_string: str, replace_all: bool = False):
    """Find-and-replace in a file."""
    return _call("patch", {"path": path, "old_string": old_string, "new_string": new_string, "replace_all": str(replace_all).lower()})

def web_search(query: str, limit: int = 5):
    """Search the web."""
    return _call("web_search", {"query": query, "limit": str(limit)})

def browse(url: str):
    """Extract content from a URL as markdown."""
    return _call("browse", {"url": url})
"#,
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal, secret-free environment for the child process.
fn build_sandbox_env(sock_path: &std::path::Path) -> Vec<(String, String)> {
    let safe_prefixes = [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "LC_",
        "TERM",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SHELL",
        "LOGNAME",
        "XDG_",
        "PYTHONPATH",
        "VIRTUAL_ENV",
        "CONDA",
    ];
    let secret_substrings = [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "PASSWD",
        "AUTH",
    ];

    let mut env: Vec<(String, String)> = Vec::new();
    for (k, v) in std::env::vars() {
        let k_upper = k.to_uppercase();
        if secret_substrings.iter().any(|s| k_upper.contains(s)) {
            continue;
        }
        if safe_prefixes.iter().any(|p| k.starts_with(p)) {
            env.push((k, v));
        }
    }
    env.push((
        "GENESIS_RPC_SOCKET".to_owned(),
        sock_path.to_string_lossy().to_string(),
    ));
    env.push(("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()));
    env.push(("PYTHONUNBUFFERED".to_owned(), "1".to_owned()));
    env
}

/// Drain a pipe into a byte buffer, capped at max_bytes.
fn drain_pipe(pipe: Option<impl std::io::Read>, chunks: Arc<Mutex<Vec<u8>>>, max_bytes: usize) {
    let Some(mut pipe) = pipe else { return };
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if total < max_bytes {
                    let keep = (max_bytes - total).min(n);
                    chunks.lock().unwrap().extend_from_slice(&buf[..keep]);
                }
                total += n;
            }
            Err(_) => break,
        }
    }
}

/// Build a PTC error output.
fn ptc_error(error: &str, tool_calls: usize, start: std::time::Instant) -> ToolOutput {
    ToolOutput {
        content: format!("PTC error: {error}"),
        metadata: BTreeMap::from([
            ("tool".to_owned(), "execute_code".to_owned()),
            ("status".to_owned(), "error".to_owned()),
            ("tool_calls_made".to_owned(), tool_calls.to_string()),
            (
                "duration_seconds".to_owned(),
                format!("{:.2}", start.elapsed().as_secs_f64()),
            ),
        ]),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx_destructive()
    }

    // --- Basic code execution tests (non-PTC) ---

    #[test]
    fn runs_simple_python() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([("code".to_owned(), "print(2 + 2)".to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content.trim(), "4");
        assert_eq!(output.metadata.get("exit_code").unwrap(), "0");
        assert_eq!(output.metadata.get("language").unwrap(), "python");
    }

    #[test]
    fn captures_python_stderr() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([(
                "code".to_owned(),
                "import sys; sys.stderr.write('oops\\n'); sys.exit(1)".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("oops"));
        assert!(output.content.contains("[stderr]"));
        assert_eq!(output.metadata.get("exit_code").unwrap(), "1");
    }

    #[test]
    fn requires_code_argument() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn env_is_sandboxed() {
        std::env::set_var("GENESIS_TEST_SECRET", "hunter2");
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([(
                "code".to_owned(),
                "import os; print(os.environ.get('GENESIS_TEST_SECRET', 'NOT_FOUND'))".to_owned(),
            )]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert_eq!(output.content.trim(), "NOT_FOUND");
        std::env::remove_var("GENESIS_TEST_SECRET");
    }

    #[test]
    fn timeout_kills_slow_code() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([
                ("code".to_owned(), "import time; time.sleep(60)".to_owned()),
                ("timeout_secs".to_owned(), "1".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(
                    reason.contains("timed out"),
                    "expected timeout, got: {reason}"
                );
            }
            _ => panic!("expected ExecutionFailed, got: {err:?}"),
        }
    }

    #[test]
    fn supports_node_language() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([
                ("code".to_owned(), "console.log(3 * 7)".to_owned()),
                ("language".to_owned(), "node".to_owned()),
            ]),
        };

        match tool.run(&call, &ctx()) {
            Ok(output) => {
                assert_eq!(output.metadata.get("language").unwrap(), "node");
            }
            Err(ToolError::ExecutionFailed { reason, .. }) => {
                assert!(
                    reason.contains("failed to spawn"),
                    "unexpected error: {reason}"
                );
            }
            Err(e) => panic!("unexpected error type: {e:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_language() {
        let tool = CodeExecutionTool;
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([
                ("code".to_owned(), "puts 'hello'".to_owned()),
                ("language".to_owned(), "haskell".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unsupported language"));
            }
            _ => panic!("expected ExecutionFailed, got: {err:?}"),
        }
    }

    #[test]
    fn multiline_python() {
        let tool = CodeExecutionTool;
        let code = "for i in range(3):\n    print(f'item {i}')";
        let call = ToolCall {
            name: "code_execution".to_owned(),
            arguments: BTreeMap::from([("code".to_owned(), code.to_owned())]),
        };

        let output = tool.run(&call, &ctx()).expect("should succeed");
        assert!(output.content.contains("item 0"));
        assert!(output.content.contains("item 1"));
        assert!(output.content.contains("item 2"));
    }

    // --- PTC stub generation tests ---

    #[test]
    fn generates_genesis_tools_module() {
        let src = generate_genesis_tools_module();
        assert!(src.contains("def terminal("));
        assert!(src.contains("def read_file("));
        assert!(src.contains("def write_file("));
        assert!(src.contains("def search_files("));
        assert!(src.contains("def patch("));
        assert!(src.contains("def web_search("));
        assert!(src.contains("def browse("));
        assert!(src.contains("GENESIS_RPC_SOCKET"));
        assert!(src.contains("def _call("));
        assert!(src.contains("def _connect("));
        assert!(src.contains("def json_parse("));
        assert!(src.contains("def shell_quote("));
        assert!(src.contains("def retry("));
    }

    // --- PTC execution tests ---

    #[test]
    fn ptc_basic_python_no_tools() {
        let registry = crate::default_registry();
        let context = ctx();
        let code = "print(sum(range(10)))";
        let result = execute_code_ptc(code, &registry, &context);
        assert!(
            result.content.trim().contains("45"),
            "expected 45, got: {}",
            result.content
        );
        assert_eq!(result.metadata.get("tool_calls_made").unwrap(), "0");
    }

    #[test]
    fn ptc_handles_script_errors() {
        let registry = crate::default_registry();
        let context = ctx();
        let code = "raise ValueError('boom')";
        let result = execute_code_ptc(code, &registry, &context);
        assert_eq!(result.metadata.get("status").unwrap(), "error");
        assert!(
            result.content.contains("boom") || result.content.contains("ValueError"),
            "expected error in output: {}",
            result.content
        );
    }

    #[test]
    fn ptc_env_is_sandboxed() {
        std::env::set_var("MY_SECRET_TOKEN", "hunter2");
        let registry = crate::default_registry();
        let context = ctx();
        let code = r#"
import os
token = os.environ.get('MY_SECRET_TOKEN', 'NOT_FOUND')
print(f"token={token}")
"#;
        let result = execute_code_ptc(code, &registry, &context);
        assert!(
            result.content.contains("token=NOT_FOUND"),
            "secret should not leak: {}",
            result.content
        );
        std::env::remove_var("MY_SECRET_TOKEN");
    }

    #[test]
    fn ptc_rpc_calls_read_file() {
        let registry = crate::default_registry();
        let context = ctx();

        // Create a temp file to read
        let dir = tempfile::tempdir().unwrap();
        let test_file = dir.path().join("hello.txt");
        std::fs::write(&test_file, "Hello from PTC!").unwrap();

        let code = format!(
            r#"
from genesis_tools import read_file
import json
result = read_file("{}")
if isinstance(result, dict):
    print(result.get("content", result))
elif isinstance(result, str):
    print(result)
else:
    print(str(result))
"#,
            test_file.display()
        );

        let result = execute_code_ptc(&code, &registry, &context);
        assert!(
            result.content.contains("Hello from PTC!"),
            "expected file content, got: {}",
            result.content
        );
        let calls: usize = result
            .metadata
            .get("tool_calls_made")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(calls, 1, "expected 1 tool call");
    }

    #[test]
    fn ptc_rpc_rejects_disallowed_tools() {
        let registry = crate::default_registry();
        let context = ctx();

        let code = r#"
from genesis_tools import _call
result = _call("delete_file", {"path": "/etc/passwd"})
print(result)
"#;

        let result = execute_code_ptc(code, &registry, &context);
        assert!(
            result.content.contains("not available") || result.content.contains("error"),
            "should reject disallowed tool: {}",
            result.content
        );
    }

    #[test]
    fn ptc_rpc_wire_protocol() {
        use std::io::{BufRead, Write};
        use std::os::unix::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");

        let registry = crate::default_registry();
        let context = ctx();
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        let server = std::thread::spawn(move || {
            rpc_server_loop(listener, &registry, &context, call_count_clone, 50);
        });

        let stream = UnixStream::connect(&sock_path).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let mut reader = std::io::BufReader::new(&stream);
        let mut writer = &stream;

        // Send request for disallowed tool
        writeln!(writer, r#"{{"tool": "nonexistent", "args": {{}}}}"#).unwrap();
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert!(
            response.contains("not available"),
            "expected error for unknown tool, got: {response}"
        );

        drop(stream);
        server.join().unwrap();
    }
}
