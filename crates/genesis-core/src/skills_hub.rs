use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::skill_manifest::{parse_skill_file, scan_skills_dir};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Local,
    GitHub {
        owner: String,
        repo: String,
        path: String,
    },
    Registry {
        url: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub license: String,
    pub tags: Vec<String>,
    pub source: SkillSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillLock {
    pub name: String,
    pub version: String,
    pub source: SkillSource,
    pub installed_at: u64,
    pub checksum: String,
}

#[derive(Debug, Clone)]
pub struct SkillHubClient {
    available_dir: PathBuf,
    installed_dir: PathBuf,
    lock_path: PathBuf,
}

#[derive(Debug)]
pub enum SkillHubError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Parse(String),
    NotFound(String),
    Network(String),
    UnsupportedSource(&'static str),
}

impl std::fmt::Display for SkillHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::Json(err) => write!(f, "json error: {err}"),
            Self::Parse(err) => write!(f, "parse error: {err}"),
            Self::NotFound(name) => write!(f, "skill not found: {name}"),
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::UnsupportedSource(kind) => {
                write!(f, "source type is not supported yet: {kind}")
            }
        }
    }
}

impl std::error::Error for SkillHubError {}

impl From<std::io::Error> for SkillHubError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for SkillHubError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl SkillHubClient {
    pub fn new(available_dir: impl Into<PathBuf>, installed_dir: impl Into<PathBuf>) -> Self {
        let installed_dir = installed_dir.into();
        let lock_path = installed_dir.join("skills.lock.json");
        Self {
            available_dir: available_dir.into(),
            installed_dir,
            lock_path,
        }
    }

    pub fn with_lock_path(
        available_dir: impl Into<PathBuf>,
        installed_dir: impl Into<PathBuf>,
        lock_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            available_dir: available_dir.into(),
            installed_dir: installed_dir.into(),
            lock_path: lock_path.into(),
        }
    }

    pub fn list_available(&self) -> Result<Vec<SkillManifest>, SkillHubError> {
        if !self.available_dir.exists() {
            return Ok(Vec::new());
        }

        let entries = scan_skills_dir(&self.available_dir)
            .map_err(|err| SkillHubError::Parse(err.to_string()))?;
        let mut manifests = Vec::new();

        for entry in entries {
            let parsed = parse_skill_file(&entry.path)
                .map_err(|err| SkillHubError::Parse(err.to_string()))?;
            manifests.push(SkillManifest {
                name: parsed.frontmatter.name,
                description: parsed.frontmatter.description,
                version: parsed.frontmatter.version,
                author: parsed.frontmatter.author,
                license: parsed.frontmatter.license,
                tags: parsed.frontmatter.tags,
                source: SkillSource::Local,
            });
        }

        manifests.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(manifests)
    }

    pub fn search(&self, query: &str) -> Result<Vec<SkillManifest>, SkillHubError> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.list_available();
        }

        let manifests = self.list_available()?;
        Ok(manifests
            .into_iter()
            .filter(|manifest| {
                manifest.name.to_lowercase().contains(&query)
                    || manifest.description.to_lowercase().contains(&query)
                    || manifest
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .collect())
    }

    pub fn install(&self, manifest: &SkillManifest) -> Result<SkillLock, SkillHubError> {
        match &manifest.source {
            SkillSource::Local => self.install_local(manifest),
            SkillSource::GitHub { owner, repo, path } => {
                self.install_github(manifest, owner, repo, path)
            }
            SkillSource::Registry { .. } => Err(SkillHubError::UnsupportedSource("registry")),
        }
    }

    pub fn uninstall(&self, name: &str) -> Result<(), SkillHubError> {
        let installed_path = self.installed_dir.join(name);
        if !installed_path.exists() {
            return Err(SkillHubError::NotFound(name.to_owned()));
        }

        fs::remove_dir_all(&installed_path)?;
        let mut locks = load_lock_file(&self.lock_path)?;
        locks.retain(|lock| lock.name != name);
        save_lock_file(&self.lock_path, &locks)?;
        Ok(())
    }

    fn install_local(&self, manifest: &SkillManifest) -> Result<SkillLock, SkillHubError> {
        let source_dir = self.find_local_skill_dir(&manifest.name)?;
        fs::create_dir_all(&self.installed_dir)?;

        let destination_dir = self.installed_dir.join(&manifest.name);
        if destination_dir.exists() {
            fs::remove_dir_all(&destination_dir)?;
        }
        copy_dir_recursive(&source_dir, &destination_dir)?;

        let lock = SkillLock {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source: manifest.source.clone(),
            installed_at: now_unix_timestamp(),
            checksum: checksum_dir(&destination_dir)?,
        };

        let mut locks = load_lock_file(&self.lock_path)?;
        locks.retain(|existing| existing.name != lock.name);
        locks.push(lock.clone());
        locks.sort_by(|left, right| left.name.cmp(&right.name));
        save_lock_file(&self.lock_path, &locks)?;

        Ok(lock)
    }

    fn install_github(
        &self,
        manifest: &SkillManifest,
        owner: &str,
        repo: &str,
        path: &str,
    ) -> Result<SkillLock, SkillHubError> {
        fs::create_dir_all(&self.installed_dir)?;
        let destination_dir = self.installed_dir.join(&manifest.name);
        if destination_dir.exists() {
            fs::remove_dir_all(&destination_dir)?;
        }
        fs::create_dir_all(&destination_dir)?;

        fetch_github_dir(owner, repo, path, &destination_dir)?;

        let lock = SkillLock {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source: manifest.source.clone(),
            installed_at: now_unix_timestamp(),
            checksum: checksum_dir(&destination_dir)?,
        };

        let mut locks = load_lock_file(&self.lock_path)?;
        locks.retain(|existing| existing.name != lock.name);
        locks.push(lock.clone());
        locks.sort_by(|left, right| left.name.cmp(&right.name));
        save_lock_file(&self.lock_path, &locks)?;

        Ok(lock)
    }

    fn find_local_skill_dir(&self, name: &str) -> Result<PathBuf, SkillHubError> {
        let entries = scan_skills_dir(&self.available_dir)
            .map_err(|err| SkillHubError::Parse(err.to_string()))?;
        for entry in entries {
            if entry.name == name {
                return entry
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| SkillHubError::NotFound(name.to_owned()));
            }
        }
        Err(SkillHubError::NotFound(name.to_owned()))
    }
}

pub fn load_lock_file(path: &Path) -> Result<Vec<SkillLock>, SkillHubError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&raw)?)
}

pub fn save_lock_file(path: &Path, locks: &[SkillLock]) -> Result<(), SkillHubError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(locks)?;
    fs::write(path, json)?;
    Ok(())
}

fn now_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), SkillHubError> {
    fs::create_dir_all(destination)?;
    let mut entries = fs::read_dir(source)?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

/// Fetch a directory from GitHub using the Contents API and write files to `dest`.
fn fetch_github_dir(
    owner: &str,
    repo: &str,
    path: &str,
    dest: &Path,
) -> Result<(), SkillHubError> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("genesis-skills-hub")
        .build()
        .map_err(|e| SkillHubError::Network(e.to_string()))?;

    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}");
    let mut request = client.get(&url);

    // Use GITHUB_TOKEN if available for rate limiting / private repos
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .map_err(|e| SkillHubError::Network(e.to_string()))?;

    if !response.status().is_success() {
        return Err(SkillHubError::Network(format!(
            "GitHub API returned {} for {}",
            response.status(),
            url,
        )));
    }

    let entries: Vec<GitHubContent> = response
        .json()
        .map_err(|e| SkillHubError::Network(format!("failed to parse GitHub response: {e}")))?;

    for entry in entries {
        let dest_path = dest.join(&entry.name);
        match entry.content_type.as_str() {
            "dir" => {
                fs::create_dir_all(&dest_path)?;
                let sub_path = format!("{path}/{}", entry.name);
                fetch_github_dir(owner, repo, &sub_path, &dest_path)?;
            }
            "file" => {
                let download_url = entry.download_url.ok_or_else(|| {
                    SkillHubError::Network(format!("no download_url for {}", entry.name))
                })?;
                let mut dl_request = client.get(&download_url);
                if let Ok(token) = std::env::var("GITHUB_TOKEN") {
                    dl_request = dl_request.bearer_auth(token);
                }
                let content = dl_request
                    .send()
                    .and_then(|r| r.bytes())
                    .map_err(|e| SkillHubError::Network(e.to_string()))?;
                fs::write(&dest_path, &content)?;
            }
            _ => {} // skip symlinks, submodules, etc.
        }
    }

    Ok(())
}

#[derive(Deserialize)]
struct GitHubContent {
    name: String,
    #[serde(rename = "type")]
    content_type: String,
    download_url: Option<String>,
}

fn checksum_dir(path: &Path) -> Result<String, SkillHubError> {
    let mut hasher = DefaultHasher::new();
    hash_path(path, path, &mut hasher)?;
    Ok(format!("{:016x}", hasher.finish()))
}

fn hash_path(root: &Path, path: &Path, hasher: &mut DefaultHasher) -> Result<(), SkillHubError> {
    if path.is_dir() {
        let mut entries = fs::read_dir(path)?
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            hash_path(root, &entry.path(), hasher)?;
        }
        return Ok(());
    }

    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    relative.hash(hasher);
    fs::read(path)?.hash(hasher);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = format!(
                "{}-{}",
                prefix,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_skill(dir: &Path, folder: &str, skill_md: &str, extra_file: Option<(&str, &str)>) {
        let skill_dir = dir.join(folder);
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(skill_dir.join("SKILL.md"), skill_md).expect("write SKILL.md");
        if let Some((name, content)) = extra_file {
            fs::write(skill_dir.join(name), content).expect("write extra file");
        }
    }

    fn sample_skill_md(name: &str, description: &str, tags: &[&str]) -> String {
        let tags_yaml = tags
            .iter()
            .map(|tag| format!("  - {tag}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "---\nname: {name}\ndescription: {description}\nversion: \"1.2.3\"\nauthor: Eve\nlicense: MIT\ntags:\n{tags_yaml}\n---\n\n# {name}\n\nFollow the instructions.\n"
        )
    }

    #[test]
    fn list_available_reads_local_manifests() {
        let available = TempDir::new("skills-hub-available");
        let installed = TempDir::new("skills-hub-installed");
        write_skill(
            available.path(),
            "code-review",
            &sample_skill_md("code-review", "Review code", &["dev", "quality"]),
            None,
        );

        let client = SkillHubClient::new(available.path(), installed.path());
        let manifests = client.list_available().expect("list should succeed");

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].name, "code-review");
        assert_eq!(manifests[0].description, "Review code");
        assert_eq!(manifests[0].version, "1.2.3");
        assert_eq!(manifests[0].author, "Eve");
        assert_eq!(manifests[0].license, "MIT");
        assert_eq!(manifests[0].tags, vec!["dev", "quality"]);
        assert_eq!(manifests[0].source, SkillSource::Local);
    }

    #[test]
    fn search_filters_by_name_description_and_tags() {
        let available = TempDir::new("skills-hub-search-available");
        let installed = TempDir::new("skills-hub-search-installed");
        write_skill(
            available.path(),
            "code-review",
            &sample_skill_md("code-review", "Review code", &["dev", "quality"]),
            None,
        );
        write_skill(
            available.path(),
            "deploy",
            &sample_skill_md("deploy", "Ship the app", &["ops"]),
            None,
        );

        let client = SkillHubClient::new(available.path(), installed.path());
        assert_eq!(client.search("review").unwrap().len(), 1);
        assert_eq!(client.search("ship").unwrap().len(), 1);
        assert_eq!(client.search("ops").unwrap().len(), 1);
        assert_eq!(client.search("missing").unwrap().len(), 0);
    }

    #[test]
    fn install_local_skill_copies_directory_and_updates_lock() {
        let available = TempDir::new("skills-hub-install-available");
        let installed = TempDir::new("skills-hub-install-installed");
        write_skill(
            available.path(),
            "code-review",
            &sample_skill_md("code-review", "Review code", &["dev"]),
            Some(("notes.txt", "hello")),
        );

        let client = SkillHubClient::new(available.path(), installed.path());
        let manifest = client.list_available().unwrap().remove(0);
        let lock = client.install(&manifest).expect("install should succeed");

        let installed_skill = installed.path().join("code-review");
        assert!(installed_skill.join("SKILL.md").exists());
        assert!(installed_skill.join("notes.txt").exists());
        assert_eq!(lock.name, "code-review");
        assert_eq!(lock.version, "1.2.3");
        assert_eq!(lock.source, SkillSource::Local);
        assert!(!lock.checksum.is_empty());

        let lock_file = installed.path().join("skills.lock.json");
        let locks = load_lock_file(&lock_file).expect("load lock file");
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0], lock);
    }

    #[test]
    fn uninstall_removes_installed_skill_and_updates_lock() {
        let available = TempDir::new("skills-hub-uninstall-available");
        let installed = TempDir::new("skills-hub-uninstall-installed");
        write_skill(
            available.path(),
            "deploy",
            &sample_skill_md("deploy", "Ship the app", &["ops"]),
            None,
        );

        let client = SkillHubClient::new(available.path(), installed.path());
        let manifest = client.list_available().unwrap().remove(0);
        client.install(&manifest).expect("install should succeed");

        client.uninstall("deploy").expect("uninstall should succeed");
        assert!(!installed.path().join("deploy").exists());
        assert!(load_lock_file(&installed.path().join("skills.lock.json"))
            .expect("load lock")
            .is_empty());
    }

    #[test]
    fn load_and_save_lock_file_round_trip() {
        let dir = TempDir::new("skills-hub-locks");
        let path = dir.path().join("skills.lock.json");
        let locks = vec![
            SkillLock {
                name: "alpha".to_owned(),
                version: "1.0.0".to_owned(),
                source: SkillSource::Local,
                installed_at: 123,
                checksum: "aaa".to_owned(),
            },
            SkillLock {
                name: "beta".to_owned(),
                version: "2.0.0".to_owned(),
                source: SkillSource::Registry {
                    url: "https://example.com".to_owned(),
                },
                installed_at: 456,
                checksum: "bbb".to_owned(),
            },
        ];

        save_lock_file(&path, &locks).expect("save lock file");
        let loaded = load_lock_file(&path).expect("load lock file");
        assert_eq!(loaded, locks);
    }

    #[test]
    fn install_rejects_registry_source() {
        let available = TempDir::new("skills-hub-registry-available");
        let installed = TempDir::new("skills-hub-registry-installed");
        let client = SkillHubClient::new(available.path(), installed.path());
        let manifest = SkillManifest {
            name: "remote".to_owned(),
            description: "Remote".to_owned(),
            version: "1.0.0".to_owned(),
            author: "Eve".to_owned(),
            license: "MIT".to_owned(),
            tags: vec!["remote".to_owned()],
            source: SkillSource::Registry {
                url: "https://example.com".to_owned(),
            },
        };

        let err = client.install(&manifest).expect_err("registry install should fail");
        assert!(matches!(err, SkillHubError::UnsupportedSource("registry")));
    }

    #[test]
    fn github_install_returns_network_error_for_nonexistent_repo() {
        let available = TempDir::new("skills-hub-gh-available");
        let installed = TempDir::new("skills-hub-gh-installed");
        let client = SkillHubClient::new(available.path(), installed.path());
        let manifest = SkillManifest {
            name: "nonexistent".to_owned(),
            description: "Does not exist".to_owned(),
            version: "1.0.0".to_owned(),
            author: "Eve".to_owned(),
            license: "MIT".to_owned(),
            tags: vec![],
            source: SkillSource::GitHub {
                owner: "this-owner-does-not-exist-12345".to_owned(),
                repo: "this-repo-does-not-exist-67890".to_owned(),
                path: "skills/nope".to_owned(),
            },
        };

        let err = client.install(&manifest).expect_err("should fail for nonexistent repo");
        assert!(matches!(err, SkillHubError::Network(_)));
    }
}
