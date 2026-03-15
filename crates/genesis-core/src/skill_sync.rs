//! Bundled skill synchronisation.
//!
//! Ships a set of *bundled* skills with the Genesis binary and keeps the
//! user-facing skills directory (`~/.genesis/skills/`) in sync using a
//! manifest file (`.bundled_manifest`) that tracks the SHA-256 content hash
//! of each bundled skill directory.
//!
//! Design goals:
//! - Never overwrite a skill the user has customised.
//! - Never re-add a skill the user deliberately deleted.
//! - Atomic manifest writes (temp file + rename).
//! - Backup-and-restore on updates.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Statistics returned after a sync run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Skills that were newly copied to the user directory.
    pub new_copied: usize,
    /// Skills that were updated (user hadn't modified the previous version).
    pub updated: usize,
    /// Skills that were already at the latest version and untouched.
    pub unchanged: usize,
    /// Skills the user had previously deleted -- we skipped re-adding them.
    pub deleted_skipped: usize,
    /// Skills the user had modified -- we preserved their version.
    pub user_modified_preserved: usize,
    /// Manifest entries cleaned because the skill was removed from bundled set.
    pub manifest_entries_cleaned: usize,
    /// Total number of skills in the bundled source directory.
    pub total_bundled: usize,
}

/// Errors that can occur during skill sync.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("skill sync IO error: {0}")]
    Io(#[from] io::Error),
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// On-disk manifest stored at `<user_skills_dir>/.bundled_manifest`.
///
/// Format: one line per entry, `skill_name<TAB>hex_hash`.
/// A tab separator is used to avoid conflicts with colons in skill names.
/// An entry with a hash of `DELETED` means the user explicitly deleted that
/// skill and we must not re-add it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledManifest {
    /// Maps skill name -> origin hash (SHA-256 hex) or "DELETED".
    entries: BTreeMap<String, String>,
    path: PathBuf,
}

const DELETED_MARKER: &str = "DELETED";

impl BundledManifest {
    /// Load (or create empty) manifest from the given path.
    pub fn load(path: &Path) -> Result<Self, SyncError> {
        let mut entries = BTreeMap::new();
        if path.exists() {
            let content = fs::read_to_string(path)?;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // Support both tab (current) and colon (legacy) separators.
                if let Some((name, hash)) = line.split_once('\t').or_else(|| line.split_once(':')) {
                    entries.insert(name.to_owned(), hash.to_owned());
                }
            }
        }
        Ok(Self {
            entries,
            path: path.to_owned(),
        })
    }

    /// Write manifest atomically (temp file + rename).
    pub fn save(&self) -> Result<(), SyncError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        let mut content = String::new();
        for (name, hash) in &self.entries {
            content.push_str(name);
            content.push('\t');
            content.push_str(hash);
            content.push('\n');
        }
        fs::write(&tmp, &content)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Get the recorded hash for a skill, if any.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|s| s.as_str())
    }

    /// Record a hash for a skill.
    pub fn set(&mut self, name: impl Into<String>, hash: impl Into<String>) {
        self.entries.insert(name.into(), hash.into());
    }

    /// Mark a skill as user-deleted so we never re-add it.
    pub fn mark_deleted(&mut self, name: impl Into<String>) {
        self.entries.insert(name.into(), DELETED_MARKER.to_owned());
    }

    /// Remove an entry (used when the skill is removed from bundled set).
    pub fn remove(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// Check whether a skill is marked as user-deleted.
    pub fn is_deleted(&self, name: &str) -> bool {
        self.entries.get(name).map(|v| v == DELETED_MARKER).unwrap_or(false)
    }

    /// Iterator over all entry names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Directory hashing (SHA-256)
// ---------------------------------------------------------------------------

/// Compute a deterministic SHA-256 hash of a directory's contents.
///
/// Hashes every file's relative path (sorted) and content. This means
/// any change to file names, paths, or contents produces a different hash.
pub fn dir_hash(root: &Path) -> Result<String, SyncError> {
    let mut hasher = Sha256::new();
    let mut file_entries = collect_files(root, root)?;
    // Sort by relative path for determinism.
    file_entries.sort();
    for (rel_path, full_path) in &file_entries {
        hasher.update(rel_path.as_bytes());
        hasher.update(b"\0");
        let content = fs::read(full_path)?;
        hasher.update(&content);
        hasher.update(b"\0");
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Recursively collect `(relative_path_string, absolute_path)` pairs.
fn collect_files(root: &Path, current: &Path) -> Result<Vec<(String, PathBuf)>, SyncError> {
    let mut result = Vec::new();
    if !current.is_dir() {
        return Ok(result);
    }
    let mut entries: Vec<_> = fs::read_dir(current)?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_files(root, &path)?);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            result.push((rel, path));
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

/// Synchronise bundled skills into the user skills directory.
///
/// `bundled_dir` is the source directory shipped with the binary (read-only).
/// Each subdirectory is a skill (must contain a `SKILL.md`).
///
/// `user_dir` is the writable user skills directory (e.g. `~/.genesis/skills/`).
///
/// Returns statistics about what happened.
pub fn sync_bundled_skills(bundled_dir: &Path, user_dir: &Path) -> Result<SyncStats, SyncError> {
    fs::create_dir_all(user_dir)?;

    let manifest_path = user_dir.join(".bundled_manifest");
    let mut manifest = BundledManifest::load(&manifest_path)?;
    let mut stats = SyncStats::default();

    // Discover bundled skills (subdirectories with a SKILL.md).
    let bundled_skills = list_skill_dirs(bundled_dir)?;
    stats.total_bundled = bundled_skills.len();

    // Track which bundled names we see, so we can clean stale manifest entries.
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (name, bundled_path) in &bundled_skills {
        seen_names.insert(name.clone());
        let bundled_hash = dir_hash(bundled_path)?;
        let user_skill_dir = user_dir.join(name);

        // --- Scenario: user deleted this skill previously ---
        if manifest.is_deleted(name) {
            // Respect the deletion. Don't re-add.
            stats.deleted_skipped += 1;
            continue;
        }

        let manifest_entry = manifest.get(name).map(|s| s.to_owned());

        if !user_skill_dir.exists() {
            match &manifest_entry {
                None => {
                    // New skill, user dir empty -> copy and record.
                    copy_dir_recursive(bundled_path, &user_skill_dir)?;
                    manifest.set(name, &bundled_hash);
                    stats.new_copied += 1;
                }
                Some(_) => {
                    // We previously synced this skill but the user removed it.
                    // Respect the deletion.
                    manifest.mark_deleted(name);
                    stats.deleted_skipped += 1;
                }
            }
            continue;
        }

        // User directory exists.
        match &manifest_entry {
            None => {
                // New bundled skill but user already has a directory with the
                // same name. Skip to avoid overwriting user content.
                manifest.set(name, &bundled_hash);
                stats.user_modified_preserved += 1;
            }
            Some(prev_hash) => {
                // We've seen this skill before.
                if prev_hash == &bundled_hash {
                    // Bundled version hasn't changed.
                    // Check if user modified their copy.
                    let user_hash = dir_hash(&user_skill_dir)?;
                    if user_hash == bundled_hash {
                        stats.unchanged += 1;
                    } else {
                        stats.user_modified_preserved += 1;
                    }
                } else {
                    // Bundled version changed. Check if user modified
                    // their copy from what we last installed.
                    let user_hash = dir_hash(&user_skill_dir)?;
                    if user_hash == *prev_hash {
                        // User hasn't modified -> safe to update.
                        let backup_dir = user_dir.join(".bak");
                        fs::create_dir_all(&backup_dir)?;
                        let backup_skill = backup_dir.join(name);
                        if backup_skill.exists() {
                            fs::remove_dir_all(&backup_skill)?;
                        }
                        // Backup current version.
                        copy_dir_recursive(&user_skill_dir, &backup_skill)?;
                        // Replace with new bundled version.
                        fs::remove_dir_all(&user_skill_dir)?;
                        if let Err(e) = copy_dir_recursive(bundled_path, &user_skill_dir) {
                            // Rollback: restore from backup so the skill isn't lost.
                            let _ = fs::remove_dir_all(&user_skill_dir);
                            copy_dir_recursive(&backup_skill, &user_skill_dir)?;
                            return Err(e);
                        }
                        manifest.set(name, &bundled_hash);
                        stats.updated += 1;
                    } else {
                        // User customised -> preserve their version.
                        // Update the manifest hash to the new bundled hash so we
                        // don't keep trying to update on every sync.
                        manifest.set(name, &bundled_hash);
                        stats.user_modified_preserved += 1;
                    }
                }
            }
        }
    }

    // Clean manifest entries for skills removed from bundled set.
    let stale: Vec<String> = manifest
        .names()
        .filter(|n| !seen_names.contains(*n))
        .map(|n| n.to_owned())
        .collect();
    for name in &stale {
        manifest.remove(name);
        stats.manifest_entries_cleaned += 1;
    }

    manifest.save()?;
    Ok(stats)
}

/// List skill subdirectories (those containing a `SKILL.md`).
fn list_skill_dirs(dir: &Path) -> Result<Vec<(String, PathBuf)>, SyncError> {
    let mut result = Vec::new();
    if !dir.exists() {
        return Ok(result);
    }
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").exists() {
            let name = entry
                .file_name()
                .to_string_lossy()
                .to_string();
            result.push((name, path));
        }
    }
    Ok(result)
}

/// Copy a directory tree recursively.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), SyncError> {
    fs::create_dir_all(dst)?;
    let mut entries: Vec<_> = fs::read_dir(src)?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Helper: write a minimal SKILL.md and optional extra files into a directory.
    fn write_skill(base: &Path, name: &str, body: &str, extras: &[(&str, &str)]) {
        let dir = base.join(name);
        fs::create_dir_all(&dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: A test skill\nversion: \"1.0\"\nauthor: test\nlicense: MIT\ntags:\n  - test\n---\n\n{body}\n"
        );
        fs::write(dir.join("SKILL.md"), content).unwrap();
        for (fname, fdata) in extras {
            fs::write(dir.join(fname), fdata).unwrap();
        }
    }

    // -- Manifest tests -------------------------------------------------------

    #[test]
    fn manifest_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");

        let mut m = BundledManifest::load(&path).unwrap();
        assert!(m.get("alpha").is_none());

        m.set("alpha", "aaa111");
        m.set("beta", "bbb222");
        m.mark_deleted("gamma");
        m.save().unwrap();

        let m2 = BundledManifest::load(&path).unwrap();
        assert_eq!(m2.get("alpha"), Some("aaa111"));
        assert_eq!(m2.get("beta"), Some("bbb222"));
        assert!(m2.is_deleted("gamma"));
        assert!(!m2.is_deleted("alpha"));
    }

    #[test]
    fn manifest_load_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");
        fs::write(&path, "").unwrap();

        let m = BundledManifest::load(&path).unwrap();
        assert!(m.get("anything").is_none());
    }

    #[test]
    fn manifest_load_with_comments_and_blanks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");
        fs::write(&path, "# comment\n\nalpha:abc123\n\n# another comment\nbeta:def456\n").unwrap();

        let m = BundledManifest::load(&path).unwrap();
        assert_eq!(m.get("alpha"), Some("abc123"));
        assert_eq!(m.get("beta"), Some("def456"));
    }

    #[test]
    fn manifest_remove_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");

        let mut m = BundledManifest::load(&path).unwrap();
        m.set("alpha", "aaa");
        m.set("beta", "bbb");
        m.remove("alpha");
        m.save().unwrap();

        let m2 = BundledManifest::load(&path).unwrap();
        assert!(m2.get("alpha").is_none());
        assert_eq!(m2.get("beta"), Some("bbb"));
    }

    #[test]
    fn manifest_atomic_write_uses_rename() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");

        let mut m = BundledManifest::load(&path).unwrap();
        m.set("skill", "hash");
        m.save().unwrap();

        // The temp file should not linger.
        let tmp = path.with_extension("tmp");
        assert!(!tmp.exists());
        // The actual file should exist.
        assert!(path.exists());
    }

    #[test]
    fn manifest_uses_tab_separator() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");

        let mut m = BundledManifest::load(&path).unwrap();
        m.set("alpha", "aaa111");
        m.save().unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("alpha\taaa111"), "manifest should use tab separator");
        assert!(!raw.contains("alpha:aaa111"), "manifest should not use colon separator");
    }

    #[test]
    fn manifest_handles_colon_in_skill_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");

        let mut m = BundledManifest::load(&path).unwrap();
        m.set("my:skill:name", "abc123");
        m.save().unwrap();

        let m2 = BundledManifest::load(&path).unwrap();
        assert_eq!(m2.get("my:skill:name"), Some("abc123"));
    }

    #[test]
    fn manifest_reads_legacy_colon_format() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".bundled_manifest");
        // Simulate a legacy manifest with colon separators (no colons in names).
        fs::write(&path, "alpha:abc123\nbeta:def456\n").unwrap();

        let m = BundledManifest::load(&path).unwrap();
        assert_eq!(m.get("alpha"), Some("abc123"));
        assert_eq!(m.get("beta"), Some("def456"));
    }

    // -- dir_hash tests -------------------------------------------------------

    #[test]
    fn dir_hash_deterministic() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "test-skill", "Do the thing", &[("notes.txt", "hello")]);

        let skill_dir = dir.path().join("test-skill");
        let h1 = dir_hash(&skill_dir).unwrap();
        let h2 = dir_hash(&skill_dir).unwrap();
        assert_eq!(h1, h2);
        // SHA-256 hex is 64 characters.
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn dir_hash_changes_on_content_change() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "skill", "version 1", &[]);

        let skill_dir = dir.path().join("skill");
        let h1 = dir_hash(&skill_dir).unwrap();

        // Modify the SKILL.md.
        fs::write(skill_dir.join("SKILL.md"), "---\nname: skill\ndescription: changed\nversion: \"2.0\"\nauthor: test\nlicense: MIT\ntags:\n  - test\n---\n\nversion 2\n").unwrap();
        let h2 = dir_hash(&skill_dir).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn dir_hash_changes_on_file_add() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "skill", "body", &[]);

        let skill_dir = dir.path().join("skill");
        let h1 = dir_hash(&skill_dir).unwrap();

        fs::write(skill_dir.join("extra.txt"), "extra data").unwrap();
        let h2 = dir_hash(&skill_dir).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn dir_hash_changes_on_filename_change() {
        let d1 = tempdir().unwrap();
        let d2 = tempdir().unwrap();
        let dir1 = d1.path().join("s");
        let dir2 = d2.path().join("s");
        fs::create_dir_all(&dir1).unwrap();
        fs::create_dir_all(&dir2).unwrap();

        fs::write(dir1.join("a.txt"), "same content").unwrap();
        fs::write(dir2.join("b.txt"), "same content").unwrap();

        let h1 = dir_hash(&dir1).unwrap();
        let h2 = dir_hash(&dir2).unwrap();
        assert_ne!(h1, h2);
    }

    // -- sync_bundled_skills tests --------------------------------------------

    #[test]
    fn sync_new_skill_copied_to_empty_user_dir() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "greet", "Say hello", &[]);

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();

        assert_eq!(stats.new_copied, 1);
        assert_eq!(stats.total_bundled, 1);
        assert!(user.path().join("greet/SKILL.md").exists());

        // Manifest should exist.
        let manifest = BundledManifest::load(&user.path().join(".bundled_manifest")).unwrap();
        assert!(manifest.get("greet").is_some());
    }

    #[test]
    fn sync_new_skill_skips_existing_user_dir() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "deploy", "Run deploy", &[]);
        // User already has a "deploy" skill (independently created).
        write_skill(user.path(), "deploy", "My custom deploy", &[]);

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();

        assert_eq!(stats.new_copied, 0);
        assert_eq!(stats.user_modified_preserved, 1);

        // User's custom content should be preserved.
        let content = fs::read_to_string(user.path().join("deploy/SKILL.md")).unwrap();
        assert!(content.contains("My custom deploy"));
    }

    #[test]
    fn sync_unchanged_skill_stays_unchanged() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "greet", "Say hello", &[]);

        // First sync: copies the skill.
        let stats1 = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats1.new_copied, 1);

        // Second sync: nothing changed.
        let stats2 = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats2.unchanged, 1);
        assert_eq!(stats2.new_copied, 0);
        assert_eq!(stats2.updated, 0);
    }

    #[test]
    fn sync_updates_unmodified_user_copy_when_bundled_changes() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "greet", "Say hello v1", &[]);

        // First sync.
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        // Update the bundled skill.
        write_skill(bundled.path(), "greet", "Say hello v2", &[]);

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.updated, 1);

        // User dir should have the new version.
        let content = fs::read_to_string(user.path().join("greet/SKILL.md")).unwrap();
        assert!(content.contains("Say hello v2"));

        // Backup should exist.
        let backup = user.path().join(".bak/greet/SKILL.md");
        assert!(backup.exists());
        let backup_content = fs::read_to_string(backup).unwrap();
        assert!(backup_content.contains("Say hello v1"));
    }

    #[test]
    fn sync_preserves_user_modified_skill_on_bundled_change() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "greet", "Say hello v1", &[]);

        // First sync.
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        // User modifies their copy.
        fs::write(
            user.path().join("greet/SKILL.md"),
            "---\nname: greet\ndescription: customised\nversion: \"1.0\"\nauthor: test\nlicense: MIT\ntags:\n  - test\n---\n\nMy custom greeting\n",
        )
        .unwrap();

        // Update the bundled skill.
        write_skill(bundled.path(), "greet", "Say hello v2", &[]);

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.user_modified_preserved, 1);
        assert_eq!(stats.updated, 0);

        // User's custom content is still there.
        let content = fs::read_to_string(user.path().join("greet/SKILL.md")).unwrap();
        assert!(content.contains("My custom greeting"));
    }

    #[test]
    fn sync_respects_user_deletion() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "greet", "Say hello", &[]);

        // First sync.
        sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert!(user.path().join("greet/SKILL.md").exists());

        // User deletes the skill directory.
        fs::remove_dir_all(user.path().join("greet")).unwrap();

        // Second sync: should detect the deletion and mark it.
        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.new_copied, 0);
        assert_eq!(stats.deleted_skipped, 1);

        // Third sync: still should not re-add (now uses the DELETED marker path).
        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.new_copied, 0);
        assert_eq!(stats.deleted_skipped, 1);
        assert!(!user.path().join("greet").exists());

        // Manifest should have DELETED marker.
        let manifest = BundledManifest::load(&user.path().join(".bundled_manifest")).unwrap();
        assert!(manifest.is_deleted("greet"));
    }

    #[test]
    fn sync_cleans_manifest_for_removed_bundled_skill() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "alpha", "Alpha skill", &[]);
        write_skill(bundled.path(), "beta", "Beta skill", &[]);

        // First sync: copies both.
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        // Remove "beta" from bundled.
        fs::remove_dir_all(bundled.path().join("beta")).unwrap();

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.manifest_entries_cleaned, 1);
        assert_eq!(stats.total_bundled, 1);

        // Manifest should no longer have "beta".
        let manifest = BundledManifest::load(&user.path().join(".bundled_manifest")).unwrap();
        assert!(manifest.get("beta").is_none());
    }

    #[test]
    fn sync_preserves_category_structure() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        // Create a skill with subdirectory structure.
        let skill_dir = bundled.path().join("complex");
        fs::create_dir_all(skill_dir.join("templates")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: complex\ndescription: Complex skill\nversion: \"1.0\"\nauthor: test\nlicense: MIT\ntags: []\n---\n\nComplex instructions\n",
        )
        .unwrap();
        fs::write(skill_dir.join("templates/greeting.txt"), "Hello {{name}}!").unwrap();
        fs::write(skill_dir.join("config.yaml"), "key: value").unwrap();

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.new_copied, 1);

        // Verify structure is preserved.
        assert!(user.path().join("complex/SKILL.md").exists());
        assert!(user.path().join("complex/templates/greeting.txt").exists());
        assert!(user.path().join("complex/config.yaml").exists());

        let template = fs::read_to_string(user.path().join("complex/templates/greeting.txt")).unwrap();
        assert_eq!(template, "Hello {{name}}!");
    }

    #[test]
    fn sync_multiple_skills_produces_correct_stats() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "new1", "New skill 1", &[]);
        write_skill(bundled.path(), "new2", "New skill 2", &[]);
        write_skill(bundled.path(), "existing", "Existing", &[]);

        // Pre-populate user with a custom "existing" skill.
        write_skill(user.path(), "existing", "My custom version", &[]);

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.total_bundled, 3);
        assert_eq!(stats.new_copied, 2);
        assert_eq!(stats.user_modified_preserved, 1);
        assert_eq!(stats.updated, 0);
        assert_eq!(stats.unchanged, 0);
        assert_eq!(stats.deleted_skipped, 0);
    }

    #[test]
    fn sync_empty_bundled_dir_cleans_manifest() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "alpha", "Alpha", &[]);
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        // Remove all bundled skills.
        fs::remove_dir_all(bundled.path().join("alpha")).unwrap();

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.total_bundled, 0);
        assert_eq!(stats.manifest_entries_cleaned, 1);
    }

    #[test]
    fn sync_nonexistent_bundled_dir_returns_empty_stats() {
        let user = tempdir().unwrap();
        let stats = sync_bundled_skills(Path::new("/nonexistent/bundled"), user.path()).unwrap();
        assert_eq!(stats.total_bundled, 0);
        assert_eq!(stats, SyncStats::default());
    }

    #[test]
    fn sync_creates_user_dir_if_missing() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();
        let nested_user = user.path().join("deep/nested/skills");

        write_skill(bundled.path(), "greet", "Hello", &[]);

        let stats = sync_bundled_skills(bundled.path(), &nested_user).unwrap();
        assert_eq!(stats.new_copied, 1);
        assert!(nested_user.join("greet/SKILL.md").exists());
    }

    #[test]
    fn sync_backup_is_overwritten_on_subsequent_updates() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "greet", "v1", &[]);
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        // Update bundled to v2.
        write_skill(bundled.path(), "greet", "v2", &[]);
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        let backup1 = fs::read_to_string(user.path().join(".bak/greet/SKILL.md")).unwrap();
        assert!(backup1.contains("v1"));

        // Update bundled to v3.
        write_skill(bundled.path(), "greet", "v3", &[]);
        sync_bundled_skills(bundled.path(), user.path()).unwrap();

        let backup2 = fs::read_to_string(user.path().join(".bak/greet/SKILL.md")).unwrap();
        assert!(backup2.contains("v2"));

        let current = fs::read_to_string(user.path().join("greet/SKILL.md")).unwrap();
        assert!(current.contains("v3"));
    }

    #[test]
    fn sync_ignores_non_skill_subdirectories() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(bundled.path(), "valid", "A real skill", &[]);

        // Create a directory without SKILL.md.
        fs::create_dir_all(bundled.path().join("not-a-skill")).unwrap();
        fs::write(bundled.path().join("not-a-skill/README.md"), "not a skill").unwrap();

        // Create a plain file (not a directory).
        fs::write(bundled.path().join("random.txt"), "just a file").unwrap();

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.total_bundled, 1);
        assert_eq!(stats.new_copied, 1);
        assert!(!user.path().join("not-a-skill").exists());
        assert!(!user.path().join("random.txt").exists());
    }

    #[test]
    fn sync_with_extra_files_in_skill() {
        let bundled = tempdir().unwrap();
        let user = tempdir().unwrap();

        write_skill(
            bundled.path(),
            "rich-skill",
            "Skill with extras",
            &[("helper.py", "print('hello')"), ("data.json", "{\"key\": 1}")],
        );

        let stats = sync_bundled_skills(bundled.path(), user.path()).unwrap();
        assert_eq!(stats.new_copied, 1);

        assert!(user.path().join("rich-skill/SKILL.md").exists());
        assert!(user.path().join("rich-skill/helper.py").exists());
        assert!(user.path().join("rich-skill/data.json").exists());

        let py = fs::read_to_string(user.path().join("rich-skill/helper.py")).unwrap();
        assert_eq!(py, "print('hello')");
    }
}
