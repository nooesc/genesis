use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use genesis_tools::sandbox::PathValidator;
use mlua::{Lua, Table};

/// Resolve a raw path string through the optional [`PathValidator`].
fn resolve_path(raw: &str, validator: &Option<Arc<PathValidator>>) -> mlua::Result<PathBuf> {
    match validator {
        Some(v) => v
            .validate(raw)
            .map_err(|e| mlua::Error::external(e.to_string())),
        None => Ok(PathBuf::from(raw)),
    }
}

/// Determine the effective base directory for search operations.
///
/// Priority: explicitly provided path > working_dir > "."
fn effective_base(
    explicit: Option<&str>,
    working_dir: &Option<PathBuf>,
    validator: &Option<Arc<PathValidator>>,
) -> mlua::Result<PathBuf> {
    if let Some(path) = explicit {
        return resolve_path(path, validator);
    }
    if let Some(ref wd) = *working_dir {
        return Ok(wd.clone());
    }
    Ok(PathBuf::from("."))
}

/// Build the `genesis.search` bridge table.
///
/// Provides three methods:
/// - `files(pattern, opts?)` — search file contents using ripgrep (falls back to grep)
/// - `glob(pattern, opts?)` — find files matching a glob pattern
/// - `tree(path?, opts?)` — render a directory tree
pub fn make_search_bridge(
    lua: &Lua,
    path_validator: Option<Arc<PathValidator>>,
    working_dir: Option<PathBuf>,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;

    // --- files ---
    let files_validator = path_validator.clone();
    let files_wd = working_dir.clone();
    table.set(
        "files",
        lua.create_function(move |lua, (pattern, opts): (String, Option<Table>)| {
            let search_path_raw = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("path").ok().flatten());
            let glob_filter = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("glob").ok().flatten());
            let max_results: usize = opts
                .as_ref()
                .and_then(|o| o.get::<Option<usize>>("max_results").ok().flatten())
                .unwrap_or(100);

            let base = effective_base(search_path_raw.as_deref(), &files_wd, &files_validator)?;

            // Try ripgrep first, fall back to grep -rn.
            let output = try_ripgrep(&pattern, &base, glob_filter.as_deref())
                .or_else(|| try_grep(&pattern, &base))
                .ok_or_else(|| {
                    mlua::Error::external("search.files: neither 'rg' nor 'grep' could be executed")
                })?;

            let results = lua.create_table()?;
            let mut count = 0;
            for line in output.lines() {
                if count >= max_results {
                    break;
                }
                // Parse vimgrep-style output: file:line:col:content  (rg)
                // or grep output: file:line:content
                if let Some(entry) = parse_search_line(lua, line)? {
                    count += 1;
                    results.set(count, entry)?;
                }
            }

            Ok(results)
        })?,
    )?;

    // --- glob ---
    let glob_validator = path_validator.clone();
    let glob_wd = working_dir.clone();
    table.set(
        "glob",
        lua.create_function(move |lua, (pattern, opts): (String, Option<Table>)| {
            let base_path_raw = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("path").ok().flatten());
            let base = effective_base(base_path_raw.as_deref(), &glob_wd, &glob_validator)?;

            // Build the full glob pattern by joining the base path and the pattern.
            let full_pattern = base.join(&pattern);
            let full_pattern_str = full_pattern.to_string_lossy();

            let results = lua.create_table()?;
            let mut index = 0;

            // Use the glob crate via std::process since we don't have it as a
            // direct dependency. Instead, do a simple recursive walk with
            // pattern matching.
            let entries = collect_glob_matches(&full_pattern_str)
                .map_err(|e| mlua::Error::external(format!("search.glob: {e}")))?;

            for path in entries {
                index += 1;
                results.set(index, path.to_string_lossy().into_owned())?;
            }

            Ok(results)
        })?,
    )?;

    // --- tree ---
    let tree_validator = path_validator;
    let tree_wd = working_dir;
    table.set(
        "tree",
        lua.create_function(
            move |_, (path_arg, opts): (Option<String>, Option<Table>)| {
                let max_depth: usize = opts
                    .as_ref()
                    .and_then(|o| o.get::<Option<usize>>("max_depth").ok().flatten())
                    .unwrap_or(3);

                let base = effective_base(path_arg.as_deref(), &tree_wd, &tree_validator)?;

                if !base.is_dir() {
                    return Err(mlua::Error::external(format!(
                        "search.tree: '{}' is not a directory",
                        base.display()
                    )));
                }

                let mut output = String::new();
                output.push_str(&format!("{}\n", base.display()));
                build_tree(&base, "", max_depth, 0, &mut output)
                    .map_err(|e| mlua::Error::external(format!("search.tree: {e}")))?;

                Ok(output)
            },
        )?,
    )?;

    Ok(table)
}

/// Attempt to run `rg --vimgrep` and return its stdout on success.
fn try_ripgrep(pattern: &str, path: &Path, glob_filter: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("rg");
    cmd.args(["--vimgrep", "--no-heading", pattern]);
    if let Some(g) = glob_filter {
        cmd.args(["--glob", g]);
    }
    cmd.arg(path);
    let output = cmd.output().ok()?;
    // rg returns exit code 1 for "no matches" — that's not an error.
    if output.status.success() || output.status.code() == Some(1) {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// Attempt to run `grep -rn` and return its stdout on success.
fn try_grep(pattern: &str, path: &Path) -> Option<String> {
    let output = Command::new("grep")
        .args(["-rn", pattern])
        .arg(path)
        .output()
        .ok()?;
    // grep returns exit code 1 for "no matches" — that's not an error.
    if output.status.success() || output.status.code() == Some(1) {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// Parse a single line of vimgrep-style or grep-style output into a Lua table.
///
/// Expected formats:
/// - `file:line:col:content` (rg --vimgrep)
/// - `file:line:content`     (grep -rn)
fn parse_search_line(lua: &Lua, line: &str) -> mlua::Result<Option<Table>> {
    if line.is_empty() {
        return Ok(None);
    }

    // Split on `:` — we need at least 3 parts (file, line, content).
    // Be careful: file paths on Windows could contain `:`, but on Unix this is
    // safe. The content portion may also contain colons, so we use splitn.
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 3 {
        return Ok(None);
    }

    let file = parts[0];
    let line_num = parts[1];
    // If we got 4 parts, parts[2] might be a column (rg) and parts[3] is content.
    // If we got 3 parts, parts[2] is the content (grep).
    let content = if parts.len() == 4 {
        // Check if parts[2] looks like a column number.
        if parts[2].parse::<u32>().is_ok() {
            parts[3]
        } else {
            // Not a column — rejoin parts[2] and parts[3].
            // This shouldn't normally happen with vimgrep format.
            parts[2]
        }
    } else {
        parts[2]
    };

    let entry = lua.create_table()?;
    entry.set("file", file)?;
    entry.set(
        "line",
        line_num
            .parse::<i64>()
            .map_err(|_| mlua::Error::external("search.files: could not parse line number"))?,
    )?;
    entry.set("content", content.trim_end())?;

    Ok(Some(entry))
}

/// Collect file paths matching a glob pattern using a simple recursive walk.
///
/// The pattern is expected to be a full path pattern like `/tmp/dir/**/*.rs`.
fn collect_glob_matches(pattern: &str) -> Result<Vec<PathBuf>, String> {
    // Split the pattern into a base directory and the glob portion.
    // Find the first component that contains a glob meta-character.
    let path = Path::new(pattern);
    let mut base = PathBuf::new();
    let mut glob_start = None;

    for (i, component) in path.components().enumerate() {
        let s = component.as_os_str().to_string_lossy();
        if s.contains('*') || s.contains('?') || s.contains('[') {
            glob_start = Some(i);
            break;
        }
        base.push(component);
    }

    if glob_start.is_none() {
        // No glob characters — just check if the path exists.
        if Path::new(pattern).exists() {
            return Ok(vec![PathBuf::from(pattern)]);
        }
        return Ok(vec![]);
    }

    // Extract the glob portion (everything from glob_start onwards).
    let glob_parts: Vec<String> = path
        .components()
        .skip(glob_start.unwrap())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let glob_pattern = glob_parts.join("/");

    if !base.is_dir() {
        return Ok(vec![]);
    }

    let mut results = Vec::new();
    walk_and_match(&base, &glob_pattern, &mut results)?;
    results.sort();
    Ok(results)
}

/// Recursively walk `dir` and collect paths that match the glob `pattern`
/// relative to `dir`.
fn walk_and_match(dir: &Path, pattern: &str, results: &mut Vec<PathBuf>) -> Result<(), String> {
    walk_and_match_inner(dir, dir, pattern, results)
}

fn walk_and_match_inner(
    base: &Path,
    current: &Path,
    pattern: &str,
    results: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(current)
        .map_err(|e| format!("reading directory {}: {e}", current.display()))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("reading entry: {e}"))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .map_err(|e| format!("strip prefix: {e}"))?;
        let relative_str = relative.to_string_lossy();

        if glob_matches(pattern, &relative_str) {
            results.push(path.clone());
        }

        // Use symlink_metadata to avoid following symlinks (which could cause
        // infinite loops or escape the sandbox).
        let is_real_dir = std::fs::symlink_metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_real_dir {
            // Only recurse if the pattern could match deeper paths (contains `/` or `**`).
            walk_and_match_inner(base, &path, pattern, results)?;
        }
    }

    Ok(())
}

/// Simple glob matching supporting `*`, `**`, and `?`.
fn glob_matches(pattern: &str, text: &str) -> bool {
    glob_matches_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_matches_inner(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    // Handle `**` (matches any number of path segments).
    if pattern.starts_with(b"**/") {
        // `**/rest` matches `rest` or `anything/.../rest`
        let rest = &pattern[3..];
        if glob_matches_inner(rest, text) {
            return true;
        }
        // Try skipping one path component at a time.
        for i in 0..text.len() {
            if text[i] == b'/' && glob_matches_inner(rest, &text[i + 1..]) {
                return true;
            }
        }
        return false;
    }

    // Handle trailing `**`.
    if pattern == b"**" {
        return true;
    }

    match pattern[0] {
        b'*' => {
            // `*` matches any sequence of non-`/` characters.
            if glob_matches_inner(&pattern[1..], text) {
                return true;
            }
            if !text.is_empty() && text[0] != b'/' {
                return glob_matches_inner(pattern, &text[1..]);
            }
            false
        }
        b'?' => {
            // `?` matches any single non-`/` character.
            if !text.is_empty() && text[0] != b'/' {
                glob_matches_inner(&pattern[1..], &text[1..])
            } else {
                false
            }
        }
        b => {
            if !text.is_empty() && text[0] == b {
                glob_matches_inner(&pattern[1..], &text[1..])
            } else {
                false
            }
        }
    }
}

/// Build a tree representation of a directory, similar to the `tree` command.
fn build_tree(
    dir: &Path,
    prefix: &str,
    max_depth: usize,
    current_depth: usize,
    output: &mut String,
) -> Result<(), std::io::Error> {
    if current_depth >= max_depth {
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    let total = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let is_last = i == total - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden directories to avoid noise.
        if name_str.starts_with('.') {
            continue;
        }

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            output.push_str(&format!("{prefix}{connector}{name_str}/\n"));
            let child_prefix = if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}│   ")
            };
            build_tree(
                &entry.path(),
                &child_prefix,
                max_depth,
                current_depth + 1,
                output,
            )?;
        } else {
            output.push_str(&format!("{prefix}{connector}{name_str}\n"));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use genesis_tools::sandbox::PathValidator;

    /// Create a bare Lua VM with `genesis.search` installed, backed by a
    /// [`PathValidator`] rooted at `working_dir`.
    fn test_lua_with_search(working_dir: &std::path::Path) -> mlua::Lua {
        let validator = Arc::new(PathValidator::new(Some(working_dir.to_path_buf())));
        let lua = mlua::Lua::new();
        let search_table =
            super::make_search_bridge(&lua, Some(validator), Some(working_dir.to_path_buf()))
                .expect("make_search_bridge should succeed");
        let genesis = lua.create_table().expect("table should create");
        genesis
            .set("search", search_table)
            .expect("set search should work");
        lua.globals()
            .set("genesis", genesis)
            .expect("set genesis should work");
        lua
    }

    #[test]
    fn search_files_finds_pattern() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        std::fs::write(dir.path().join("hello.txt"), "hello world\nfoo bar\n")
            .expect("write hello.txt");
        std::fs::write(dir.path().join("other.txt"), "nothing here\nhello again\n")
            .expect("write other.txt");
        std::fs::write(dir.path().join("ignore.rs"), "no match\n").expect("write ignore.rs");

        let lua = test_lua_with_search(dir.path());
        let result: mlua::Table = lua
            .load(&format!(
                "return genesis.search.files('hello', {{ path = '{}' }})",
                dir.path().display()
            ))
            .eval()
            .expect("search.files should succeed");

        let len = result.raw_len();
        assert!(
            len >= 2,
            "should find at least 2 matches for 'hello', got {len}"
        );

        // Verify the structure of the first result.
        let first: mlua::Table = result.get(1).expect("first result should exist");
        let _file: String = first.get("file").expect("file field should exist");
        let _line: i64 = first.get("line").expect("line field should exist");
        let _content: String = first.get("content").expect("content field should exist");
    }

    #[test]
    fn search_files_respects_max_results() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        // Create a file with many matching lines.
        let content: String = (0..50).map(|i| format!("match line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), &content).expect("write big.txt");

        let lua = test_lua_with_search(dir.path());
        let result: mlua::Table = lua
            .load(&format!(
                "return genesis.search.files('match', {{ path = '{}', max_results = 5 }})",
                dir.path().display()
            ))
            .eval()
            .expect("search.files should succeed");

        let len = result.raw_len();
        assert_eq!(len, 5, "should limit to max_results=5, got {len}");
    }

    #[test]
    fn glob_finds_matching_files() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        std::fs::write(dir.path().join("a.txt"), "aaa").expect("write a.txt");
        std::fs::write(dir.path().join("b.txt"), "bbb").expect("write b.txt");
        std::fs::write(dir.path().join("c.rs"), "ccc").expect("write c.rs");

        let lua = test_lua_with_search(dir.path());
        let result: mlua::Table = lua
            .load(&format!(
                "return genesis.search.glob('*.txt', {{ path = '{}' }})",
                dir.path().display()
            ))
            .eval()
            .expect("search.glob should succeed");

        let len = result.raw_len();
        assert_eq!(len, 2, "should find 2 .txt files, got {len}");

        // Collect the paths and verify they end with .txt.
        let mut paths: Vec<String> = Vec::new();
        for i in 1..=len {
            let path: String = result.get(i).expect("path should exist");
            assert!(path.ends_with(".txt"), "path should end with .txt: {path}");
            paths.push(path);
        }
        paths.sort();
        assert!(paths[0].ends_with("a.txt"));
        assert!(paths[1].ends_with("b.txt"));
    }

    #[test]
    fn glob_recursive_pattern() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("mkdir sub");
        std::fs::write(dir.path().join("top.rs"), "top").expect("write top.rs");
        std::fs::write(sub.join("nested.rs"), "nested").expect("write nested.rs");

        let lua = test_lua_with_search(dir.path());
        let result: mlua::Table = lua
            .load(&format!(
                "return genesis.search.glob('**/*.rs', {{ path = '{}' }})",
                dir.path().display()
            ))
            .eval()
            .expect("search.glob should succeed");

        let len = result.raw_len();
        assert_eq!(len, 2, "should find 2 .rs files recursively, got {len}");
    }

    #[test]
    fn tree_shows_directory_structure() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).expect("mkdir src");
        std::fs::write(dir.path().join("README.md"), "readme").expect("write README");
        std::fs::write(sub.join("main.rs"), "fn main() {}").expect("write main.rs");

        let lua = test_lua_with_search(dir.path());
        let result: String = lua
            .load(&format!(
                "return genesis.search.tree('{}')",
                dir.path().display()
            ))
            .eval()
            .expect("search.tree should succeed");

        // The tree should contain the directory name and files.
        assert!(
            result.contains("README.md"),
            "tree should contain README.md: {result}"
        );
        assert!(
            result.contains("src/"),
            "tree should contain src/: {result}"
        );
        assert!(
            result.contains("main.rs"),
            "tree should contain main.rs: {result}"
        );
        // Verify tree connectors are present.
        assert!(
            result.contains("├── ") || result.contains("└── "),
            "tree should contain tree connectors: {result}"
        );
    }

    #[test]
    fn tree_respects_max_depth() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let deep = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).expect("mkdir deep");
        std::fs::write(deep.join("deep.txt"), "deep").expect("write deep.txt");

        let lua = test_lua_with_search(dir.path());
        let result: String = lua
            .load(&format!(
                "return genesis.search.tree('{}', {{ max_depth = 1 }})",
                dir.path().display()
            ))
            .eval()
            .expect("search.tree should succeed");

        // At max_depth=1, we should see `a/` but NOT `b/` or `deep.txt`.
        assert!(result.contains("a/"), "tree should contain a/: {result}");
        assert!(
            !result.contains("b/"),
            "tree should NOT contain b/ at depth 1: {result}"
        );
        assert!(
            !result.contains("deep.txt"),
            "tree should NOT contain deep.txt at depth 1: {result}"
        );
    }

    // --- glob_matches unit tests ---

    #[test]
    fn glob_matches_simple_star() {
        assert!(super::glob_matches("*.txt", "hello.txt"));
        assert!(!super::glob_matches("*.txt", "hello.rs"));
        assert!(!super::glob_matches("*.txt", "dir/hello.txt"));
    }

    #[test]
    fn glob_matches_double_star() {
        assert!(super::glob_matches("**/*.rs", "main.rs"));
        assert!(super::glob_matches("**/*.rs", "src/main.rs"));
        assert!(super::glob_matches("**/*.rs", "src/lib/mod.rs"));
        assert!(!super::glob_matches("**/*.rs", "main.txt"));
    }

    #[test]
    fn glob_matches_question_mark() {
        assert!(super::glob_matches("?.txt", "a.txt"));
        assert!(!super::glob_matches("?.txt", "ab.txt"));
        assert!(!super::glob_matches("?.txt", ".txt"));
    }
}
