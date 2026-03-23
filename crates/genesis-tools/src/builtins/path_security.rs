//! Path canonicalization and validation for filesystem tools.
//!
//! Provides [`validate_path`] which:
//! - Canonicalizes paths (resolves symlinks, `..`, `.`)
//! - Enforces working-directory containment when `context.default_working_dir` is set
//! - Blocks access to known sensitive paths (`.ssh/`, `.gnupg/`, `.aws/credentials`, etc.)
//!
//! For paths that don't exist yet (e.g. write targets), the parent directory is
//! canonicalized and the final component is appended.

use std::path::{Path, PathBuf};

use crate::{ToolContext, ToolError};

/// Sensitive path fragments that are always blocked regardless of context.
///
/// Each entry is matched as a component-boundary pattern: a path is blocked
/// when any of its components (or the full canonical string) contains the
/// fragment.  The check is case-sensitive on purpose — the listed paths are
/// well-known Unix locations.
const SENSITIVE_FRAGMENTS: &[&str] = &[
    "/.ssh/",
    "/.ssh",
    "/.gnupg/",
    "/.gnupg",
    "/.aws/credentials",
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/.env",
];

/// Returns `true` when `canonical` refers to (or is inside) a sensitive path.
fn is_sensitive(canonical: &Path) -> bool {
    let s = canonical.to_string_lossy();
    for frag in SENSITIVE_FRAGMENTS {
        // Exact tail match (path *is* the sensitive location).
        if s.ends_with(frag) {
            return true;
        }
        // Interior match (path is inside a sensitive directory).
        if s.contains(frag) {
            return true;
        }
    }
    false
}

/// Validate and canonicalize a user-supplied path.
///
/// # Rules
/// 1. **Sensitive-path blocking** — always enforced.
/// 2. **Working-directory containment** — only when
///    `context.default_working_dir` is `Some`.  The canonical path must be
///    equal to or underneath the canonical working directory.
///
/// For paths that do not yet exist (write targets), the *parent* directory is
/// canonicalized and the file-name component is appended.  If even the parent
/// does not exist, the error explains the situation.
///
/// Returns the canonical `PathBuf` on success.
pub fn validate_path(
    raw: &str,
    tool_name: &str,
    context: &ToolContext,
) -> Result<PathBuf, ToolError> {
    let path = Path::new(raw);

    // --- Canonicalize ---
    let canonical = if path.exists() {
        path.canonicalize().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to canonicalize `{raw}`: {e}"),
        })?
    } else {
        // Path doesn't exist yet — canonicalize the parent and append the
        // file name so WriteFileTool can create new files.
        let parent = path.parent().unwrap_or(Path::new("."));
        let file_name = path
            .file_name()
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!("invalid path `{raw}`: cannot determine file name"),
            })?;

        // The parent must exist (or be ".") so we can canonicalize it.
        let canonical_parent = if parent.as_os_str().is_empty() || parent == Path::new(".") {
            std::env::current_dir().map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!("failed to determine current directory: {e}"),
            })?
        } else if parent.exists() {
            parent
                .canonicalize()
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: tool_name.to_owned(),
                    reason: format!(
                        "failed to canonicalize parent directory `{}`: {e}",
                        parent.display()
                    ),
                })?
        } else {
            // Parent chain doesn't exist — try to resolve as far as possible
            // by walking up until we find a real ancestor.
            resolve_best_effort(parent).map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!(
                    "failed to resolve parent directory `{}`: {e}",
                    parent.display()
                ),
            })?
        };

        canonical_parent.join(file_name)
    };

    // --- Sensitive-path check (always enforced) ---
    if is_sensitive(&canonical) {
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!(
                "access denied: `{}` is a sensitive path",
                canonical.display()
            ),
        });
    }

    // --- Working-directory containment (only when set) ---
    if let Some(ref working_dir) = context.default_working_dir {
        let wd = Path::new(working_dir);
        let canonical_wd = wd.canonicalize().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!(
                "failed to canonicalize working directory `{working_dir}`: {e}"
            ),
        })?;

        if !canonical.starts_with(&canonical_wd) {
            return Err(ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!(
                    "path `{}` is outside the allowed working directory `{}`",
                    canonical.display(),
                    canonical_wd.display()
                ),
            });
        }
    }

    Ok(canonical)
}

/// Walk up the path until we find a real directory, canonicalize it, then
/// re-append the remaining components.  This lets us handle cases like
/// `/real/dir/new_subdir/file.txt` where `new_subdir` doesn't exist yet.
fn resolve_best_effort(path: &Path) -> Result<PathBuf, std::io::Error> {
    let mut components: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;

    loop {
        if current.exists() {
            let mut resolved = current.canonicalize()?;
            for c in components.into_iter().rev() {
                resolved.push(c);
            }
            return Ok(resolved);
        }

        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                components.push(name);
                current = parent;
            }
            _ => {
                // Reached the root or an invalid state — fall back to the
                // original path (validation will still catch traversal).
                return Ok(path.to_path_buf());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    fn ctx_with_working_dir(dir: &str) -> ToolContext {
        ToolContext {
            default_working_dir: Some(dir.to_owned()),
            ..ctx()
        }
    }

    // --- Sensitive path tests ---

    #[test]
    fn blocks_ssh_directory() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        let ssh_key = format!("{home}/.ssh/id_rsa");
        let result = validate_path(&ssh_key, "read_file", &ctx());
        assert!(result.is_err(), "should block .ssh access");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("sensitive path"), "error: {err}");
    }

    #[test]
    fn blocks_gnupg_directory() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        let gpg = format!("{home}/.gnupg/pubring.kbx");
        let result = validate_path(&gpg, "read_file", &ctx());
        assert!(result.is_err(), "should block .gnupg access");
    }

    #[test]
    fn blocks_aws_credentials() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        let aws = format!("{home}/.aws/credentials");
        let result = validate_path(&aws, "read_file", &ctx());
        assert!(result.is_err(), "should block .aws/credentials access");
    }

    #[test]
    fn blocks_etc_shadow() {
        let result = validate_path("/etc/shadow", "read_file", &ctx());
        assert!(result.is_err(), "should block /etc/shadow");
    }

    #[test]
    fn blocks_etc_passwd() {
        let result = validate_path("/etc/passwd", "read_file", &ctx());
        assert!(result.is_err(), "should block /etc/passwd");
    }

    #[test]
    fn blocks_env_file() {
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(&env_path, "SECRET=value").unwrap();
        let result = validate_path(&env_path.to_string_lossy(), "read_file", &ctx());
        assert!(result.is_err(), "should block .env files");
    }

    #[test]
    fn blocks_etc_sudoers() {
        let result = validate_path("/etc/sudoers", "read_file", &ctx());
        assert!(result.is_err(), "should block /etc/sudoers");
    }

    // --- Working directory containment tests ---

    #[test]
    fn allows_path_inside_working_dir() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("allowed.txt");
        fs::write(&file, "ok").unwrap();

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let result = validate_path(&file.to_string_lossy(), "read_file", &ctx);
        assert!(result.is_ok(), "should allow path inside working dir");
    }

    #[test]
    fn blocks_path_outside_working_dir() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let file = outside.path().join("outside.txt");
        fs::write(&file, "nope").unwrap();

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let result = validate_path(&file.to_string_lossy(), "read_file", &ctx);
        assert!(result.is_err(), "should block path outside working dir");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("outside the allowed working directory"), "error: {err}");
    }

    #[test]
    fn blocks_path_traversal_with_dotdot() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("sub");
        fs::create_dir(&subdir).unwrap();
        let file = dir.path().join("secret.txt");
        fs::write(&file, "secret").unwrap();

        let ctx = ctx_with_working_dir(&subdir.to_string_lossy());
        // Try to escape via ../
        let traversal = format!("{}/../../etc/hosts", subdir.display());
        let result = validate_path(&traversal, "read_file", &ctx);
        assert!(result.is_err(), "should block path traversal via ..");
    }

    #[test]
    fn allows_path_without_working_dir() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("anywhere.txt");
        fs::write(&file, "ok").unwrap();

        // No working_dir set — containment is not enforced.
        let ctx = ctx();
        let result = validate_path(&file.to_string_lossy(), "read_file", &ctx);
        assert!(result.is_ok(), "should allow any path when no working_dir is set");
    }

    // --- New file creation tests ---

    #[test]
    fn allows_new_file_in_valid_directory() {
        let dir = tempdir().unwrap();
        let new_file = dir.path().join("new_file.txt");

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let result = validate_path(&new_file.to_string_lossy(), "write_file", &ctx);
        assert!(result.is_ok(), "should allow creating new file in working dir");
    }

    #[test]
    fn blocks_new_file_outside_working_dir() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let new_file = outside.path().join("new_outside.txt");

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let result = validate_path(&new_file.to_string_lossy(), "write_file", &ctx);
        assert!(result.is_err(), "should block new file outside working dir");
    }

    #[test]
    fn allows_new_file_in_nonexistent_subdirectory() {
        let dir = tempdir().unwrap();
        let new_file = dir.path().join("new_sub").join("file.txt");

        let ctx = ctx_with_working_dir(&dir.path().to_string_lossy());
        let result = validate_path(&new_file.to_string_lossy(), "write_file", &ctx);
        assert!(result.is_ok(), "should allow new file in not-yet-created subdir within working dir");
    }

    // --- Normal operation tests ---

    #[test]
    fn normal_path_allowed_without_restriction() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("normal.txt");
        fs::write(&file, "contents").unwrap();

        let result = validate_path(&file.to_string_lossy(), "read_file", &ctx());
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("normal.txt"));
    }

    #[test]
    fn returns_canonical_path() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("test.txt");
        fs::write(&file, "test").unwrap();

        // Use a path with .. that should resolve to the same file.
        let dotdot_path = format!("{}/a/b/../b/test.txt", dir.path().display());
        let result = validate_path(&dotdot_path, "read_file", &ctx());
        assert!(result.is_ok());
        let canonical = result.unwrap();
        // The canonical path should not contain ".."
        assert!(!canonical.to_string_lossy().contains(".."));
    }
}
