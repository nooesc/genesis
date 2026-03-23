use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

use super::{ExecResult, SandboxBackend, SandboxConfig, SandboxError, SandboxInstance};

// ---------------------------------------------------------------------------
// Binary detection
// ---------------------------------------------------------------------------

fn detect_binary() -> Result<String, SandboxError> {
    for name in &["apptainer", "singularity"] {
        match std::process::Command::new("which").arg(name).output() {
            Ok(out) if out.status.success() => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(name.to_string());
                }
            }
            Ok(_) => {} // command ran but binary not found — expected
            Err(e) => {
                tracing::debug!(binary = %name, error = %e, "failed to check for sandbox binary");
            }
        }
    }
    Err(SandboxError::BinaryNotFound {
        tried: vec!["apptainer", "singularity"],
    })
}

// ---------------------------------------------------------------------------
// SIF cache key
// ---------------------------------------------------------------------------

/// Convert an image URL to a safe filename for SIF caching.
/// Example: `docker://nikolaik/python-nodejs:python3.11-nodejs20`
///       -> `nikolaik-python-nodejs-python3.11-nodejs20.sif`
pub fn sif_cache_key(image_url: &str) -> String {
    let stripped = image_url.strip_prefix("docker://").unwrap_or(image_url);
    let sanitized = stripped.replace(['/', ':'], "-");
    format!("{sanitized}.sif")
}

// ---------------------------------------------------------------------------
// Scratch directory resolution
// ---------------------------------------------------------------------------

fn resolve_scratch_dir() -> PathBuf {
    // 1. TERMINAL_SCRATCH_DIR env var
    if let Some(val) = genesis_config::env::get_opt(genesis_config::env::TERMINAL_SCRATCH_DIR) {
        return PathBuf::from(val);
    }

    // 2. TERMINAL_SANDBOX_DIR + /singularity
    if let Some(val) = genesis_config::env::get_opt(genesis_config::env::TERMINAL_SANDBOX_DIR) {
        return PathBuf::from(val).join("singularity");
    }

    // 3. /scratch/{USER}/genesis — check exists + writable
    if let Ok(user) = genesis_config::env::get(genesis_config::env::USER) {
        let candidate = PathBuf::from(format!("/scratch/{user}/genesis"));
        if std::fs::metadata(&candidate)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
        {
            return candidate;
        }
    }

    // 4. ~/.genesis/sandboxes/singularity
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".genesis")
        .join("sandboxes")
        .join("singularity")
}

// ---------------------------------------------------------------------------
// SingularitySandbox struct
// ---------------------------------------------------------------------------

pub struct SingularitySandbox {
    binary: String,
    scratch_dir: PathBuf,
    sif_build_lock: tokio::sync::Mutex<()>,
}

impl SingularitySandbox {
    pub fn new() -> Result<Self, SandboxError> {
        let binary = detect_binary()?;
        let scratch_dir = resolve_scratch_dir();
        Ok(Self {
            binary,
            scratch_dir,
            sif_build_lock: tokio::sync::Mutex::new(()),
        })
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Build the argument list for `apptainer instance start`.
pub fn build_start_args(
    image: &str,
    instance_id: &str,
    persistent: bool,
    overlay_dir: Option<String>,
    cpu: f32,
    memory_mb: u32,
) -> Vec<String> {
    let mut args = vec![
        "instance".to_string(),
        "start".to_string(),
        "--containall".to_string(),
        "--no-home".to_string(),
    ];

    if persistent {
        if let Some(dir) = overlay_dir {
            args.push("--overlay".to_string());
            args.push(dir);
        }
    } else {
        args.push("--writable-tmpfs".to_string());
    }

    if memory_mb > 0 {
        args.push("--memory".to_string());
        args.push(format!("{memory_mb}M"));
    }

    if cpu > 0.0 {
        // Format with no trailing zeros: 1.0 -> "1", 0.5 -> "0.5"
        let cpu_str = if cpu.fract() == 0.0 {
            format!("{}", cpu as u32)
        } else {
            format!("{cpu}")
        };
        args.push("--cpus".to_string());
        args.push(cpu_str);
    }

    args.push(image.to_string());
    args.push(instance_id.to_string());

    args
}

/// Handle `~` in cwd for exec commands.
/// Returns `(shell_command, pwd_for_exec)`.
pub fn prepare_exec_command(command: &str, cwd: Option<&str>) -> (String, String) {
    match cwd {
        Some(dir) if dir.starts_with('~') => {
            // Quote the directory to guard against spaces and metacharacters.
            // Use double-quotes to preserve tilde expansion in bash.
            let quoted = format!("\"{}\"", dir.replace('"', "\\\""));
            (format!("cd {quoted} && {command}"), "/tmp".to_string())
        }
        Some(dir) => (command.to_string(), dir.to_string()),
        None => (command.to_string(), "/tmp".to_string()),
    }
}

// ---------------------------------------------------------------------------
// SandboxBackend impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SandboxBackend for SingularitySandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, SandboxError> {
        let uuid_hex = uuid::Uuid::new_v4().to_string().replace('-', "");
        let instance_id = format!("genesis_{}", &uuid_hex[..12]);

        // Ensure scratch dir exists
        std::fs::create_dir_all(&self.scratch_dir).map_err(|e| {
            SandboxError::Other(format!(
                "failed to create scratch dir {}: {e}",
                self.scratch_dir.display()
            ))
        })?;

        // Resolve overlay directory for persistent sandboxes.
        // If resuming from a snapshot, reuse the previous overlay path.
        let overlay_dir: Option<String> = if config.persistent {
            let dir = config
                .snapshot_data
                .as_deref()
                .and_then(
                    |snap| match serde_json::from_str::<serde_json::Value>(snap) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse sandbox snapshot data");
                            None
                        }
                    },
                )
                .and_then(|v| v["overlay_path"].as_str().map(|s| s.to_owned()))
                .unwrap_or_else(|| {
                    self.scratch_dir
                        .join("overlays")
                        .join(&instance_id)
                        .to_string_lossy()
                        .into_owned()
                });
            std::fs::create_dir_all(&dir).map_err(|e| {
                SandboxError::Other(format!("failed to create overlay dir {dir}: {e}",))
            })?;
            Some(dir)
        } else {
            None
        };

        // If the image is a Docker URL, optionally build the SIF
        let image_path = if config.image.starts_with("docker://") {
            let sif_name = sif_cache_key(&config.image);
            let sif_path = self.scratch_dir.join("sif").join(&sif_name);

            if !sif_path.exists() {
                let _lock = self.sif_build_lock.lock().await;
                // Double-check after acquiring lock (another task may have built it)
                if !sif_path.exists() {
                    std::fs::create_dir_all(sif_path.parent().unwrap()).map_err(|e| {
                        SandboxError::Other(format!("failed to create SIF dir: {e}"))
                    })?;
                    let build_output = Command::new(&self.binary)
                        .args([
                            "build",
                            "--force",
                            sif_path.to_str().unwrap_or_default(),
                            &config.image,
                        ])
                        .output()
                        .await
                        .map_err(|e| SandboxError::SubprocessFailed {
                            reason: format!("SIF build spawn failed: {e}"),
                        })?;
                    if !build_output.status.success() {
                        let stderr = String::from_utf8_lossy(&build_output.stderr).into_owned();
                        return Err(SandboxError::SubprocessFailed {
                            reason: format!("SIF build failed: {stderr}"),
                        });
                    }
                }
            }
            sif_path.to_string_lossy().into_owned()
        } else {
            config.image.clone()
        };

        let args = build_start_args(
            &image_path,
            &instance_id,
            config.persistent,
            overlay_dir.clone(),
            config.cpu,
            config.memory_mb,
        );

        let start_output = Command::new(&self.binary)
            .args(&args)
            .output()
            .await
            .map_err(|e| SandboxError::SubprocessFailed {
                reason: format!("instance start spawn failed: {e}"),
            })?;

        if !start_output.status.success() {
            let stderr = String::from_utf8_lossy(&start_output.stderr).into_owned();
            return Err(SandboxError::SubprocessFailed {
                reason: format!("instance start failed: {stderr}"),
            });
        }

        let snapshot_data = overlay_dir
            .as_ref()
            .map(|dir| json!({"overlay_path": dir}).to_string());

        let now = SystemTime::now();
        Ok(SandboxInstance {
            id: instance_id,
            backend_type: self.backend_type().to_string(),
            task_id: config.task_id.clone(),
            snapshot_data,
            persistent: config.persistent,
            created_at: now,
            last_active: now,
            cache_instant: std::time::Instant::now(),
        })
    }

    async fn execute(
        &self,
        instance: &SandboxInstance,
        command: &str,
        working_dir: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<ExecResult, SandboxError> {
        let (shell_cmd, pwd) = prepare_exec_command(command, working_dir);

        let mut cmd = Command::new(&self.binary);
        cmd.args([
            "exec",
            "--pwd",
            &pwd,
            &format!("instance://{}", instance.id),
            "bash",
            "-c",
            &shell_cmd,
        ]);

        let exec_future = cmd.output();

        let output = if let Some(dur) = timeout {
            tokio::time::timeout(dur, exec_future)
                .await
                .map_err(|_| SandboxError::Timeout {
                    seconds: dur.as_secs(),
                })?
                .map_err(|e| SandboxError::SubprocessFailed {
                    reason: format!("exec spawn failed: {e}"),
                })?
        } else {
            exec_future
                .await
                .map_err(|e| SandboxError::SubprocessFailed {
                    reason: format!("exec spawn failed: {e}"),
                })?
        };

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let combined = if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}{stderr}")
        };

        Ok(ExecResult {
            output: combined,
            exit_code,
        })
    }

    async fn snapshot(&self, instance: &SandboxInstance) -> Result<Option<String>, SandboxError> {
        Ok(instance.snapshot_data.clone())
    }

    async fn cleanup(
        &self,
        instance: &SandboxInstance,
        _persistent: bool,
    ) -> Result<(), SandboxError> {
        let output = Command::new(&self.binary)
            .args(["instance", "stop", &instance.id])
            .output()
            .await
            .map_err(|e| SandboxError::SubprocessFailed {
                reason: format!("instance stop spawn failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(SandboxError::SubprocessFailed {
                reason: format!("instance stop failed: {stderr}"),
            });
        }

        Ok(())
    }

    fn backend_type(&self) -> &'static str {
        "singularity"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env-mutating tests to avoid race conditions between threads.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_sif_cache_key() {
        assert_eq!(
            sif_cache_key("docker://nikolaik/python-nodejs:python3.11-nodejs20"),
            "nikolaik-python-nodejs-python3.11-nodejs20.sif"
        );
        assert_eq!(sif_cache_key("docker://ubuntu:22.04"), "ubuntu-22.04.sif");
        // No docker:// prefix — just sanitize
        assert_eq!(sif_cache_key("myimage:latest"), "myimage-latest.sif");
    }

    #[test]
    fn test_resolve_scratch_dir_env_override() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("TERMINAL_SANDBOX_DIR");
        std::env::set_var("TERMINAL_SCRATCH_DIR", "/custom/scratch");
        let dir = resolve_scratch_dir();
        std::env::remove_var("TERMINAL_SCRATCH_DIR");
        assert_eq!(dir, PathBuf::from("/custom/scratch"));
    }

    #[test]
    fn test_resolve_scratch_dir_sandbox_dir_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("TERMINAL_SCRATCH_DIR");
        std::env::set_var("TERMINAL_SANDBOX_DIR", "/sandbox/base");
        let dir = resolve_scratch_dir();
        std::env::remove_var("TERMINAL_SANDBOX_DIR");
        assert_eq!(dir, PathBuf::from("/sandbox/base/singularity"));
    }

    #[test]
    fn test_build_start_args_ephemeral() {
        let args = build_start_args(
            "docker://ubuntu:22.04",
            "genesis_abc123456789",
            false,
            None,
            0.0,
            0,
        );
        assert!(args.contains(&"--writable-tmpfs".to_string()));
        assert!(!args.contains(&"--overlay".to_string()));
        assert!(args.contains(&"instance".to_string()));
        assert!(args.contains(&"start".to_string()));
        assert!(args.contains(&"--containall".to_string()));
        assert!(args.contains(&"--no-home".to_string()));
        // Last two should be image and instance id
        assert_eq!(args[args.len() - 2], "docker://ubuntu:22.04");
        assert_eq!(args[args.len() - 1], "genesis_abc123456789");
    }

    #[test]
    fn test_build_start_args_persistent() {
        let args = build_start_args(
            "docker://ubuntu:22.04",
            "genesis_abc123456789",
            true,
            Some("/scratch/overlays/genesis_abc123456789".to_string()),
            2.0,
            512,
        );
        assert!(!args.contains(&"--writable-tmpfs".to_string()));
        let overlay_pos = args
            .iter()
            .position(|a| a == "--overlay")
            .expect("--overlay missing");
        assert_eq!(
            args[overlay_pos + 1],
            "/scratch/overlays/genesis_abc123456789"
        );
        // memory
        let mem_pos = args
            .iter()
            .position(|a| a == "--memory")
            .expect("--memory missing");
        assert_eq!(args[mem_pos + 1], "512M");
        // cpu
        let cpu_pos = args
            .iter()
            .position(|a| a == "--cpus")
            .expect("--cpus missing");
        assert_eq!(args[cpu_pos + 1], "2");
    }

    #[test]
    fn test_build_start_args_fractional_cpu() {
        let args = build_start_args("img", "id", false, None, 0.5, 0);
        let cpu_pos = args
            .iter()
            .position(|a| a == "--cpus")
            .expect("--cpus missing");
        assert_eq!(args[cpu_pos + 1], "0.5");
    }

    #[test]
    fn test_prepare_exec_command_tilde() {
        let (cmd, pwd) = prepare_exec_command("ls -la", Some("~/workspace"));
        assert_eq!(cmd, "cd \"~/workspace\" && ls -la");
        assert_eq!(pwd, "/tmp");
    }

    #[test]
    fn test_prepare_exec_command_tilde_with_spaces() {
        let (cmd, pwd) = prepare_exec_command("ls", Some("~/my project"));
        assert_eq!(cmd, "cd \"~/my project\" && ls");
        assert_eq!(pwd, "/tmp");
    }

    #[test]
    fn test_prepare_exec_command_normal() {
        let (cmd, pwd) = prepare_exec_command("ls -la", Some("/home/user/workspace"));
        assert_eq!(cmd, "ls -la");
        assert_eq!(pwd, "/home/user/workspace");
    }

    #[test]
    fn test_prepare_exec_command_no_cwd() {
        let (cmd, pwd) = prepare_exec_command("echo hello", None);
        assert_eq!(cmd, "echo hello");
        assert_eq!(pwd, "/tmp");
    }
}
