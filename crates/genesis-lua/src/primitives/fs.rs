use std::path::PathBuf;
use std::sync::Arc;

use genesis_tools::sandbox::PathValidator;
use mlua::{Lua, Table};

/// Maximum bytes returned by `genesis.fs.read`. Matches the ReadFileTool limit.
const MAX_READ_BYTES: usize = 128 * 1024;

/// Resolve a raw path string through the optional [`PathValidator`].
///
/// When a validator is provided, the path is validated against the sandbox
/// policy. When no validator is set, the raw path is returned as-is (useful
/// for backward compatibility and tests without sandboxing).
fn resolve_path(raw: &str, validator: &Option<Arc<PathValidator>>) -> mlua::Result<PathBuf> {
    match validator {
        Some(v) => v
            .validate(raw)
            .map_err(|e| mlua::Error::external(e.to_string())),
        None => Ok(PathBuf::from(raw)),
    }
}

/// Build the `genesis.fs` bridge table.
///
/// Provides five methods:
/// - `read(path)` — read file contents (truncated at 128 KB)
/// - `write(path, content)` — write file, creating parent directories
/// - `list(path)` — list directory entries as `{ name, is_dir, size }`
/// - `exists(path)` — check whether a path exists
/// - `mkdir(path)` — create a directory (and parents)
pub fn make_fs_bridge(lua: &Lua, validator: Option<Arc<PathValidator>>) -> mlua::Result<Table> {
    let table = lua.create_table()?;

    // --- read ---
    let read_validator = validator.clone();
    table.set(
        "read",
        lua.create_function(move |_, path: String| {
            let resolved = resolve_path(&path, &read_validator)?;
            let contents = std::fs::read_to_string(&resolved).map_err(|e| {
                mlua::Error::external(format!("fs.read `{}`: {e}", resolved.display()))
            })?;
            if contents.len() > MAX_READ_BYTES {
                // Truncate at a UTF-8 safe boundary.
                let truncated = &contents[..contents.floor_char_boundary(MAX_READ_BYTES)];
                Ok(truncated.to_owned())
            } else {
                Ok(contents)
            }
        })?,
    )?;

    // --- write ---
    let write_validator = validator.clone();
    table.set(
        "write",
        lua.create_function(move |_, (path, content): (String, String)| {
            let resolved = resolve_path(&path, &write_validator)?;
            // Create parent directories (no-op if they already exist).
            if let Some(parent) = resolved.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    mlua::Error::external(format!(
                        "fs.write create parent dirs `{}`: {e}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::write(&resolved, content).map_err(|e| {
                mlua::Error::external(format!("fs.write `{}`: {e}", resolved.display()))
            })?;
            Ok(true)
        })?,
    )?;

    // --- list ---
    let list_validator = validator.clone();
    table.set(
        "list",
        lua.create_function(move |lua, path: String| {
            let resolved = resolve_path(&path, &list_validator)?;
            let entries = std::fs::read_dir(&resolved).map_err(|e| {
                mlua::Error::external(format!("fs.list `{}`: {e}", resolved.display()))
            })?;
            let result = lua.create_table()?;
            let mut index = 1;
            for entry in entries {
                let entry =
                    entry.map_err(|e| mlua::Error::external(format!("fs.list entry: {e}")))?;
                let meta = entry
                    .metadata()
                    .map_err(|e| mlua::Error::external(format!("fs.list metadata: {e}")))?;
                let row = lua.create_table()?;
                row.set("name", entry.file_name().to_string_lossy().into_owned())?;
                row.set("is_dir", meta.is_dir())?;
                row.set("size", meta.len())?;
                result.set(index, row)?;
                index += 1;
            }
            Ok(result)
        })?,
    )?;

    // --- exists ---
    let exists_validator = validator.clone();
    table.set(
        "exists",
        lua.create_function(move |_, path: String| {
            let resolved = resolve_path(&path, &exists_validator)?;
            Ok(resolved.exists())
        })?,
    )?;

    // --- mkdir ---
    let mkdir_validator = validator;
    table.set(
        "mkdir",
        lua.create_function(move |_, path: String| {
            let resolved = resolve_path(&path, &mkdir_validator)?;
            std::fs::create_dir_all(&resolved).map_err(|e| {
                mlua::Error::external(format!("fs.mkdir `{}`: {e}", resolved.display()))
            })?;
            Ok(true)
        })?,
    )?;

    Ok(table)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use genesis_tools::sandbox::PathValidator;

    /// Create a bare Lua VM with `genesis.fs` installed, backed by a
    /// [`PathValidator`] rooted at `working_dir`.
    fn test_lua_with_fs(working_dir: &std::path::Path) -> mlua::Lua {
        let validator = Arc::new(PathValidator::new(
            Some(working_dir.to_path_buf()),
            PathBuf::from("/tmp/fake-home"),
        ));
        let lua = mlua::Lua::new();
        let fs_table =
            super::make_fs_bridge(&lua, Some(validator)).expect("make_fs_bridge should succeed");
        let genesis = lua.create_table().expect("table should create");
        genesis.set("fs", fs_table).expect("set fs should work");
        lua.globals()
            .set("genesis", genesis)
            .expect("set genesis should work");
        lua
    }

    #[test]
    fn read_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        std::fs::write(dir.path().join("hello.txt"), "hello world").expect("write should succeed");
        let lua = test_lua_with_fs(dir.path());
        let result: String = lua
            .load(&format!(
                "return genesis.fs.read('{}/hello.txt')",
                dir.path().display()
            ))
            .eval()
            .expect("read should succeed");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn write_and_read_file() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let lua = test_lua_with_fs(dir.path());
        let file_path = dir.path().join("output.txt");
        let wrote: bool = lua
            .load(&format!(
                "return genesis.fs.write('{}', 'test content')",
                file_path.display()
            ))
            .eval()
            .expect("write should succeed");
        assert!(wrote);

        let content: String = lua
            .load(&format!(
                "return genesis.fs.read('{}')",
                file_path.display()
            ))
            .eval()
            .expect("read should succeed");
        assert_eq!(content, "test content");
    }

    #[test]
    fn list_directory() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        std::fs::write(dir.path().join("a.txt"), "aaa").expect("write a");
        std::fs::write(dir.path().join("b.txt"), "bbb").expect("write b");
        std::fs::create_dir(dir.path().join("subdir")).expect("mkdir subdir");
        let lua = test_lua_with_fs(dir.path());
        let result: mlua::Table = lua
            .load(&format!(
                "return genesis.fs.list('{}')",
                dir.path().display()
            ))
            .eval()
            .expect("list should succeed");

        let len = result.raw_len();
        assert_eq!(len, 3, "should have 3 entries");

        // Collect names for verification.
        let mut names: Vec<String> = Vec::new();
        let mut found_dir = false;
        for i in 1..=len {
            let entry: mlua::Table = result.get(i).expect("entry should exist");
            let name: String = entry.get("name").expect("name should exist");
            let is_dir: bool = entry.get("is_dir").expect("is_dir should exist");
            if name == "subdir" {
                assert!(is_dir, "subdir should be a directory");
                found_dir = true;
            }
            names.push(name);
        }
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt", "subdir"]);
        assert!(found_dir, "should have found subdir as a directory");
    }

    #[test]
    fn exists_returns_true_for_existing() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        std::fs::write(dir.path().join("present.txt"), "data").expect("write");
        let lua = test_lua_with_fs(dir.path());
        let result: bool = lua
            .load(&format!(
                "return genesis.fs.exists('{}/present.txt')",
                dir.path().display()
            ))
            .eval()
            .expect("exists should succeed");
        assert!(result);
    }

    #[test]
    fn exists_returns_false_for_missing() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let lua = test_lua_with_fs(dir.path());
        let result: bool = lua
            .load(&format!(
                "return genesis.fs.exists('{}/nope.txt')",
                dir.path().display()
            ))
            .eval()
            .expect("exists should succeed");
        assert!(!result);
    }

    #[test]
    fn mkdir_creates_directory() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let lua = test_lua_with_fs(dir.path());
        let new_dir = dir.path().join("deep/nested/dir");
        let result: bool = lua
            .load(&format!("return genesis.fs.mkdir('{}')", new_dir.display()))
            .eval()
            .expect("mkdir should succeed");
        assert!(result);
        assert!(new_dir.exists(), "directory should have been created");
        assert!(new_dir.is_dir(), "path should be a directory");
    }

    #[test]
    fn read_blocked_outside_working_dir() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let lua = test_lua_with_fs(dir.path());
        let err = lua
            .load("return genesis.fs.read('/etc/hosts')")
            .eval::<String>()
            .expect_err("read outside working dir should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("blocked") || msg.contains("outside"),
            "error should mention blocking: {msg}"
        );
    }

    #[test]
    fn write_creates_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let lua = test_lua_with_fs(dir.path());
        let nested_file = dir.path().join("a/b/c/file.txt");
        let result: bool = lua
            .load(&format!(
                "return genesis.fs.write('{}', 'nested content')",
                nested_file.display()
            ))
            .eval()
            .expect("write with nested dirs should succeed");
        assert!(result);
        assert_eq!(
            std::fs::read_to_string(&nested_file).expect("should read back"),
            "nested content"
        );
    }

    #[test]
    fn write_blocked_outside_working_dir() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let lua = test_lua_with_fs(dir.path());
        let err = lua
            .load("return genesis.fs.write('/tmp/evil.txt', 'bad')")
            .eval::<bool>()
            .expect_err("write outside working dir should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("blocked") || msg.contains("outside"),
            "error should mention blocking: {msg}"
        );
    }
}
