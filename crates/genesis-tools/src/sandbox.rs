use std::path::{Component, Path, PathBuf};

use thiserror::Error;

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
}
