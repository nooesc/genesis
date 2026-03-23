use std::path::PathBuf;

use genesis_storage::MemoryStore;
use mlua::{Lua, LuaSerdeExt, Table, Value};

/// Build the `genesis.storage` bridge table.
///
/// Currently exposes a `memory` sub-table with `list`, `search`, `create`, and
/// `delete` operations backed by [`genesis_storage::MemoryStore`].
pub fn make_storage_bridge(lua: &Lua, database_path: PathBuf) -> mlua::Result<Table> {
    let storage = lua.create_table()?;
    storage.set("memory", make_memory_table(lua, database_path)?)?;
    Ok(storage)
}

fn make_memory_table(lua: &Lua, database_path: PathBuf) -> mlua::Result<Table> {
    let memory = lua.create_table()?;

    // genesis.storage.memory.list(limit?)
    let list_path = database_path.clone();
    memory.set(
        "list",
        lua.create_function(move |lua, limit: Option<usize>| {
            let store = MemoryStore::new(&list_path);
            let memories = store
                .list(limit.unwrap_or(10))
                .map_err(mlua::Error::external)?;
            lua.to_value(&memories)
        })?,
    )?;

    // genesis.storage.memory.search(query, limit?)
    let search_path = database_path.clone();
    memory.set(
        "search",
        lua.create_function(move |lua, (query, limit): (String, Option<usize>)| {
            let store = MemoryStore::new(&search_path);
            let memories = store
                .search(&query, limit.unwrap_or(5))
                .map_err(mlua::Error::external)?;
            lua.to_value(&memories)
        })?,
    )?;

    // genesis.storage.memory.create(content, metadata?)
    let create_path = database_path.clone();
    memory.set(
        "create",
        lua.create_function(move |_, (content, metadata): (String, Option<Value>)| {
            let kind = resolve_memory_kind(metadata)?;
            let store = MemoryStore::new(&create_path);
            let created = store
                .create(None, &kind, &content)
                .map_err(mlua::Error::external)?;
            Ok(created.id)
        })?,
    )?;

    // genesis.storage.memory.delete(id)
    memory.set(
        "delete",
        lua.create_function(move |_, id: String| {
            let store = MemoryStore::new(&database_path);
            let deleted = store.delete(&id).map_err(mlua::Error::external)?;
            Ok(deleted)
        })?,
    )?;

    Ok(memory)
}

/// Resolve the memory kind from an optional metadata argument.
///
/// Accepts `nil` (defaults to `"note"`), a plain string, or a table with a
/// `kind` field.
fn resolve_memory_kind(metadata: Option<Value>) -> mlua::Result<String> {
    match metadata {
        None | Some(Value::Nil) => Ok("note".to_owned()),
        Some(Value::String(kind)) => Ok(kind.to_str()?.to_owned()),
        Some(Value::Table(table)) => {
            if let Ok(kind) = table.get::<String>("kind") {
                return Ok(kind);
            }
            if let Ok(kind) = table.get::<String>(1) {
                return Ok(kind);
            }
            Ok("note".to_owned())
        }
        Some(other) => Err(mlua::Error::external(format!(
            "unsupported memory metadata: {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{LuaRuntimeConfig, LuaSessionContext};

    fn test_runtime_with_db(database_path: &std::path::Path) -> crate::LuaRuntime {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let mut config_values = BTreeMap::new();
        config_values.insert(
            "database_path".to_owned(),
            database_path.to_string_lossy().into_owned(),
        );
        crate::LuaRuntime::builder()
            .with_config(LuaRuntimeConfig {
                plugin_dir: dir.path().to_path_buf(),
                session: LuaSessionContext {
                    id: "test-sess".to_owned(),
                    model: "test-model".to_owned(),
                    turn_count: 0,
                    total_tokens: 0,
                    platform: "cli".to_owned(),
                    personality: None,
                },
                disabled_plugins: Vec::new(),
                plugin_verbose: None,
                config_values,
            })
            .build()
            .expect("test runtime should build")
    }

    fn bootstrapped_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).expect("bootstrap should succeed");
        (dir, db_path)
    }

    #[test]
    fn memory_create_and_list() {
        let (_dir, db_path) = bootstrapped_db();
        let runtime = test_runtime_with_db(&db_path);

        let value = runtime
            .eval_string(
                r#"
                local id = genesis.storage.memory.create("remember this fact")
                local memories = genesis.storage.memory.list(10)
                local found = false
                for _, m in ipairs(memories) do
                    if m.id == id then
                        found = true
                    end
                end
                return { id = id, found = found, count = #memories }
                "#,
            )
            .expect("create and list should succeed");

        assert!(
            value["id"].is_string(),
            "id should be a string: {:?}",
            value["id"]
        );
        assert_eq!(value["found"], true);
        assert_eq!(value["count"], 1);
    }

    #[test]
    fn memory_search() {
        let (_dir, db_path) = bootstrapped_db();
        let runtime = test_runtime_with_db(&db_path);

        let value = runtime
            .eval_string(
                r#"
                genesis.storage.memory.create("the sky is blue")
                genesis.storage.memory.create("roses are red")
                local results = genesis.storage.memory.search("blue")
                local found_blue = false
                for _, m in ipairs(results) do
                    if string.find(m.content, "blue") then
                        found_blue = true
                    end
                end
                return { found_blue = found_blue, count = #results }
                "#,
            )
            .expect("search should succeed");

        assert_eq!(value["found_blue"], true);
        assert!(
            value["count"].as_i64().unwrap() >= 1,
            "should find at least one result"
        );
    }

    #[test]
    fn memory_delete() {
        let (_dir, db_path) = bootstrapped_db();
        let runtime = test_runtime_with_db(&db_path);

        let value = runtime
            .eval_string(
                r#"
                local id = genesis.storage.memory.create("temporary note")
                local deleted = genesis.storage.memory.delete(id)
                local memories = genesis.storage.memory.list(10)
                local still_exists = false
                for _, m in ipairs(memories) do
                    if m.id == id then
                        still_exists = true
                    end
                end
                return { deleted = deleted, still_exists = still_exists }
                "#,
            )
            .expect("delete should succeed");

        assert_eq!(value["deleted"], true);
        assert_eq!(value["still_exists"], false);
    }
}
