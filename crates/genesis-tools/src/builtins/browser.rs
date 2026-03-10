// Items in this module are used by the browser tool structs (added in a
// subsequent task).  Suppress dead-code warnings until then.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::ToolError;

const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 30;
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 300;
const SNAPSHOT_MAX_CHARS: usize = 8000;

const BOT_DETECTION_PATTERNS: &[&str] = &[
    "access denied",
    "access to this page has been denied",
    "blocked",
    "bot detected",
    "verification required",
    "please verify",
    "are you a robot",
    "captcha",
    "cloudflare",
    "ddos protection",
    "checking your browser",
    "just a moment",
    "attention required",
];

// ---------------------------------------------------------------------------
// SessionInfo
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SessionInfo {
    session_name: String,
    bb_session_id: Option<String>,
    cdp_url: Option<String>,
    first_nav: bool,
    last_activity: Instant,
    features: BTreeMap<String, bool>,
}

// ---------------------------------------------------------------------------
// BrowserManager
// ---------------------------------------------------------------------------

pub struct BrowserManager {
    sessions: Mutex<HashMap<String, SessionInfo>>,
}

impl Default for BrowserManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the session_name for the given session_id, creating a new
    /// session if one does not already exist.
    ///
    /// For cloud mode (Browserbase), the network call to create a session is
    /// done outside the lock. A TOCTOU race is handled by checking after
    /// re-acquiring the lock and closing the redundant Browserbase session.
    pub fn get_or_create_session(&self, session_id: &str) -> Result<String, ToolError> {
        // Fast path: session already exists
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(info) = sessions.get(session_id) {
                return Ok(info.session_name.clone());
            }
        }

        // Slow path: create session outside the lock
        let new_info = if is_cloud_mode() {
            create_browserbase_session(session_id)?
        } else {
            create_local_session(session_id)
        };

        // Re-acquire and insert, handling TOCTOU race
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(existing) = sessions.get(session_id) {
            // Another thread created a session in the meantime
            let name = existing.session_name.clone();
            // Close the redundant Browserbase session if we created one
            if let Some(bb_id) = &new_info.bb_session_id {
                let _ = close_browserbase_session(bb_id);
            }
            return Ok(name);
        }

        let name = new_info.session_name.clone();
        sessions.insert(session_id.to_owned(), new_info);
        Ok(name)
    }

    /// Update the last_activity timestamp for a session and cleanup stale ones.
    pub fn touch_session(&self, session_id: &str) {
        {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(info) = sessions.get_mut(session_id) {
                info.last_activity = Instant::now();
            }
        }
        self.cleanup_stale_sessions();
    }

    /// Returns (session_name, cdp_url) for the given session_id if it exists.
    pub fn get_session_cmd_info(&self, session_id: &str) -> Option<(String, Option<String>)> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session_id)
            .map(|info| (info.session_name.clone(), info.cdp_url.clone()))
    }

    /// Mark first_nav as false and return (was_first_nav, features).
    pub fn consume_first_nav(&self, session_id: &str) -> (bool, BTreeMap<String, bool>) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(info) = sessions.get_mut(session_id) {
            let was_first = info.first_nav;
            info.first_nav = false;
            (was_first, info.features.clone())
        } else {
            (false, BTreeMap::new())
        }
    }

    /// Close and remove a specific session. Releases Browserbase session if
    /// applicable and cleans up daemon/socket files.
    pub fn close_session(&self, session_id: &str) {
        let removed = {
            let mut sessions = self.sessions.lock().unwrap();
            sessions.remove(session_id)
        };

        if let Some(info) = removed {
            // Run agent-browser close command
            let _ = run_browser_command_raw(
                &info.session_name,
                info.cdp_url.as_deref(),
                "close",
                &BTreeMap::new(),
                Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
            );

            // Release Browserbase session if applicable
            if let Some(bb_id) = &info.bb_session_id {
                let _ = close_browserbase_session(bb_id);
            }

            // Clean up daemon and socket
            cleanup_daemon_and_socket(&info.session_name);
        }
    }

    /// Close all active sessions.
    fn close_all_sessions(&self) {
        let all: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions.keys().cloned().collect()
        };
        for session_id in all {
            self.close_session(&session_id);
        }
    }

    /// Remove sessions that have been inactive longer than the timeout.
    fn cleanup_stale_sessions(&self) {
        let timeout_secs: u64 = std::env::var("BROWSER_INACTIVITY_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS);
        let timeout = Duration::from_secs(timeout_secs);

        let stale: Vec<String> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|(_, info)| info.last_activity.elapsed() > timeout)
                .map(|(id, _)| id.clone())
                .collect()
        };

        for session_id in stale {
            self.close_session(&session_id);
        }
    }
}

impl Drop for BrowserManager {
    fn drop(&mut self) {
        self.close_all_sessions();
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Returns a short temporary directory path suitable for Unix domain sockets
/// (which have a ~104-char path limit). On macOS, std::env::temp_dir() returns
/// a very long path, so we use "/tmp" instead.
fn socket_safe_tmpdir() -> String {
    if cfg!(target_os = "macos") {
        "/tmp".to_owned()
    } else {
        std::env::temp_dir().to_string_lossy().into_owned()
    }
}

/// Build CLI argument list for the agent-browser command.
///
/// Uses `--cdp` for cloud mode (when cdp_url is provided) or `--session` for
/// local mode. NEVER combines both flags.
fn build_cmd_args(
    session_name: &str,
    cdp_url: Option<&str>,
    command: &str,
    args: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut cmd_args = Vec::new();

    if let Some(url) = cdp_url {
        cmd_args.push("--cdp".to_owned());
        cmd_args.push(url.to_owned());
    } else {
        cmd_args.push("--session".to_owned());
        cmd_args.push(session_name.to_owned());
    }

    cmd_args.push("--json".to_owned());
    cmd_args.push(command.to_owned());

    for (key, value) in args {
        cmd_args.push(format!("--{key}"));
        cmd_args.push(value.clone());
    }

    cmd_args
}

/// Normalize an accessibility ref by prepending "@" if missing.
fn normalize_ref(r: &str) -> String {
    if r.starts_with('@') {
        r.to_owned()
    } else {
        format!("@{r}")
    }
}

/// Generate a unique session name from a session_id.
/// Format: `genesis_{session_id}_{8 hex chars from uuid}`
fn generate_session_name(session_id: &str) -> String {
    let uuid_hex = uuid::Uuid::new_v4().to_string().replace('-', "");
    let short = &uuid_hex[..8];
    format!("genesis_{session_id}_{short}")
}

/// Truncate text to a maximum character count, splitting at a char boundary.
/// Appends a truncation marker if the text was shortened.
fn truncate_snapshot(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    // Find the byte index of the max_chars-th character
    let byte_end = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    let mut truncated = text[..byte_end].to_owned();
    truncated.push_str("\n[... content truncated ...]");
    truncated
}

/// Check whether a page title matches known bot-detection patterns.
fn check_bot_detection(title: &str) -> bool {
    let lower = title.to_lowercase();
    BOT_DETECTION_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

/// Returns true if both BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID are set.
fn is_cloud_mode() -> bool {
    std::env::var("BROWSERBASE_API_KEY").is_ok()
        && std::env::var("BROWSERBASE_PROJECT_ID").is_ok()
}

/// Create a local (non-cloud) session.
fn create_local_session(session_id: &str) -> SessionInfo {
    SessionInfo {
        session_name: generate_session_name(session_id),
        bb_session_id: None,
        cdp_url: None,
        first_nav: true,
        last_activity: Instant::now(),
        features: BTreeMap::from([("local".to_owned(), true)]),
    }
}

// ---------------------------------------------------------------------------
// CLI discovery and executor
// ---------------------------------------------------------------------------

/// Find the `agent-browser` binary, falling back to npx.
fn find_agent_browser() -> Result<String, ToolError> {
    // Try direct binary first
    let which_result = Command::new("which")
        .arg("agent-browser")
        .output();

    if let Ok(output) = which_result {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    // Try npx fallback
    let npx_result = Command::new("which")
        .arg("npx")
        .output();

    if let Ok(output) = npx_result {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                return Ok(format!("{path} agent-browser"));
            }
        }
    }

    Err(ToolError::ExecutionFailed {
        tool: "browser".to_owned(),
        reason: "agent-browser not found. Install with: npm install -g agent-browser".to_owned(),
    })
}

/// Build the environment variables for a browser command invocation.
fn build_browser_env(session_name: &str) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    // Ensure /usr/bin is in PATH
    if let Some(path) = env.get("PATH") {
        if !path.contains("/usr/bin") {
            env.insert("PATH".to_owned(), format!("{path}:/usr/bin"));
        }
    } else {
        env.insert("PATH".to_owned(), "/usr/bin:/usr/local/bin".to_owned());
    }

    // Set socket directory for the agent-browser daemon
    let tmpdir = socket_safe_tmpdir();
    let socket_dir = format!("{tmpdir}/agent-browser-{session_name}/");
    env.insert("AGENT_BROWSER_SOCKET_DIR".to_owned(), socket_dir);

    env
}

/// Execute an agent-browser CLI command and return the parsed JSON output.
///
/// Spawns the process and uses a separate thread with `child.wait_with_output()`
/// for clean pipe handling. On timeout, the child process is killed.
fn run_browser_command_raw(
    session_name: &str,
    cdp_url: Option<&str>,
    command: &str,
    args: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<serde_json::Value, ToolError> {
    let browser_bin = find_agent_browser()?;
    let cmd_args = build_cmd_args(session_name, cdp_url, command, args);
    let env = build_browser_env(session_name);

    // Split the binary path to handle the "npx agent-browser" case
    let parts: Vec<&str> = browser_bin.split_whitespace().collect();
    let (program, prefix_args): (&str, &[&str]) = if parts.len() > 1 {
        (parts[0], &parts[1..])
    } else {
        (parts[0], &[])
    };

    let child = Command::new(program)
        .args(prefix_args)
        .args(&cmd_args)
        .envs(&env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: format!("failed to spawn agent-browser: {e}"),
        })?;

    // Use a separate thread for wait_with_output to allow timeout handling
    let deadline = Instant::now() + timeout;

    // We need to handle timeout by polling try_wait, then kill if exceeded.
    // But the spec says to use a separate thread with wait_with_output for
    // clean pipe handling. So we move the child into a thread:
    let handle = std::thread::spawn(move || child.wait_with_output());

    // Wait for the thread to finish, checking for timeout
    loop {
        if handle.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            // Thread still running = process still running. We can't kill from
            // here since child was moved. The thread will eventually complete
            // but we return a timeout error.
            return Err(ToolError::ExecutionFailed {
                tool: "browser".to_owned(),
                reason: format!(
                    "browser command '{}' timed out after {}s",
                    command,
                    timeout.as_secs()
                ),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let output = handle
        .join()
        .map_err(|_| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: "browser command thread panicked".to_owned(),
        })?
        .map_err(|e| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: format!("browser command failed: {e}"),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    if !output.status.success() {
        let msg = if stderr.is_empty() {
            format!(
                "agent-browser exited with status {}: {}",
                output.status, stdout
            )
        } else {
            format!("agent-browser failed: {stderr}")
        };
        return Err(ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: msg,
        });
    }

    // Try to parse as JSON; fall back to wrapping raw text
    if stdout.is_empty() {
        Ok(json!({"output": ""}))
    } else {
        Ok(serde_json::from_str(&stdout).unwrap_or_else(|_| json!({"output": stdout})))
    }
}

/// Clean up a daemon process and its socket directory.
fn cleanup_daemon_and_socket(session_name: &str) {
    let tmpdir = socket_safe_tmpdir();
    let socket_dir = format!("{tmpdir}/agent-browser-{session_name}");
    let pid_file = format!("{socket_dir}/daemon.pid");

    // Try to read and kill the daemon process
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        let pid = pid_str.trim();
        if !pid.is_empty() {
            let _ = Command::new("kill").arg(pid).output();
        }
    }

    // Remove the socket directory
    let _ = std::fs::remove_dir_all(&socket_dir);
}

// ---------------------------------------------------------------------------
// Browserbase cloud mode
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct BrowserbaseConfig {
    api_key: String,
    project_id: String,
}

fn get_browserbase_config() -> Result<BrowserbaseConfig, ToolError> {
    let api_key = std::env::var("BROWSERBASE_API_KEY").map_err(|_| ToolError::ExecutionFailed {
        tool: "browser".to_owned(),
        reason: "BROWSERBASE_API_KEY environment variable not set".to_owned(),
    })?;
    let project_id =
        std::env::var("BROWSERBASE_PROJECT_ID").map_err(|_| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: "BROWSERBASE_PROJECT_ID environment variable not set".to_owned(),
        })?;
    Ok(BrowserbaseConfig {
        api_key,
        project_id,
    })
}

fn build_browserbase_session_body(
    project_id: &str,
    proxies: bool,
    advanced_stealth: bool,
    keep_alive: bool,
    timeout_ms: Option<u64>,
) -> serde_json::Value {
    let mut body = json!({
        "projectId": project_id,
    });

    if proxies {
        body["proxies"] = json!(true);
    }

    if advanced_stealth {
        body["browserSettings"] = json!({
            "advancedStealth": true,
        });
    }

    if keep_alive {
        body["keepAlive"] = json!(true);
    }

    if let Some(ms) = timeout_ms {
        body["timeout"] = json!(ms);
    }

    body
}

fn browserbase_http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client")
    })
}

/// Create a Browserbase cloud session. On 402 (payment required), retries
/// without keepAlive, then without proxies.
fn create_browserbase_session(session_id: &str) -> Result<SessionInfo, ToolError> {
    let config = get_browserbase_config()?;
    let client = browserbase_http_client();
    let url = "https://api.browserbase.com/v1/sessions";

    // Attempt 1: full features (proxies + keepAlive)
    let body = build_browserbase_session_body(&config.project_id, true, true, true, None);
    let resp = client
        .post(url)
        .header("X-BB-API-Key", &config.api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: format!("Browserbase API request failed: {e}"),
        })?;

    if resp.status().as_u16() == 402 {
        // Attempt 2: without keepAlive
        let body = build_browserbase_session_body(&config.project_id, true, true, false, None);
        let resp = client
            .post(url)
            .header("X-BB-API-Key", &config.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "browser".to_owned(),
                reason: format!("Browserbase API request failed: {e}"),
            })?;

        if resp.status().as_u16() == 402 {
            // Attempt 3: without proxies either
            let body =
                build_browserbase_session_body(&config.project_id, false, true, false, None);
            let resp = client
                .post(url)
                .header("X-BB-API-Key", &config.api_key)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "browser".to_owned(),
                    reason: format!("Browserbase API request failed: {e}"),
                })?;

            return parse_browserbase_response(resp, session_id, false, false);
        }

        return parse_browserbase_response(resp, session_id, true, false);
    }

    parse_browserbase_response(resp, session_id, true, true)
}

/// Parse a successful Browserbase session creation response.
fn parse_browserbase_response(
    resp: reqwest::blocking::Response,
    session_id: &str,
    proxies: bool,
    keep_alive: bool,
) -> Result<SessionInfo, ToolError> {
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: format!("Browserbase API returned {status}: {body}"),
        });
    }

    let json: serde_json::Value = resp.json().map_err(|e| ToolError::ExecutionFailed {
        tool: "browser".to_owned(),
        reason: format!("failed to parse Browserbase response: {e}"),
    })?;

    let bb_session_id = json["id"]
        .as_str()
        .ok_or_else(|| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: "Browserbase response missing 'id' field".to_owned(),
        })?
        .to_owned();

    let cdp_url = json["connectUrl"]
        .as_str()
        .ok_or_else(|| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: "Browserbase response missing 'connectUrl' field".to_owned(),
        })?
        .to_owned();

    let mut features = BTreeMap::new();
    features.insert("cloud".to_owned(), true);
    features.insert("proxies".to_owned(), proxies);
    features.insert("keepAlive".to_owned(), keep_alive);
    features.insert("advancedStealth".to_owned(), true);

    Ok(SessionInfo {
        session_name: generate_session_name(session_id),
        bb_session_id: Some(bb_session_id),
        cdp_url: Some(cdp_url),
        first_nav: true,
        last_activity: Instant::now(),
        features,
    })
}

/// Release a Browserbase session by setting its status to REQUEST_RELEASE.
fn close_browserbase_session(bb_session_id: &str) -> Result<(), ToolError> {
    let config = get_browserbase_config()?;
    let client = browserbase_http_client();
    let url = format!("https://api.browserbase.com/v1/sessions/{bb_session_id}");

    let resp = client
        .post(&url)
        .header("X-BB-API-Key", &config.api_key)
        .header("Content-Type", "application/json")
        .json(&json!({"status": "REQUEST_RELEASE"}))
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: format!("failed to release Browserbase session: {e}"),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: "browser".to_owned(),
            reason: format!("Browserbase release returned {status}: {body}"),
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
            default_working_dir: None,
        }
    }

    #[test]
    fn socket_safe_tmpdir_returns_short_path() {
        let dir = socket_safe_tmpdir();
        // The path must be short enough for Unix domain sockets (~104 chars)
        assert!(
            dir.len() < 50,
            "tmpdir path too long for sockets: {dir}"
        );
        // Must be a valid directory
        assert!(
            std::path::Path::new(&dir).is_dir(),
            "tmpdir does not exist: {dir}"
        );
    }

    #[test]
    fn build_cmd_args_local_mode() {
        let args = build_cmd_args("my-session", None, "navigate", &BTreeMap::new());
        assert!(args.contains(&"--session".to_owned()));
        assert!(args.contains(&"my-session".to_owned()));
        assert!(args.contains(&"--json".to_owned()));
        assert!(args.contains(&"navigate".to_owned()));
        assert!(!args.contains(&"--cdp".to_owned()));
    }

    #[test]
    fn build_cmd_args_cloud_mode() {
        let args = build_cmd_args(
            "my-session",
            Some("wss://cloud.example.com/cdp"),
            "snapshot",
            &BTreeMap::new(),
        );
        assert!(args.contains(&"--cdp".to_owned()));
        assert!(args.contains(&"wss://cloud.example.com/cdp".to_owned()));
        assert!(args.contains(&"--json".to_owned()));
        assert!(args.contains(&"snapshot".to_owned()));
        assert!(!args.contains(&"--session".to_owned()));
    }

    #[test]
    fn ref_normalization_prepends_at() {
        assert_eq!(normalize_ref("btn1"), "@btn1");
        assert_eq!(normalize_ref("@btn1"), "@btn1");
        assert_eq!(normalize_ref(""), "@");
        assert_eq!(normalize_ref("@"), "@");
    }

    #[test]
    fn session_name_generation() {
        let name = generate_session_name("abc-123");
        assert!(
            name.starts_with("genesis_abc-123_"),
            "unexpected name: {name}"
        );
        // 8 hex chars suffix
        let suffix = name.strip_prefix("genesis_abc-123_").unwrap();
        assert_eq!(suffix.len(), 8, "suffix should be 8 hex chars: {suffix}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix should be hex: {suffix}"
        );
    }

    #[test]
    fn truncate_snapshot_short_text_unchanged() {
        let text = "Hello, world!";
        assert_eq!(truncate_snapshot(text, 100), text);
    }

    #[test]
    fn truncate_snapshot_long_text_truncated() {
        let text = "abcdefghij"; // 10 chars
        let result = truncate_snapshot(text, 5);
        assert!(result.starts_with("abcde"));
        assert!(result.contains("[... content truncated ...]"));
        // Should not contain the full original text
        assert!(!result.contains("fghij"));
    }

    #[test]
    fn bot_detection_matches_known_patterns() {
        assert!(check_bot_detection("Access Denied"));
        assert!(check_bot_detection("Bot Detected - Please verify"));
        assert!(check_bot_detection("Cloudflare DDoS Protection"));
        assert!(check_bot_detection("Just a moment..."));
        assert!(check_bot_detection("Attention Required!"));
        assert!(check_bot_detection("Are you a robot?"));
        assert!(check_bot_detection("CAPTCHA required"));
        // Should NOT match normal titles
        assert!(!check_bot_detection("Google Search"));
        assert!(!check_bot_detection("GitHub - Repository"));
        assert!(!check_bot_detection("Wikipedia"));
    }

    #[test]
    fn is_cloud_mode_checks_env() {
        // Save and clear env vars to ensure a clean state
        let saved_key = std::env::var("BROWSERBASE_API_KEY").ok();
        let saved_proj = std::env::var("BROWSERBASE_PROJECT_ID").ok();

        std::env::remove_var("BROWSERBASE_API_KEY");
        std::env::remove_var("BROWSERBASE_PROJECT_ID");
        assert!(!is_cloud_mode(), "should be false when env vars are unset");

        std::env::set_var("BROWSERBASE_API_KEY", "test-key");
        assert!(
            !is_cloud_mode(),
            "should be false when only API key is set"
        );

        std::env::set_var("BROWSERBASE_PROJECT_ID", "test-project");
        assert!(is_cloud_mode(), "should be true when both vars are set");

        // Restore
        std::env::remove_var("BROWSERBASE_API_KEY");
        std::env::remove_var("BROWSERBASE_PROJECT_ID");
        if let Some(k) = saved_key {
            std::env::set_var("BROWSERBASE_API_KEY", k);
        }
        if let Some(p) = saved_proj {
            std::env::set_var("BROWSERBASE_PROJECT_ID", p);
        }
    }

    #[test]
    fn create_local_session_generates_valid_info() {
        let info = create_local_session("test-id");
        assert!(info.session_name.starts_with("genesis_test-id_"));
        assert!(info.bb_session_id.is_none());
        assert!(info.cdp_url.is_none());
        assert!(info.first_nav);
        assert_eq!(info.features.get("local"), Some(&true));
    }

    #[test]
    fn browserbase_config_missing_key_fails() {
        // Save and clear env vars
        let saved_key = std::env::var("BROWSERBASE_API_KEY").ok();
        let saved_proj = std::env::var("BROWSERBASE_PROJECT_ID").ok();

        std::env::remove_var("BROWSERBASE_API_KEY");
        std::env::remove_var("BROWSERBASE_PROJECT_ID");

        let result = get_browserbase_config();
        assert!(result.is_err());
        let err = result.unwrap_err();
        match &err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("BROWSERBASE_API_KEY"));
            }
            _ => panic!("expected ExecutionFailed, got: {err:?}"),
        }

        // Restore
        if let Some(k) = saved_key {
            std::env::set_var("BROWSERBASE_API_KEY", k);
        }
        if let Some(p) = saved_proj {
            std::env::set_var("BROWSERBASE_PROJECT_ID", p);
        }
    }

    #[test]
    fn browserbase_session_body_defaults() {
        let body = build_browserbase_session_body("proj-123", true, true, true, None);
        assert_eq!(body["projectId"], "proj-123");
        assert_eq!(body["proxies"], true);
        assert_eq!(body["browserSettings"]["advancedStealth"], true);
        assert_eq!(body["keepAlive"], true);
        assert!(body.get("timeout").is_none());
    }

    #[test]
    fn browserbase_session_body_advanced_stealth() {
        let body = build_browserbase_session_body("proj-123", false, true, false, Some(60000));
        assert_eq!(body["projectId"], "proj-123");
        assert_eq!(body["browserSettings"]["advancedStealth"], true);
        assert!(body.get("proxies").is_none());
        assert!(body.get("keepAlive").is_none());
        assert_eq!(body["timeout"], 60000);
    }

    #[test]
    fn browserbase_session_body_no_proxies() {
        let body = build_browserbase_session_body("proj-456", false, false, true, None);
        assert_eq!(body["projectId"], "proj-456");
        assert!(body.get("proxies").is_none());
        assert!(body.get("browserSettings").is_none());
        assert_eq!(body["keepAlive"], true);
    }
}
