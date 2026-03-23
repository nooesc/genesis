use mlua::{Lua, Table, Value};

/// Convert a Lua [`Value`] into a [`serde_json::Value`].
///
/// Tables are inspected via `raw_len()`: if the raw length is > 0 the table is
/// treated as an array (sequential integer keys starting at 1), otherwise as an
/// object (string keys).
pub fn lua_value_to_json(value: Value) -> mlua::Result<serde_json::Value> {
    match value {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Boolean(b) => Ok(serde_json::Value::Bool(b)),
        Value::Integer(n) => Ok(serde_json::json!(n)),
        Value::Number(n) => serde_json::Number::from_f64(n)
            .map(serde_json::Value::Number)
            .ok_or_else(|| mlua::Error::external(format!("cannot represent {n} as JSON number"))),
        Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_owned())),
        Value::Table(table) => {
            let raw_len = table.raw_len();
            if raw_len > 0 {
                // Array path — sequential integer keys 1..=raw_len.
                let mut arr = Vec::with_capacity(raw_len);
                for i in 1..=raw_len {
                    let v: Value = table.raw_get(i)?;
                    arr.push(lua_value_to_json(v)?);
                }
                Ok(serde_json::Value::Array(arr))
            } else {
                // Object path — iterate all pairs and keep string keys.
                let mut map = serde_json::Map::new();
                for pair in table.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_value_to_json(v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
        }
        other => Err(mlua::Error::external(format!(
            "cannot convert {} to JSON",
            other.type_name()
        ))),
    }
}

/// Convert a [`serde_json::Value`] into a Lua [`Value`].
pub fn json_to_lua_value(lua: &Lua, value: &serde_json::Value) -> mlua::Result<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Err(mlua::Error::external(format!(
                    "cannot represent JSON number {n} in Lua"
                )))
            }
        }
        serde_json::Value::String(s) => Ok(Value::String(lua.create_string(s)?)),
        serde_json::Value::Array(arr) => {
            let table = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                table.raw_set(i + 1, json_to_lua_value(lua, v)?)?;
            }
            Ok(Value::Table(table))
        }
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (k, v) in map {
                table.raw_set(k.as_str(), json_to_lua_value(lua, v)?)?;
            }
            Ok(Value::Table(table))
        }
    }
}

/// Build the `genesis.json` bridge table with `encode` and `decode` functions.
pub fn make_json_bridge(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;

    table.set(
        "encode",
        lua.create_function(|_lua, value: Value| {
            let json_value = lua_value_to_json(value)?;
            serde_json::to_string(&json_value)
                .map_err(|e| mlua::Error::external(format!("JSON encode error: {e}")))
        })?,
    )?;

    table.set(
        "decode",
        lua.create_function(|lua, s: String| {
            let json_value: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| mlua::Error::external(format!("JSON decode error: {e}")))?;
            json_to_lua_value(lua, &json_value)
        })?,
    )?;

    // Pretty-print variant of encode.
    table.set(
        "encode_pretty",
        lua.create_function(|_lua, value: Value| {
            let json_value = lua_value_to_json(value)?;
            serde_json::to_string_pretty(&json_value)
                .map_err(|e| mlua::Error::external(format!("JSON encode error: {e}")))
        })?,
    )?;

    Ok(table)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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
                disabled_plugins: Vec::new(),
                plugin_verbose: None,
                config_values: BTreeMap::new(),
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
            msg.contains("JSON decode error"),
            "error should mention JSON decode: {msg}"
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
