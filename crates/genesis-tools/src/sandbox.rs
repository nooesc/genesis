use std::fs;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Single-component directory names that are always sensitive (e.g. `.ssh`).
const SENSITIVE_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws", ".docker"];

/// Multi-component relative paths that are sensitive (e.g. `.config/gcloud`).
const SENSITIVE_DIR_PATHS: &[&str] = &[".config/gcloud"];

/// Absolute paths that are sensitive (exact match or parent-of).
const SENSITIVE_ABSOLUTE: &[&str] = &[
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/private/etc/shadow",
    "/private/etc/passwd",
    "/private/etc/sudoers",
];

/// Errors produced by the filesystem sandbox.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// A path was blocked by the sandbox policy.
    #[error("blocked path `{path}`: {reason}")]
    Blocked { path: String, reason: String },

    /// A path could not be resolved to a canonical form.
    #[error("failed to resolve path `{path}`: {reason}")]
    ResolutionFailed { path: String, reason: String },
}

/// Validates and constrains filesystem paths to a sandbox boundary.
#[derive(Debug, Clone)]
pub struct PathValidator {
    /// If set, paths outside this directory are rejected.
    pub working_dir: Option<PathBuf>,
    /// The user's home directory.
    pub home_dir: PathBuf,
}

impl PathValidator {
    /// Creates a new `PathValidator`.
    pub fn new(working_dir: Option<PathBuf>, home_dir: PathBuf) -> Self {
        Self {
            working_dir,
            home_dir,
        }
    }

    /// Returns `true` if `path` refers to a known sensitive location.
    ///
    /// When `working_dir` is provided and `path` starts with it, the path is
    /// considered project-internal and is **not** treated as sensitive — e.g. a
    /// `.ssh/` directory inside the project root is legitimate.
    ///
    /// All matching is done at the **component level** so that paths like
    /// `not.aws/credentials` do not produce false positives.
    pub fn is_sensitive_path(path: &Path, working_dir: Option<&Path>) -> bool {
        // If the path lives inside the working directory, it is project-internal.
        if let Some(wd) = working_dir {
            if path.starts_with(wd) {
                return false;
            }
        }

        // --- Absolute sensitive paths (exact or starts_with with `/` boundary) ---
        let path_str = path.to_string_lossy();
        for &sensitive in SENSITIVE_ABSOLUTE {
            if path_str == sensitive || path_str.starts_with(&format!("{sensitive}/")) {
                return true;
            }
        }

        // Collect Normal components as strings for the remaining checks.
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();

        // --- Single-component sensitive directories ---
        for &dir in SENSITIVE_DIRS {
            if components.contains(&dir) {
                return true;
            }
        }

        // --- Multi-component sensitive directory paths ---
        for &dir_path in SENSITIVE_DIR_PATHS {
            let parts: Vec<&str> = dir_path.split('/').collect();
            if parts.len() <= components.len() {
                for window in components.windows(parts.len()) {
                    if window == parts.as_slice() {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Performs purely lexical normalization of a path (no filesystem access).
    ///
    /// - `.` segments are removed.
    /// - `..` segments pop the previous component (clamped at the root — will
    ///   not escape above `/`).
    /// - Relative paths are prepended with `self.working_dir` (or `.` when no
    ///   working directory is configured).
    /// - Absolute paths are returned as-is (after normalization).
    pub fn normalize_lexical(&self, path: &Path) -> PathBuf {
        // If the path is relative, make it absolute by prepending the working dir.
        let path = if path.is_relative() {
            let base = self
                .working_dir
                .as_deref()
                .unwrap_or_else(|| Path::new("."));
            base.join(path)
        } else {
            path.to_path_buf()
        };

        let mut components: Vec<Component> = Vec::new();

        for component in path.components() {
            match component {
                Component::CurDir => {
                    // Skip `.` — it has no effect.
                }
                Component::ParentDir => {
                    // Pop the last normal component, but never pop past the
                    // root prefix / root dir.
                    match components.last() {
                        Some(Component::Normal(_)) => {
                            components.pop();
                        }
                        _ => {
                            // At or above root — clamp; do not add `..`.
                        }
                    }
                }
                other => {
                    components.push(other);
                }
            }
        }

        if components.is_empty() {
            PathBuf::from(".")
        } else {
            components.iter().collect()
        }
    }

    /// Validates a raw path string against the sandbox policy.
    ///
    /// Steps:
    /// 1. Lexically normalize the path.
    /// 2. Detect and follow symlinks (reject dangling ones).
    /// 3. Canonicalize (walking up to the first existing ancestor if needed).
    /// 4. If `working_dir` is set, enforce containment — paths inside it are
    ///    always allowed, paths outside are blocked.
    /// 5. If no `working_dir`, check against the sensitive-path list.
    pub fn validate(&self, raw: &str) -> Result<PathBuf, SandboxError> {
        // 1. Lexical normalization.
        let normalized = self.normalize_lexical(Path::new(raw));

        // 2. Symlink check.
        if let Ok(meta) = fs::symlink_metadata(&normalized) {
            if meta.file_type().is_symlink() {
                let target =
                    fs::read_link(&normalized).map_err(|e| SandboxError::ResolutionFailed {
                        path: raw.to_string(),
                        reason: format!("could not read symlink: {e}"),
                    })?;
                // Resolve relative symlink targets against the link's parent.
                let resolved = if target.is_relative() {
                    normalized.parent().unwrap_or(Path::new("/")).join(&target)
                } else {
                    target
                };
                if !resolved.exists() {
                    return Err(SandboxError::Blocked {
                        path: raw.to_string(),
                        reason: "dangling symlink".to_string(),
                    });
                }
            }
        }

        // 3. Canonicalize.
        let canonical = if normalized.exists() {
            normalized
                .canonicalize()
                .map_err(|e| SandboxError::ResolutionFailed {
                    path: raw.to_string(),
                    reason: format!("canonicalize failed: {e}"),
                })?
        } else {
            // Walk up to the first existing ancestor and canonicalize that,
            // then re-append the remaining components.
            let mut existing = normalized.as_path();
            let mut tail_components: Vec<&std::ffi::OsStr> = Vec::new();
            loop {
                if existing.exists() {
                    break;
                }
                if let Some(name) = existing.file_name() {
                    tail_components.push(name);
                    existing = existing.parent().unwrap_or(Path::new("/"));
                } else {
                    break;
                }
            }
            let mut canonical_base =
                existing
                    .canonicalize()
                    .map_err(|e| SandboxError::ResolutionFailed {
                        path: raw.to_string(),
                        reason: format!("canonicalize ancestor failed: {e}"),
                    })?;
            for component in tail_components.into_iter().rev() {
                canonical_base.push(component);
            }
            canonical_base
        };

        // 4. Working-dir containment.
        if let Some(ref wd) = self.working_dir {
            let wd_canonical = wd.canonicalize().unwrap_or_else(|_| wd.clone());
            if canonical.starts_with(&wd_canonical) {
                return Ok(canonical);
            }
            return Err(SandboxError::Blocked {
                path: raw.to_string(),
                reason: format!(
                    "path is outside working directory `{}`",
                    wd_canonical.display()
                ),
            });
        }

        // 5. Sensitive-path check (only when no working_dir).
        if Self::is_sensitive_path(&canonical, None) {
            return Err(SandboxError::Blocked {
                path: raw.to_string(),
                reason: "path refers to a sensitive location".to_string(),
            });
        }

        Ok(canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validator() -> PathValidator {
        PathValidator::new(
            Some(PathBuf::from("/projects/myapp")),
            PathBuf::from("/tmp/fake-home"),
        )
    }

    #[test]
    fn normalize_resolves_dot_segments() {
        let v = validator();
        // Absolute path with `.` segments should have them removed.
        let result = v.normalize_lexical(Path::new("/a/./b/./c"));
        assert_eq!(result, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn normalize_resolves_dotdot_segments() {
        let v = validator();
        // `..` pops the preceding component.
        let result = v.normalize_lexical(Path::new("/a/b/../c"));
        assert_eq!(result, PathBuf::from("/a/c"));

        // Multiple `..` segments.
        let result = v.normalize_lexical(Path::new("/a/b/c/../../d"));
        assert_eq!(result, PathBuf::from("/a/d"));

        // `..` at the root is clamped.
        let result = v.normalize_lexical(Path::new("/a/../../b"));
        assert_eq!(result, PathBuf::from("/b"));
    }

    #[test]
    fn normalize_prepends_working_dir_for_relative_paths() {
        let v = validator();
        let result = v.normalize_lexical(Path::new("src/main.rs"));
        assert_eq!(result, PathBuf::from("/projects/myapp/src/main.rs"));

        // Relative path with `..` should resolve against working_dir.
        let result = v.normalize_lexical(Path::new("../other/file.txt"));
        assert_eq!(result, PathBuf::from("/projects/other/file.txt"));
    }

    #[test]
    fn normalize_absolute_path_unchanged() {
        let v = validator();
        // A clean absolute path should pass through unchanged.
        let result = v.normalize_lexical(Path::new("/usr/local/bin/tool"));
        assert_eq!(result, PathBuf::from("/usr/local/bin/tool"));
    }

    // ---- Sensitive path tests ----

    #[test]
    fn blocks_ssh_directory() {
        assert!(PathValidator::is_sensitive_path(
            Path::new("/home/user/.ssh/id_rsa"),
            None,
        ));
    }

    #[test]
    fn blocks_aws_directory() {
        assert!(PathValidator::is_sensitive_path(
            Path::new("/home/user/.aws/sso/cache/token.json"),
            None,
        ));
    }

    #[test]
    fn blocks_etc_shadow() {
        assert!(PathValidator::is_sensitive_path(
            Path::new("/etc/shadow"),
            None,
        ));
    }

    #[test]
    fn blocks_private_etc_shadow_macos() {
        assert!(PathValidator::is_sensitive_path(
            Path::new("/private/etc/shadow"),
            None,
        ));
    }

    #[test]
    fn allows_normal_path() {
        assert!(!PathValidator::is_sensitive_path(
            Path::new("/workspace/src/main.rs"),
            None,
        ));
    }

    #[test]
    fn allows_ssh_inside_working_dir() {
        assert!(!PathValidator::is_sensitive_path(
            Path::new("/workspace/.ssh/config"),
            Some(Path::new("/workspace")),
        ));
    }

    #[test]
    fn no_false_positive_on_not_aws() {
        assert!(!PathValidator::is_sensitive_path(
            Path::new("/home/user/not.aws/credentials"),
            None,
        ));
    }

    // ---- validate() tests ----

    #[test]
    fn validate_allows_path_inside_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();
        let v = PathValidator::new(
            Some(dir.path().to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        );
        assert!(v.validate(file.to_str().unwrap()).is_ok());
    }

    #[test]
    fn validate_blocks_path_outside_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let v = PathValidator::new(
            Some(dir.path().to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        );
        assert!(v.validate("/etc/hosts").is_err());
    }

    #[test]
    fn validate_blocks_traversal_outside_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let v = PathValidator::new(
            Some(dir.path().to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        );
        assert!(v.validate("../../etc/hosts").is_err());
    }

    #[test]
    fn validate_handles_nonexistent_path_inside_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let v = PathValidator::new(
            Some(dir.path().to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        );
        assert!(v
            .validate(dir.path().join("new_file.txt").to_str().unwrap())
            .is_ok());
    }

    #[test]
    fn validate_blocks_sensitive_path_without_working_dir() {
        let v = PathValidator::new(None, PathBuf::from("/home/user"));
        let result = v.validate("/home/user/.ssh/id_rsa");
        assert!(result.is_err());
    }

    #[test]
    fn validate_rejects_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("dangling");
        std::os::unix::fs::symlink("/nonexistent/target", &link).unwrap();
        let v = PathValidator::new(
            Some(dir.path().to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        );
        assert!(v.validate(link.to_str().unwrap()).is_err());
    }

    #[test]
    fn validate_follows_symlink_to_check_target() {
        let dir = tempfile::tempdir().unwrap();
        let real_file = dir.path().join("real.txt");
        std::fs::write(&real_file, "data").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real_file, &link).unwrap();
        let v = PathValidator::new(
            Some(dir.path().to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        );
        assert!(v.validate(link.to_str().unwrap()).is_ok());
    }
}
