use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolCall,
    PostToolCall,
    PreTurn,
    PostTurn,
    OnError,
    OnComplete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookConfig {
    pub event: HookEvent,
    pub command: String,
    pub timeout_ms: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookResult {
    pub event: HookEvent,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct HookRunner {
    hooks: Vec<HookConfig>,
}

impl HookRunner {
    pub fn new(hooks: Vec<HookConfig>) -> Self {
        Self { hooks }
    }

    pub fn run_hooks(
        &self,
        event: HookEvent,
        context: &serde_json::Value,
    ) -> Vec<HookResult> {
        self.hooks
            .iter()
            .filter(|hook| hook.enabled && hook.event == event)
            .map(|hook| run_hook(hook, context))
            .collect()
    }

    pub fn hooks(&self) -> &[HookConfig] {
        &self.hooks
    }
}

pub fn load_hooks(config: Vec<HookConfig>) -> HookRunner {
    HookRunner::new(config)
}

fn run_hook(config: &HookConfig, context: &serde_json::Value) -> HookResult {
    let started = Instant::now();
    let mut command = shell_command(&config.command);
    command
        .env("GENESIS_HOOK_CONTEXT", context.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let spawn_result = command.spawn();
    let result = match spawn_result {
        Ok(mut child) => {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let stdout_handle = thread::spawn(move || read_stream(stdout));
            let stderr_handle = thread::spawn(move || read_stream(stderr));

            let timeout = Duration::from_millis(config.timeout_ms.max(1));
            let exit_code = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.code().unwrap_or(-1),
                    Ok(None) if started.elapsed() >= timeout => {
                        let _ = child.kill();
                        let status = child.wait().ok();
                        break status.and_then(|s| s.code()).unwrap_or(-1);
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(5)),
                    Err(_) => {
                        let _ = child.kill();
                        break -1;
                    }
                }
            };

            let stdout = stdout_handle.join().unwrap_or_default();
            let stderr = stderr_handle.join().unwrap_or_default();
            HookResult {
                event: config.event.clone(),
                command: config.command.clone(),
                exit_code,
                stdout,
                stderr,
                duration_ms: started.elapsed().as_millis() as u64,
            }
        }
        Err(err) => HookResult {
            event: config.event.clone(),
            command: config.command.clone(),
            exit_code: -1,
            stdout: String::new(),
            stderr: err.to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
        },
    };

    result
}

fn read_stream(stream: Option<impl Read>) -> String {
    match stream {
        Some(mut stream) => {
            let mut buf = String::new();
            let _ = stream.read_to_string(&mut buf);
            buf
        }
        None => String::new(),
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_hooks_builds_runner() {
        let hooks = vec![HookConfig {
            event: HookEvent::PreTurn,
            command: "true".to_owned(),
            timeout_ms: 100,
            enabled: true,
        }];

        let runner = load_hooks(hooks.clone());
        assert_eq!(runner.hooks(), hooks.as_slice());
    }

    #[test]
    fn run_hooks_executes_matching_enabled_hooks() {
        let runner = HookRunner::new(vec![
            HookConfig {
                event: HookEvent::PreTurn,
                command: "echo pre-turn".to_owned(),
                timeout_ms: 1000,
                enabled: true,
            },
            HookConfig {
                event: HookEvent::PostTurn,
                command: "echo post-turn".to_owned(),
                timeout_ms: 1000,
                enabled: true,
            },
            HookConfig {
                event: HookEvent::PreTurn,
                command: "echo disabled".to_owned(),
                timeout_ms: 1000,
                enabled: false,
            },
        ]);

        let results = runner.run_hooks(HookEvent::PreTurn, &serde_json::json!({"turn": 1}));

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event, HookEvent::PreTurn);
        assert_eq!(results[0].exit_code, 0);
        assert!(results[0].stdout.contains("pre-turn"));
    }

    #[test]
    fn run_hooks_passes_context_via_env_var() {
        let runner = HookRunner::new(vec![HookConfig {
            event: HookEvent::OnComplete,
            command: "printf '%s' \"$GENESIS_HOOK_CONTEXT\"".to_owned(),
            timeout_ms: 1000,
            enabled: true,
        }]);

        let results = runner.run_hooks(
            HookEvent::OnComplete,
            &serde_json::json!({"session_id": "s-1", "ok": true}),
        );

        assert_eq!(results.len(), 1);
        assert!(results[0].stdout.contains("\"session_id\":\"s-1\""));
        assert!(results[0].stdout.contains("\"ok\":true"));
    }

    #[test]
    fn run_hooks_captures_non_zero_exit_code() {
        let runner = HookRunner::new(vec![HookConfig {
            event: HookEvent::OnError,
            command: "false".to_owned(),
            timeout_ms: 1000,
            enabled: true,
        }]);

        let results = runner.run_hooks(HookEvent::OnError, &serde_json::json!({"error": "boom"}));

        assert_eq!(results.len(), 1);
        assert_ne!(results[0].exit_code, 0);
    }

    #[test]
    fn run_hooks_timeout_kills_long_running_process() {
        let runner = HookRunner::new(vec![HookConfig {
            event: HookEvent::PreToolCall,
            command: "sleep 1".to_owned(),
            timeout_ms: 10,
            enabled: true,
        }]);

        let results = runner.run_hooks(
            HookEvent::PreToolCall,
            &serde_json::json!({"tool": "shell_exec"}),
        );

        assert_eq!(results.len(), 1);
        assert_ne!(results[0].exit_code, 0);
        assert!(results[0].duration_ms < 1000);
    }

    #[test]
    fn run_hooks_handles_spawn_errors() {
        let runner = HookRunner::new(vec![HookConfig {
            event: HookEvent::PostToolCall,
            command: "nonexistent-command-for-hooks".to_owned(),
            timeout_ms: 1000,
            enabled: true,
        }]);

        let results = runner.run_hooks(
            HookEvent::PostToolCall,
            &serde_json::json!({"tool": "read_file"}),
        );

        assert_eq!(results.len(), 1);
        assert_ne!(results[0].exit_code, 0);
    }

    #[test]
    fn hook_result_serializes_cleanly() {
        let result = HookResult {
            event: HookEvent::PostTurn,
            command: "echo hi".to_owned(),
            exit_code: 0,
            stdout: "hi\n".to_owned(),
            stderr: String::new(),
            duration_ms: 12,
        };

        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"post_turn\""));
        assert!(json.contains("\"echo hi\""));
    }
}
