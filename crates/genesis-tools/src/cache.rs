use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use notify::{Event, RecursiveMode, Watcher};
use tracing::{debug, warn};

/// Cache policy for a tool — determines whether its results can be cached.
#[derive(Debug, Clone, Default)]
pub enum CachePolicy {
    /// Cache results for the given duration.
    Cacheable(Duration),
    /// Never cache results.
    #[default]
    NotCacheable,
    /// Not cached, but successful execution invalidates cache entries for the
    /// path extracted from the tool call's arguments.
    Invalidates,
}

/// Key for a cached tool result: tool name + full argument map.
///
/// Uses the full argument map for equality to avoid hash-collision risks.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    tool_name: String,
    arguments: BTreeMap<String, String>,
}

impl CacheKey {
    fn new(tool_name: &str, arguments: &BTreeMap<String, String>) -> Self {
        CacheKey {
            tool_name: tool_name.to_owned(),
            arguments: arguments.clone(),
        }
    }
}

/// A cached tool result with expiry and path tracking.
struct CachedEntry {
    content: String,
    metadata: BTreeMap<String, String>,
    expires_at: Instant,
    /// The primary filesystem path this entry depends on (if any).
    tracked_path: Option<PathBuf>,
}

/// Cache statistics.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub invalidations: u64,
    pub evictions: u64,
}

impl std::fmt::Display for CacheStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total = self.hits + self.misses;
        let hit_rate = if total > 0 {
            (self.hits as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        write!(
            f,
            "cache: {}/{} hits ({:.0}%), {} invalidations, {} evictions",
            self.hits, total, hit_rate, self.invalidations, self.evictions
        )
    }
}

struct CacheInner {
    entries: HashMap<CacheKey, CachedEntry>,
    /// Reverse index: canonical path → set of cache keys that depend on it.
    path_index: HashMap<PathBuf, HashSet<CacheKey>>,
    stats: CacheStats,
}

impl CacheInner {
    fn new() -> Self {
        CacheInner {
            entries: HashMap::new(),
            path_index: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    /// Check if a cached entry exists and is not expired (read-only).
    fn peek(&self, key: &CacheKey) -> Option<(String, BTreeMap<String, String>)> {
        if let Some(entry) = self.entries.get(key) {
            if Instant::now() < entry.expires_at {
                return Some((entry.content.clone(), entry.metadata.clone()));
            }
        }
        None
    }

    /// Get a cached entry, evicting it if expired. Requires write access.
    fn get_or_evict(
        &mut self,
        key: &CacheKey,
    ) -> Option<(String, BTreeMap<String, String>)> {
        if let Some(entry) = self.entries.get(key) {
            if Instant::now() < entry.expires_at {
                self.stats.hits += 1;
                return Some((entry.content.clone(), entry.metadata.clone()));
            }
            // Expired — remove it.
            let entry = self.entries.remove(key).unwrap();
            if let Some(ref path) = entry.tracked_path {
                if let Some(keys) = self.path_index.get_mut(path) {
                    keys.remove(key);
                    if keys.is_empty() {
                        self.path_index.remove(path);
                    }
                }
            }
            self.stats.evictions += 1;
        }
        self.stats.misses += 1;
        None
    }

    fn insert(
        &mut self,
        key: CacheKey,
        content: String,
        metadata: BTreeMap<String, String>,
        ttl: Duration,
        tracked_path: Option<PathBuf>,
    ) {
        let canonical = tracked_path.and_then(|p| std::fs::canonicalize(&p).ok());

        if let Some(ref path) = canonical {
            self.path_index
                .entry(path.clone())
                .or_default()
                .insert(key.clone());
        }

        self.entries.insert(
            key,
            CachedEntry {
                content,
                metadata,
                expires_at: Instant::now() + ttl,
                tracked_path: canonical,
            },
        );
    }

    /// Invalidate cache entries for the given changed path.
    ///
    /// - Exact match: entries whose tracked path == changed path
    /// - Child-of: entries whose tracked path is a descendant of a changed directory
    ///   (e.g., file write invalidates parent directory's list_dir)
    ///
    /// Note: we intentionally do NOT do `canonical.starts_with(tracked)` (ancestor
    /// invalidation) because `list_dir` only lists immediate children — a deep
    /// nested file change shouldn't invalidate an ancestor `list_dir` cache.
    /// The exception is when the changed file is a direct child of the tracked dir.
    fn invalidate_path(&mut self, changed_path: &Path) {
        let canonical = match std::fs::canonicalize(changed_path) {
            Ok(p) => p,
            Err(_) => changed_path.to_path_buf(),
        };

        let mut keys_to_remove = Vec::new();

        for (tracked, keys) in &self.path_index {
            // Exact match: file read cache for the changed file.
            if tracked == &canonical {
                keys_to_remove.extend(keys.iter().cloned());
                continue;
            }
            // tracked is a descendant of (or lives under) the changed path.
            // e.g., tracked="/workspace/src/foo.rs", changed="/workspace/src"
            // This handles directory-level changes invalidating file caches underneath.
            if tracked.starts_with(&canonical) {
                keys_to_remove.extend(keys.iter().cloned());
                continue;
            }
            // Changed path is a direct child of the tracked directory.
            // e.g., tracked="/workspace/src", changed="/workspace/src/new_file.rs"
            // This handles list_dir invalidation when a file is added/removed.
            if canonical.parent() == Some(tracked.as_path()) {
                keys_to_remove.extend(keys.iter().cloned());
            }
        }

        for key in &keys_to_remove {
            if let Some(entry) = self.entries.remove(key) {
                self.stats.invalidations += 1;
                if let Some(ref path) = entry.tracked_path {
                    if let Some(keys) = self.path_index.get_mut(path) {
                        keys.remove(key);
                        if keys.is_empty() {
                            self.path_index.remove(path);
                        }
                    }
                }
            }
        }

        if !keys_to_remove.is_empty() {
            debug!(
                path = %canonical.display(),
                count = keys_to_remove.len(),
                "invalidated cache entries for path"
            );
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.path_index.clear();
    }
}

/// Thread-safe tool result cache with TTL and filesystem-event invalidation.
#[derive(Clone)]
pub struct ToolCache {
    inner: Arc<RwLock<CacheInner>>,
    /// Kept alive to maintain the FS watcher background thread.
    _watcher_handle: Arc<RwLock<Option<WatcherHandle>>>,
}

/// Opaque handle that keeps the FS watcher alive.
struct WatcherHandle {
    _watcher: notify::RecommendedWatcher,
}

/// Directories to exclude from recursive FS watching.
const WATCH_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".next",
    ".nuxt",
];

impl ToolCache {
    pub fn new() -> Self {
        ToolCache {
            inner: Arc::new(RwLock::new(CacheInner::new())),
            _watcher_handle: Arc::new(RwLock::new(None)),
        }
    }

    /// Start watching a directory for filesystem changes. Changes invalidate
    /// cache entries whose tracked path is affected.
    pub fn start_watching(&self, dir: &Path) {
        let inner = Arc::clone(&self.inner);

        // Debounce: track last-invalidated paths to avoid thundering herd.
        // Bounded: prune entries older than the debounce window.
        let last_events: Arc<RwLock<HashMap<PathBuf, Instant>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let debounce_window = Duration::from_millis(200);
        let prune_threshold = Duration::from_secs(10);

        let watcher = notify::recommended_watcher(
            move |result: Result<Event, notify::Error>| match result {
                Ok(event) => {
                    // Only care about modification-like events.
                    use notify::EventKind;
                    match event.kind {
                        EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_) => {}
                        _ => return,
                    }

                    let now = Instant::now();
                    let mut cache = match inner.write() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let mut last = match last_events.write() {
                        Ok(l) => l,
                        Err(_) => return,
                    };

                    // Prune stale debounce entries periodically.
                    if last.len() > 1000 {
                        last.retain(|_, &mut t| now.duration_since(t) < prune_threshold);
                    }

                    for path in &event.paths {
                        // Skip events in noise directories.
                        if WATCH_EXCLUDE_DIRS
                            .iter()
                            .any(|d| path.components().any(|c| c.as_os_str() == *d))
                        {
                            continue;
                        }

                        // Simple debounce: skip if we processed this path very recently.
                        if let Some(&last_time) = last.get(path) {
                            if now.duration_since(last_time) < debounce_window {
                                continue;
                            }
                        }
                        last.insert(path.clone(), now);
                        cache.invalidate_path(path);
                    }
                }
                Err(e) => {
                    warn!(error = %e, "filesystem watcher error");
                }
            },
        );

        match watcher {
            Ok(mut watcher) => {
                if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
                    warn!(
                        dir = %dir.display(),
                        error = %e,
                        "failed to start filesystem watcher"
                    );
                    return;
                }
                debug!(dir = %dir.display(), "started filesystem watcher for tool cache");
                if let Ok(mut handle) = self._watcher_handle.write() {
                    *handle = Some(WatcherHandle { _watcher: watcher });
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to create filesystem watcher");
            }
        }
    }

    /// Look up a cached result. Returns `Some((content, metadata))` on hit.
    ///
    /// Uses a read lock first to check for a valid entry (fast path), falling
    /// back to a write lock only when eviction of an expired entry is needed.
    pub fn get(
        &self,
        tool_name: &str,
        arguments: &BTreeMap<String, String>,
    ) -> Option<(String, BTreeMap<String, String>)> {
        let key = CacheKey::new(tool_name, arguments);

        // Fast path: read lock to check for valid (non-expired) entry.
        if let Ok(inner) = self.inner.read() {
            if let Some(result) = inner.peek(&key) {
                // Need write lock to bump stats, but the data is valid.
                drop(inner);
                if let Ok(mut w) = self.inner.write() {
                    w.stats.hits += 1;
                }
                return Some(result);
            }
        }

        // Slow path: write lock to handle eviction of expired entries.
        self.inner.write().ok()?.get_or_evict(&key)
    }

    /// Store a tool result in the cache.
    pub fn insert(
        &self,
        tool_name: &str,
        arguments: &BTreeMap<String, String>,
        content: String,
        metadata: BTreeMap<String, String>,
        ttl: Duration,
        tracked_path: Option<PathBuf>,
    ) {
        let key = CacheKey::new(tool_name, arguments);
        if let Ok(mut inner) = self.inner.write() {
            inner.insert(key, content, metadata, ttl, tracked_path);
        }
    }

    /// Invalidate all cache entries whose tracked path overlaps with the given path.
    pub fn invalidate_path(&self, path: &Path) {
        if let Ok(mut inner) = self.inner.write() {
            inner.invalidate_path(path);
        }
    }

    /// Return a snapshot of cache statistics.
    pub fn stats(&self) -> CacheStats {
        self.inner
            .read()
            .map(|inner| inner.stats.clone())
            .unwrap_or_default()
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.clear();
        }
    }
}

impl Default for ToolCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the primary filesystem path from a tool call's arguments.
/// Returns `None` for tools that don't operate on specific paths.
pub fn extract_tracked_path(
    tool_name: &str,
    arguments: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    match tool_name {
        "read_file" | "list_dir" | "list_tree" | "write_file" | "patch" => {
            arguments.get("path").map(PathBuf::from)
        }
        "search_files" => arguments
            .get("path")
            .map(PathBuf::from)
            .or(Some(PathBuf::from("."))),
        "glob" => {
            // Extract base directory from glob pattern.
            arguments.get("pattern").map(|pattern| {
                // Take the longest non-glob prefix as the base dir.
                let base = pattern
                    .split(['*', '?', '['])
                    .next()
                    .unwrap_or(".");
                let path = Path::new(base);
                if path.is_dir() {
                    path.to_path_buf()
                } else {
                    path.parent().unwrap_or(Path::new(".")).to_path_buf()
                }
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_returns_stored_result() {
        let cache = ToolCache::new();
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "/tmp/test.txt".to_owned());

        cache.insert(
            "read_file",
            &args,
            "file contents".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(30),
            None,
        );

        let result = cache.get("read_file", &args);
        assert!(result.is_some());
        let (content, _) = result.unwrap();
        assert_eq!(content, "file contents");

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn cache_miss_for_unknown_key() {
        let cache = ToolCache::new();
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "/tmp/test.txt".to_owned());

        let result = cache.get("read_file", &args);
        assert!(result.is_none());

        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn cache_miss_after_ttl_expiry() {
        let cache = ToolCache::new();
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "/tmp/test.txt".to_owned());

        // Insert with zero TTL so it expires immediately.
        cache.insert(
            "read_file",
            &args,
            "old contents".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(0),
            None,
        );

        // Small sleep to ensure expiry.
        std::thread::sleep(Duration::from_millis(5));

        let result = cache.get("read_file", &args);
        assert!(result.is_none());

        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.evictions, 1);
    }

    #[test]
    fn different_args_produce_cache_miss() {
        let cache = ToolCache::new();

        let mut args1 = BTreeMap::new();
        args1.insert("path".to_owned(), "/tmp/a.txt".to_owned());
        let mut args2 = BTreeMap::new();
        args2.insert("path".to_owned(), "/tmp/b.txt".to_owned());

        cache.insert(
            "read_file",
            &args1,
            "contents a".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(30),
            None,
        );

        assert!(cache.get("read_file", &args1).is_some());
        assert!(cache.get("read_file", &args2).is_none());
    }

    #[test]
    fn invalidate_path_removes_matching_entries() {
        let cache = ToolCache::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), file_path.to_string_lossy().to_string());

        cache.insert(
            "read_file",
            &args,
            "hello".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(60),
            Some(file_path.clone()),
        );

        assert!(cache.get("read_file", &args).is_some());

        cache.invalidate_path(&file_path);

        assert!(cache.get("read_file", &args).is_none());

        let stats = cache.stats();
        assert_eq!(stats.invalidations, 1);
    }

    #[test]
    fn write_invalidates_list_dir_for_direct_parent() {
        let cache = ToolCache::new();
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let file_path = sub_dir.join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();

        // Cache a list_dir for the parent.
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), sub_dir.to_string_lossy().to_string());

        cache.insert(
            "list_dir",
            &args,
            "test.txt".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(60),
            Some(sub_dir.clone()),
        );

        assert!(cache.get("list_dir", &args).is_some());

        // Invalidate via a direct child file write.
        cache.invalidate_path(&file_path);

        assert!(cache.get("list_dir", &args).is_none());
    }

    #[test]
    fn deep_nested_write_does_not_invalidate_ancestor_list_dir() {
        let cache = ToolCache::new();
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("src");
        let deep_dir = sub_dir.join("deep");
        std::fs::create_dir_all(&deep_dir).unwrap();
        let deep_file = deep_dir.join("file.rs");
        std::fs::write(&deep_file, "fn main() {}").unwrap();

        // Cache a list_dir for "src/".
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), sub_dir.to_string_lossy().to_string());

        cache.insert(
            "list_dir",
            &args,
            "deep/".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(60),
            Some(sub_dir.clone()),
        );

        assert!(cache.get("list_dir", &args).is_some());

        // Modify a deeply nested file — should NOT invalidate the ancestor list_dir.
        cache.invalidate_path(&deep_file);

        assert!(
            cache.get("list_dir", &args).is_some(),
            "deep nested change should NOT invalidate ancestor list_dir"
        );
    }

    #[test]
    fn clear_removes_all_entries() {
        let cache = ToolCache::new();
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "/tmp/test.txt".to_owned());

        cache.insert(
            "read_file",
            &args,
            "contents".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(60),
            None,
        );

        cache.clear();
        assert!(cache.get("read_file", &args).is_none());
    }

    #[test]
    fn cache_stats_track_operations() {
        let cache = ToolCache::new();
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "test.txt".to_owned());

        // Miss
        cache.get("read_file", &args);
        // Insert + Hit
        cache.insert(
            "read_file",
            &args,
            "data".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(30),
            None,
        );
        cache.get("read_file", &args);
        cache.get("read_file", &args);

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 2);
    }

    #[test]
    fn extract_tracked_path_for_tools() {
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "/tmp/foo.rs".to_owned());

        assert_eq!(
            extract_tracked_path("read_file", &args),
            Some(PathBuf::from("/tmp/foo.rs"))
        );
        assert_eq!(
            extract_tracked_path("list_dir", &args),
            Some(PathBuf::from("/tmp/foo.rs"))
        );
        assert_eq!(
            extract_tracked_path("list_tree", &args),
            Some(PathBuf::from("/tmp/foo.rs"))
        );
        assert_eq!(
            extract_tracked_path("search_files", &args),
            Some(PathBuf::from("/tmp/foo.rs"))
        );
        assert_eq!(extract_tracked_path("shell_exec", &args), None);
    }

    #[test]
    fn extract_tracked_path_search_defaults_to_cwd() {
        let args = BTreeMap::new();
        assert_eq!(
            extract_tracked_path("search_files", &args),
            Some(PathBuf::from("."))
        );
    }

    #[test]
    fn extract_tracked_path_for_write_tools() {
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "/tmp/out.txt".to_owned());

        assert_eq!(
            extract_tracked_path("write_file", &args),
            Some(PathBuf::from("/tmp/out.txt"))
        );
        assert_eq!(
            extract_tracked_path("patch", &args),
            Some(PathBuf::from("/tmp/out.txt"))
        );
    }

    #[test]
    fn fs_watcher_invalidates_on_file_change() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("watched.txt");
        std::fs::write(&file_path, "original").unwrap();

        let cache = ToolCache::new();
        cache.start_watching(dir.path());

        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), file_path.to_string_lossy().to_string());

        cache.insert(
            "read_file",
            &args,
            "original".to_owned(),
            BTreeMap::new(),
            Duration::from_secs(60),
            Some(file_path.clone()),
        );

        assert!(cache.get("read_file", &args).is_some());

        // Modify the file — the FS watcher should invalidate.
        std::fs::write(&file_path, "modified").unwrap();

        // Wait for the watcher event to propagate.
        std::thread::sleep(Duration::from_millis(500));

        let result = cache.get("read_file", &args);
        assert!(
            result.is_none(),
            "cache should have been invalidated by FS event"
        );
    }

    #[test]
    fn metadata_preserved_in_cache() {
        let cache = ToolCache::new();
        let mut args = BTreeMap::new();
        args.insert("path".to_owned(), "test.txt".to_owned());

        let mut metadata = BTreeMap::new();
        metadata.insert("size".to_owned(), "42".to_owned());

        cache.insert(
            "read_file",
            &args,
            "contents".to_owned(),
            metadata.clone(),
            Duration::from_secs(30),
            None,
        );

        let (_, cached_meta) = cache.get("read_file", &args).unwrap();
        assert_eq!(cached_meta, metadata);
    }
}
