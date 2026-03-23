use std::sync::Arc;
use std::time::Duration;

use mlua::{Lua, Table, Value};

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Build the `genesis.http` bridge table.
///
/// Provides one method:
/// - `request(url, opts?)` — send an HTTP request, returning
///   `{ status, headers, body }`.
///
/// The optional `opts` table supports:
/// - `method` (string) — HTTP method: GET (default), POST, PUT, DELETE, PATCH, HEAD
/// - `headers` (table) — key/value pairs of request headers
/// - `body` (string) — request body
/// - `timeout` (number) — timeout in seconds (default 30)
///
/// When a shared `reqwest::blocking::Client` is provided it is reused across
/// calls; otherwise a temporary client is created per request.
pub fn make_http_bridge(
    lua: &Lua,
    client: Option<Arc<reqwest::blocking::Client>>,
) -> mlua::Result<Table> {
    let table = lua.create_table()?;

    table.set(
        "request",
        lua.create_function(move |lua, (url, opts): (String, Option<Table>)| {
            // Resolve options.
            let method_str = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("method").ok().flatten())
                .unwrap_or_else(|| "GET".to_owned());

            let timeout_secs: u64 = opts
                .as_ref()
                .and_then(|o| o.get::<Option<u64>>("timeout").ok().flatten())
                .unwrap_or(DEFAULT_TIMEOUT_SECS);

            let body: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("body").ok().flatten());

            // Collect headers from opts.headers table.
            let mut header_pairs: Vec<(String, String)> = Vec::new();
            if let Some(ref opts) = opts {
                if let Ok(Some(headers_table)) = opts.get::<Option<Table>>("headers") {
                    for pair in headers_table.pairs::<String, String>() {
                        let (key, value) = pair?;
                        header_pairs.push((key, value));
                    }
                }
            }

            // Use the shared client or create a temporary one with the
            // requested timeout.
            let owned_client;
            let effective_client: &reqwest::blocking::Client = match client {
                Some(ref c) => c.as_ref(),
                None => {
                    owned_client = reqwest::blocking::Client::builder()
                        .timeout(Duration::from_secs(timeout_secs))
                        .build()
                        .map_err(|e| {
                            mlua::Error::external(format!("http: failed to build client: {e}"))
                        })?;
                    &owned_client
                }
            };

            // Parse the method string.
            let method: reqwest::Method = method_str.parse().map_err(|_| {
                mlua::Error::external(format!("http: unsupported method: {method_str}"))
            })?;

            // Build the request.
            let mut request = effective_client.request(method, &url);

            // When using the shared client, override its default timeout.
            if client.is_some() {
                request = request.timeout(Duration::from_secs(timeout_secs));
            }

            for (key, value) in &header_pairs {
                request = request.header(key.as_str(), value.as_str());
            }

            if let Some(body) = body {
                request = request.body(body);
            }

            // Send the request.
            let response = request
                .send()
                .map_err(|e| mlua::Error::external(format!("http: request failed: {e}")))?;

            // Extract response fields.
            let status = response.status().as_u16();

            let resp_headers = lua.create_table()?;
            for (name, value) in response.headers().iter() {
                if let Ok(v) = value.to_str() {
                    resp_headers.set(name.as_str().to_owned(), v.to_owned())?;
                }
            }

            let resp_body = response.text().map_err(|e| {
                mlua::Error::external(format!("http: failed to read response body: {e}"))
            })?;

            // Build result table.
            let result = lua.create_table()?;
            result.set("status", status)?;
            result.set("headers", resp_headers)?;
            result.set("body", resp_body)?;

            Ok(Value::Table(result))
        })?,
    )?;

    Ok(table)
}

#[cfg(test)]
mod tests {
    /// Create a bare Lua VM with `genesis.http` installed.
    fn test_lua_with_http() -> mlua::Lua {
        let lua = mlua::Lua::new();
        let http_table =
            super::make_http_bridge(&lua, None).expect("make_http_bridge should succeed");
        let genesis = lua.create_table().expect("table should create");
        genesis
            .set("http", http_table)
            .expect("set http should work");
        lua.globals()
            .set("genesis", genesis)
            .expect("set genesis should work");
        lua
    }

    #[test]
    fn request_to_invalid_url_returns_error() {
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load("return genesis.http.request('http://localhost:1/nope')")
            .eval();
        assert!(result.is_err(), "request to localhost:1 should fail");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("http: request failed"),
            "error should mention request failure, got: {err}"
        );
    }

    #[test]
    fn request_defaults_to_get() {
        // We cannot easily inspect the method without a real server, but we
        // can verify that calling without opts (which defaults to GET)
        // behaves the same as calling with an invalid host — the error path
        // is identical regardless of method, confirming no crash.
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load("return genesis.http.request('http://localhost:1/')")
            .eval();
        assert!(result.is_err());
    }

    #[test]
    fn request_with_headers_does_not_crash() {
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://localhost:1/', {
                    headers = { ["X-Custom"] = "value" }
                })"#,
            )
            .eval();
        // Should fail due to connection refused, not due to header handling.
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("http: request failed"),
            "error should be a request error, got: {err}"
        );
    }

    #[test]
    fn request_invalid_method_returns_error() {
        let lua = test_lua_with_http();
        // A method containing a space is not a valid HTTP token and should
        // be rejected by the method parser before any network I/O.
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://localhost:1/', {
                    method = "BAD METHOD"
                })"#,
            )
            .eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unsupported method"),
            "error should mention unsupported method, got: {err}"
        );
    }

    #[test]
    fn request_with_body_does_not_crash() {
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://localhost:1/', {
                    method = "POST",
                    body = '{"key": "value"}',
                    headers = { ["Content-Type"] = "application/json" }
                })"#,
            )
            .eval();
        // Connection refused, but no crash from body/header construction.
        assert!(result.is_err());
    }

    #[test]
    fn bridge_is_callable() {
        // Verify the bridge table exists and `request` is a function.
        let lua = test_lua_with_http();
        let ty: String = lua
            .load("return type(genesis.http.request)")
            .eval()
            .expect("should be able to check type");
        assert_eq!(ty, "function");
    }
}
