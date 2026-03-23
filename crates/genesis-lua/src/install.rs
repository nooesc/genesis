use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const SOURCE_METADATA_FILE: &str = ".genesis-source.json";
/// Maximum download size for GitHub tarballs (50 MB).
const MAX_DOWNLOAD_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    GitHub {
        owner: String,
        repo: String,
        git_ref: Option<String>,
        path: Option<String>,
    },
    Local(PathBuf),
}

impl PluginSource {
    /// Parse a source string into a `PluginSource`.
    ///
    /// Accepted formats:
    /// - `github:owner/repo`
    /// - `github:owner/repo@ref`
    /// - `github:owner/repo/sub/path`
    /// - `github:owner/repo/sub/path@ref`
    /// - `https://github.com/owner/repo`
    /// - `https://github.com/owner/repo/tree/ref/sub/path`
    /// - `/absolute/local/path`
    /// - `./relative/local/path`
    pub fn parse(source: &str) -> Result<Self, InstallError> {
        if let Some(rest) = source.strip_prefix("github:") {
            return Self::parse_github_shorthand(rest);
        }
        if source.starts_with("https://github.com/") || source.starts_with("http://github.com/") {
            return Self::parse_github_url(source);
        }
        // Treat anything else as a local path.
        let path = PathBuf::from(source);
        Ok(PluginSource::Local(path))
    }

    fn parse_github_shorthand(rest: &str) -> Result<Self, InstallError> {
        // Split off an optional @ref suffix.
        let (path_part, git_ref) = if let Some(at_pos) = rest.rfind('@') {
            let (p, r) = rest.split_at(at_pos);
            (p, Some(r[1..].to_owned()))
        } else {
            (rest, None)
        };

        let segments: Vec<&str> = path_part.split('/').collect();
        if segments.len() < 2 {
            return Err(InstallError::InvalidSource {
                reason: format!(
                    "github source must be at least `owner/repo`, got `{path_part}`"
                ),
            });
        }

        let owner = segments[0].to_owned();
        let repo = segments[1].to_owned();
        let sub_path = if segments.len() > 2 {
            Some(segments[2..].join("/"))
        } else {
            None
        };

        Ok(PluginSource::GitHub {
            owner,
            repo,
            git_ref,
            path: sub_path,
        })
    }

    fn parse_github_url(url: &str) -> Result<Self, InstallError> {
        // Strip the scheme + host prefix.
        let rest = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("http://github.com/"))
            .unwrap_or(url);
        // Remove trailing slash / .git suffix.
        let rest = rest.trim_end_matches('/').trim_end_matches(".git");

        let segments: Vec<&str> = rest.split('/').collect();
        if segments.len() < 2 {
            return Err(InstallError::InvalidSource {
                reason: format!("github URL must contain owner/repo, got `{url}`"),
            });
        }

        let owner = segments[0].to_owned();
        let repo = segments[1].to_owned();

        // If URL contains /tree/ref/... extract ref and sub-path.
        let (git_ref, sub_path) = if segments.len() > 3 && segments[2] == "tree" {
            let r = segments[3].to_owned();
            let p = if segments.len() > 4 {
                Some(segments[4..].join("/"))
            } else {
                None
            };
            (Some(r), p)
        } else {
            (None, None)
        };

        Ok(PluginSource::GitHub {
            owner,
            repo,
            git_ref,
            path: sub_path,
        })
    }

    /// A human-readable source string suitable for serialisation.
    pub fn display_string(&self) -> String {
        match self {
            PluginSource::GitHub {
                owner,
                repo,
                git_ref,
                path,
            } => {
                let mut s = format!("github:{owner}/{repo}");
                if let Some(p) = path {
                    s.push('/');
                    s.push_str(p);
                }
                if let Some(r) = git_ref {
                    s.push('@');
                    s.push_str(r);
                }
                s
            }
            PluginSource::Local(path) => path.display().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSourceInfo {
    pub source: String,
    pub installed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("source not found: {reason}")]
    SourceNotFound { reason: String },

    #[error("invalid source: {reason}")]
    InvalidSource { reason: String },

    #[error("download failed: {reason}")]
    DownloadFailed { reason: String },

    #[error("extraction failed: {reason}")]
    ExtractionFailed { reason: String },

    #[error("no plugin found in source (missing init.lua/plugin.toml or .lua file)")]
    NoPluginFound,

    #[error("plugin `{name}` already exists; use --force to overwrite")]
    AlreadyExists { name: String },

    #[error("plugin `{name}` is not installed")]
    NotInstalled { name: String },

    #[error("cannot uninstall bundled plugin `{name}`")]
    CannotUninstallBundled { name: String },

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Install a plugin from a source into `plugin_dir`.
///
/// Returns the installed plugin name on success.
pub fn install_plugin(
    source: &PluginSource,
    plugin_dir: &Path,
    force: bool,
) -> Result<String, InstallError> {
    let source_label = source.display_string();
    match source {
        PluginSource::GitHub {
            owner,
            repo,
            git_ref,
            path,
        } => install_from_github(owner, repo, git_ref.as_deref(), path.as_deref(), plugin_dir, force, &source_label),
        PluginSource::Local(local_path) => install_from_local(local_path, plugin_dir, force),
    }
}

/// Uninstall a plugin by name from `plugin_dir`.
pub fn uninstall_plugin(name: &str, plugin_dir: &Path) -> Result<(), InstallError> {
    // Check for package directory first.
    let package_path = plugin_dir.join(name);
    if package_path.is_dir() {
        fs::remove_dir_all(&package_path)?;
        return Ok(());
    }
    // Check for single-file plugin.
    let file_path = plugin_dir.join(format!("{name}.lua"));
    if file_path.is_file() {
        fs::remove_file(&file_path)?;
        return Ok(());
    }

    Err(InstallError::NotInstalled {
        name: name.to_owned(),
    })
}

/// Read the `.genesis-source.json` metadata from a plugin directory.
pub fn read_source_info(plugin_dir: &Path) -> Option<PluginSourceInfo> {
    let meta_path = plugin_dir.join(SOURCE_METADATA_FILE);
    let data = fs::read_to_string(meta_path).ok()?;
    serde_json::from_str(&data).ok()
}

fn install_from_github(
    owner: &str,
    repo: &str,
    git_ref: Option<&str>,
    sub_path: Option<&str>,
    plugin_dir: &Path,
    force: bool,
    source_label: &str,
) -> Result<String, InstallError> {
    let ref_str = git_ref.unwrap_or("HEAD");
    let tarball_url = format!("https://api.github.com/repos/{owner}/{repo}/tarball/{ref_str}");

    // Download the tarball.
    let response = reqwest::blocking::Client::new()
        .get(&tarball_url)
        .header("User-Agent", "genesis-lua-installer")
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|err| InstallError::DownloadFailed {
            reason: err.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(InstallError::DownloadFailed {
            reason: format!("HTTP {} from {tarball_url}", response.status()),
        });
    }

    // Reject downloads that exceed the size limit.
    if let Some(len) = response.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(InstallError::DownloadFailed {
                reason: format!(
                    "tarball too large ({len} bytes, max {MAX_DOWNLOAD_BYTES})"
                ),
            });
        }
    }

    let bytes = response.bytes().map_err(|err| InstallError::DownloadFailed {
        reason: err.to_string(),
    })?;

    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(InstallError::DownloadFailed {
            reason: format!(
                "tarball too large ({} bytes, max {MAX_DOWNLOAD_BYTES})",
                bytes.len()
            ),
        });
    }

    // Extract to a temp directory.
    let tmp_dir = tempfile::tempdir().map_err(|err| InstallError::Io(err.into()))?;
    let decoder = flate2::read::GzDecoder::new(io::Cursor::new(&bytes));
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(tmp_dir.path())
        .map_err(|err| InstallError::ExtractionFailed {
            reason: err.to_string(),
        })?;

    // GitHub tarballs extract into a single top-level directory named
    // `{owner}-{repo}-{short_sha}`.  Find it.
    let top_level = find_single_child_dir(tmp_dir.path())?;

    // If a sub-path was requested, navigate into it.
    let source_root = if let Some(sub) = sub_path {
        let candidate = top_level.join(sub);
        if !candidate.exists() {
            return Err(InstallError::SourceNotFound {
                reason: format!("sub-path `{sub}` not found in repository"),
            });
        }
        candidate
    } else {
        top_level
    };

    // Determine plugin name and validate.
    let plugin_name = detect_plugin_name(&source_root)?;

    // Check for conflicts.
    let dest = plugin_dir.join(&plugin_name);
    if dest.exists() && !force {
        return Err(InstallError::AlreadyExists {
            name: plugin_name,
        });
    }

    // Ensure plugin_dir exists.
    fs::create_dir_all(plugin_dir)?;

    // Copy the plugin.
    if source_root.is_file() {
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        fs::copy(&source_root, &dest)?;
    } else {
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        copy_dir_recursive(&source_root, &dest)?;
    }

    // Write source metadata.
    write_source_info(&dest, source_label, None)?;

    Ok(plugin_name)
}

fn install_from_local(
    local_path: &Path,
    plugin_dir: &Path,
    force: bool,
) -> Result<String, InstallError> {
    if !local_path.exists() {
        return Err(InstallError::SourceNotFound {
            reason: format!("path `{}` does not exist", local_path.display()),
        });
    }

    let plugin_name = detect_plugin_name(local_path)?;

    // Determine destination.
    let dest = if local_path.is_file() {
        plugin_dir.join(format!("{plugin_name}.lua"))
    } else {
        plugin_dir.join(&plugin_name)
    };

    if dest.exists() && !force {
        return Err(InstallError::AlreadyExists {
            name: plugin_name,
        });
    }

    // Ensure plugin_dir exists.
    fs::create_dir_all(plugin_dir)?;

    // Copy.
    if local_path.is_file() {
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        fs::copy(local_path, &dest)?;
    } else {
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        copy_dir_recursive(local_path, &dest)?;
    }

    // Write source metadata for package plugins.
    if dest.is_dir() {
        let source_str = local_path.display().to_string();
        write_source_info(&dest, &source_str, None)?;
    }

    Ok(plugin_name)
}

/// Detect the plugin name from a path.
///
/// - Single `.lua` file: name is the file stem.
/// - Directory with `plugin.toml`: name is from the manifest.
/// - Directory with `init.lua` but no manifest: name is the directory name.
fn detect_plugin_name(path: &Path) -> Result<String, InstallError> {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "lua") {
            return path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_owned())
                .ok_or(InstallError::NoPluginFound);
        }
        return Err(InstallError::NoPluginFound);
    }

    // Directory — check for plugin.toml first.
    let manifest_path = path.join("plugin.toml");
    if manifest_path.is_file() {
        let raw = fs::read_to_string(&manifest_path)?;
        #[derive(Deserialize)]
        struct Partial {
            plugin: PartialPlugin,
        }
        #[derive(Deserialize)]
        struct PartialPlugin {
            name: String,
        }
        let parsed: Partial = toml::from_str(&raw).map_err(|err| InstallError::InvalidSource {
            reason: format!("failed to parse plugin.toml: {err}"),
        })?;
        return Ok(parsed.plugin.name);
    }

    // init.lua without plugin.toml — use directory name.
    let init_path = path.join("init.lua");
    if init_path.is_file() {
        return path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_owned())
            .ok_or(InstallError::NoPluginFound);
    }

    // Try to find a single .lua file in the directory (for repos where the
    // plugin is a single file inside a directory).
    let mut lua_files = Vec::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let ep = entry.path();
            if ep.is_file() && ep.extension().is_some_and(|ext| ext == "lua") {
                lua_files.push(ep);
            }
        }
    }
    if lua_files.len() == 1 {
        return lua_files[0]
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_owned())
            .ok_or(InstallError::NoPluginFound);
    }

    Err(InstallError::NoPluginFound)
}

/// Find the single child directory in a path (for GitHub tarball extraction).
fn find_single_child_dir(path: &Path) -> Result<PathBuf, InstallError> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    match dirs.len() {
        1 => Ok(dirs.into_iter().next().unwrap()),
        0 => Err(InstallError::ExtractionFailed {
            reason: "no directory found in extracted tarball".to_owned(),
        }),
        n => Err(InstallError::ExtractionFailed {
            reason: format!("expected 1 top-level directory in tarball, found {n}"),
        }),
    }
}

/// Recursively copy a directory and its contents.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), InstallError> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

fn write_source_info(
    plugin_path: &Path,
    source: &str,
    commit: Option<String>,
) -> Result<(), InstallError> {
    let info = PluginSourceInfo {
        source: source.to_owned(),
        installed_at: now_iso8601(),
        commit,
    };
    let json = serde_json::to_string_pretty(&info).map_err(|err| InstallError::Io(
        io::Error::new(io::ErrorKind::Other, err),
    ))?;
    let target = if plugin_path.is_dir() {
        plugin_path.join(SOURCE_METADATA_FILE)
    } else {
        // For single-file plugins we cannot embed metadata.  Skip.
        return Ok(());
    };
    fs::write(target, json)?;
    Ok(())
}

/// Simple ISO-8601 UTC timestamp without requiring chrono.
fn now_iso8601() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Format as seconds since epoch (good enough for ordering; we do not pull
    // in chrono just for a pretty timestamp).
    //
    // Actually let us do a rough calendar conversion so the value is human-
    // readable. We could use chrono, but this avoids a new dependency.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01 → year/month/day (Gregorian).
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm adapted from Howard Hinnant's civil_from_days.
    days += 719_468;
    let era = days / 146_097;
    let doe = days % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- PluginSource::parse tests ---

    #[test]
    fn parse_github_shorthand() {
        let src = PluginSource::parse("github:acme/weather").unwrap();
        assert_eq!(
            src,
            PluginSource::GitHub {
                owner: "acme".into(),
                repo: "weather".into(),
                git_ref: None,
                path: None,
            }
        );
    }

    #[test]
    fn parse_github_with_ref() {
        let src = PluginSource::parse("github:acme/weather@v1.0").unwrap();
        assert_eq!(
            src,
            PluginSource::GitHub {
                owner: "acme".into(),
                repo: "weather".into(),
                git_ref: Some("v1.0".into()),
                path: None,
            }
        );
    }

    #[test]
    fn parse_github_with_path() {
        let src = PluginSource::parse("github:acme/plugins/weather").unwrap();
        assert_eq!(
            src,
            PluginSource::GitHub {
                owner: "acme".into(),
                repo: "plugins".into(),
                git_ref: None,
                path: Some("weather".into()),
            }
        );
    }

    #[test]
    fn parse_github_with_path_and_ref() {
        let src = PluginSource::parse("github:acme/plugins/weather@main").unwrap();
        assert_eq!(
            src,
            PluginSource::GitHub {
                owner: "acme".into(),
                repo: "plugins".into(),
                git_ref: Some("main".into()),
                path: Some("weather".into()),
            }
        );
    }

    #[test]
    fn parse_github_url() {
        let src = PluginSource::parse("https://github.com/acme/weather").unwrap();
        assert_eq!(
            src,
            PluginSource::GitHub {
                owner: "acme".into(),
                repo: "weather".into(),
                git_ref: None,
                path: None,
            }
        );
    }

    #[test]
    fn parse_github_url_with_tree() {
        let src =
            PluginSource::parse("https://github.com/acme/plugins/tree/main/weather").unwrap();
        assert_eq!(
            src,
            PluginSource::GitHub {
                owner: "acme".into(),
                repo: "plugins".into(),
                git_ref: Some("main".into()),
                path: Some("weather".into()),
            }
        );
    }

    #[test]
    fn parse_local_path() {
        let src = PluginSource::parse("/opt/plugins/weather").unwrap();
        assert_eq!(
            src,
            PluginSource::Local(PathBuf::from("/opt/plugins/weather"))
        );
    }

    #[test]
    fn parse_invalid_github_shorthand() {
        let err = PluginSource::parse("github:onlyone").unwrap_err();
        assert!(matches!(err, InstallError::InvalidSource { .. }));
    }

    // --- install_plugin (local single file) ---

    #[test]
    fn install_local_single_file_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        fs::write(source_dir.join("hello.lua"), "genesis.log('hi')").unwrap();

        let plugin_dir = tmp.path().join("plugins");
        let src = PluginSource::Local(source_dir.join("hello.lua"));
        let name = install_plugin(&src, &plugin_dir, false).unwrap();

        assert_eq!(name, "hello");
        assert!(plugin_dir.join("hello.lua").is_file());
    }

    #[test]
    fn install_local_package_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("weather-pkg");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("init.lua"), "return true").unwrap();
        fs::write(
            source_dir.join("plugin.toml"),
            r#"
[plugin]
name = "weather"
version = "0.1.0"
"#,
        )
        .unwrap();

        let plugin_dir = tmp.path().join("plugins");
        let src = PluginSource::Local(source_dir);
        let name = install_plugin(&src, &plugin_dir, false).unwrap();

        assert_eq!(name, "weather");
        assert!(plugin_dir.join("weather").is_dir());
        assert!(plugin_dir.join("weather").join("init.lua").is_file());
        assert!(plugin_dir.join("weather").join("plugin.toml").is_file());

        // Check source metadata was written.
        let info = read_source_info(&plugin_dir.join("weather"));
        assert!(info.is_some());
        let info = info.unwrap();
        assert!(!info.installed_at.is_empty());
    }

    // --- uninstall_plugin ---

    #[test]
    fn uninstall_package_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        let pkg = plugin_dir.join("weather");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("init.lua"), "return true").unwrap();

        uninstall_plugin("weather", &plugin_dir).unwrap();
        assert!(!pkg.exists());
    }

    #[test]
    fn uninstall_single_file_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("hello.lua"), "return 1").unwrap();

        uninstall_plugin("hello", &plugin_dir).unwrap();
        assert!(!plugin_dir.join("hello.lua").exists());
    }

    #[test]
    fn uninstall_missing_plugin_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugin_dir).unwrap();

        let err = uninstall_plugin("nonexistent", &plugin_dir).unwrap_err();
        assert!(matches!(err, InstallError::NotInstalled { .. }));
    }

    // --- already-exists / force ---

    #[test]
    fn already_exists_blocks_install() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        fs::write(source_dir.join("hello.lua"), "return 1").unwrap();

        let plugin_dir = tmp.path().join("plugins");
        let src = PluginSource::Local(source_dir.join("hello.lua"));
        install_plugin(&src, &plugin_dir, false).unwrap();

        let err = install_plugin(&src, &plugin_dir, false).unwrap_err();
        assert!(matches!(err, InstallError::AlreadyExists { .. }));
    }

    #[test]
    fn force_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        fs::write(source_dir.join("hello.lua"), "return 1").unwrap();

        let plugin_dir = tmp.path().join("plugins");
        let src = PluginSource::Local(source_dir.join("hello.lua"));
        install_plugin(&src, &plugin_dir, false).unwrap();

        // Update the source file.
        fs::write(source_dir.join("hello.lua"), "return 2").unwrap();

        let name = install_plugin(&src, &plugin_dir, true).unwrap();
        assert_eq!(name, "hello");

        let content = fs::read_to_string(plugin_dir.join("hello.lua")).unwrap();
        assert_eq!(content, "return 2");
    }

    // --- read_source_info ---

    #[test]
    fn read_source_info_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_source_info(tmp.path()).is_none());
    }

    #[test]
    fn read_source_info_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let info = PluginSourceInfo {
            source: "github:acme/weather".into(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            commit: Some("abc123".into()),
        };
        let json = serde_json::to_string_pretty(&info).unwrap();
        fs::write(tmp.path().join(SOURCE_METADATA_FILE), json).unwrap();

        let loaded = read_source_info(tmp.path()).unwrap();
        assert_eq!(loaded.source, "github:acme/weather");
        assert_eq!(loaded.commit, Some("abc123".into()));
    }

    // --- display_string ---

    #[test]
    fn display_string_github() {
        let src = PluginSource::GitHub {
            owner: "acme".into(),
            repo: "weather".into(),
            git_ref: Some("v1.0".into()),
            path: Some("plugins/weather".into()),
        };
        assert_eq!(src.display_string(), "github:acme/weather/plugins/weather@v1.0");
    }

    #[test]
    fn display_string_local() {
        let src = PluginSource::Local(PathBuf::from("/opt/plugins/weather"));
        assert_eq!(src.display_string(), "/opt/plugins/weather");
    }

    // --- source not found ---

    #[test]
    fn install_local_nonexistent_path_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        let src = PluginSource::Local(PathBuf::from("/nonexistent/path/plugin.lua"));
        let err = install_plugin(&src, &plugin_dir, false).unwrap_err();
        assert!(matches!(err, InstallError::SourceNotFound { .. }));
    }
}
