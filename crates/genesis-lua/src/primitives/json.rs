use mlua::{Lua, LuaSerdeExt, Table, Value};

/// Build the `genesis.json` bridge table with `encode`, `decode`, and
/// `encode_pretty` functions.
///
/// Conversion between Lua values and JSON is handled by [`mlua::LuaSerdeExt`],
/// which correctly maps sequential-integer-keyed tables to JSON arrays and
/// string-keyed tables to JSON objects.
pub fn make_json_bridge(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;

    table.set(
        "encode",
        lua.create_function(|lua, value: Value| {
            let json: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string(&json).map_err(mlua::Error::external)
        })?,
    )?;

    table.set(
        "decode",
        lua.create_function(|lua, s: String| {
            let json: serde_json::Value =
                serde_json::from_str(&s).map_err(mlua::Error::external)?;
            lua.to_value(&json)
        })?,
    )?;

    // Pretty-print variant of encode.
    table.set(
        "encode_pretty",
        lua.create_function(|lua, value: Value| {
            let json: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string_pretty(&json).map_err(mlua::Error::external)
        })?,
    )?;

    Ok(table)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{LuaRuntimeConfig, LuaSessionContext};

    /// Build a minimal [`crate::LuaRuntime`] with no plugins loaded, suitable
    /// for testing the `genesis.json` bridge.
    fn test_runtime() -> crate::LuaRuntime {
        let dir = tempfile::tempdir().expect("tempdir should exist");
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
                ..Default::default()
            })
            .build()
            .expect("test runtime should build")
    }

    #[test]
    fn encode_table_to_json_string() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                local t = { name = "test", count = 42 }
                return genesis.json.encode(t)
                "#,
            )
            .expect("encode should succeed");

        // The result is a JSON string — parse it back to verify contents.
        let encoded: String = serde_json::from_value(value).expect("should be a string");
        let parsed: serde_json::Value =
            serde_json::from_str(&encoded).expect("should be valid JSON");
        assert_eq!(parsed["name"], json!("test"));
        assert_eq!(parsed["count"], json!(42));
    }

    #[test]
    fn decode_json_string_to_table() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                local t = genesis.json.decode('{"name":"test","count":42}')
                return { name = t.name, count = t.count }
                "#,
            )
            .expect("decode should succeed");

        assert_eq!(value["name"], json!("test"));
        assert_eq!(value["count"], json!(42));
    }

    #[test]
    fn encode_decode_roundtrip() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                local original = { greeting = "hello", value = 3.14, flag = true }
                local encoded = genesis.json.encode(original)
                local decoded = genesis.json.decode(encoded)
                return {
                    greeting = decoded.greeting,
                    value = decoded.value,
                    flag = decoded.flag,
                }
                "#,
            )
            .expect("roundtrip should succeed");

        assert_eq!(value["greeting"], json!("hello"));
        assert_eq!(value["value"], json!(3.14));
        assert_eq!(value["flag"], json!(true));
    }

    #[test]
    fn encode_array() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                return genesis.json.encode({1, 2, 3})
                "#,
            )
            .expect("encode array should succeed");

        let encoded: String = serde_json::from_value(value).expect("should be a string");
        assert_eq!(encoded, "[1,2,3]");
    }

    #[test]
    fn decode_array() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                local arr = genesis.json.decode("[10, 20, 30]")
                return { first = arr[1], second = arr[2], third = arr[3] }
                "#,
            )
            .expect("decode array should succeed");

        assert_eq!(value["first"], json!(10));
        assert_eq!(value["second"], json!(20));
        assert_eq!(value["third"], json!(30));
    }

    #[test]
    fn decode_invalid_json_returns_error() {
        let runtime = test_runtime();
        let err = runtime
            .eval_string(
                r#"
                return genesis.json.decode("not json")
                "#,
            )
            .expect_err("decode should fail on invalid JSON");

        let msg = err.to_string();
        assert!(
            msg.contains("expected") || msg.contains("JSON"),
            "error should mention the parse failure: {msg}"
        );
    }

    #[test]
    fn encode_nil_produces_null() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                return genesis.json.encode(nil)
                "#,
            )
            .expect("encode nil should succeed");

        let encoded: String = serde_json::from_value(value).expect("should be a string");
        assert_eq!(encoded, "null");
    }

    #[test]
    fn encode_nested_table() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                local t = { outer = { inner = "deep" } }
                return genesis.json.encode(t)
                "#,
            )
            .expect("encode nested table should succeed");

        let encoded: String = serde_json::from_value(value).expect("should be a string");
        let parsed: serde_json::Value =
            serde_json::from_str(&encoded).expect("should be valid JSON");
        assert_eq!(parsed["outer"]["inner"], json!("deep"));
    }

    #[test]
    fn encode_pretty_produces_indented_output() {
        let runtime = test_runtime();
        let value = runtime
            .eval_string(
                r#"
                return genesis.json.encode_pretty({ a = 1 })
                "#,
            )
            .expect("encode_pretty should succeed");

        let encoded: String = serde_json::from_value(value).expect("should be a string");
        assert!(
            encoded.contains('\n'),
            "pretty-printed JSON should contain newlines: {encoded}"
        );
    }
}
