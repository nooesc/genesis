use std::sync::{Arc, OnceLock};
use std::time::Duration;

use mlua::{Lua, Table, Value};

/// Default timeout used by the cached fallback client (when no shared client
/// is provided and no per-request timeout is specified).
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Return a lazily-initialized fallback HTTP client.
///
/// Used when `make_http_bridge` was not given an explicit shared client.
fn fallback_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        genesis_tools::http::build_blocking_client(Duration::from_secs(DEFAULT_TIMEOUT_SECS), |b| b)
    })
}

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
/// - `timeout` (number) — per-request timeout in seconds (omit to use the
///   client's default)
/// - `allow_private` (boolean) — skip SSRF validation for private/LAN IPs
///   (default: false). Use only for known-internal services like Home Assistant.
///
/// URLs are validated before sending to block SSRF (private IPs, localhost,
/// cloud metadata endpoints, non-HTTP schemes) unless `allow_private` is set.
///
/// When a shared `reqwest::blocking::Client` is provided it is reused across
/// calls; otherwise a lazily-initialized cached fallback client is used.
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

            let timeout_secs: Option<u64> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<u64>>("timeout").ok().flatten());

            let body: Option<String> = opts
                .as_ref()
                .and_then(|o| o.get::<Option<String>>("body").ok().flatten());

            let allow_private: bool = opts
                .as_ref()
                .and_then(|o| o.get::<Option<bool>>("allow_private").ok().flatten())
                .unwrap_or(false);

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

            // Validate the URL to prevent SSRF (skip when allow_private is set
            // for known-internal services like Home Assistant on a LAN).
            if !allow_private {
                genesis_tools::url_safety::validate_url(&url)
                    .map_err(|e| mlua::Error::external(format!("http: blocked URL: {e}")))?;
            }

            // Use the shared client or the cached fallback.
            let effective_client: &reqwest::blocking::Client = match client {
                Some(ref c) => c.as_ref(),
                None => fallback_client(),
            };

            // Parse the method string.
            let method: reqwest::Method = method_str.parse().map_err(|_| {
                mlua::Error::external(format!("http: unsupported method: {method_str}"))
            })?;

            // Build the request.
            let mut request = effective_client.request(method, &url);

            // Only apply per-request timeout when explicitly provided.
            if let Some(t) = timeout_secs {
                request = request.timeout(Duration::from_secs(t));
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
    fn request_blocks_localhost() {
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load("return genesis.http.request('http://localhost:1/nope')")
            .eval();
        assert!(result.is_err(), "request to localhost should be blocked");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("http: blocked URL"),
            "error should mention blocked URL, got: {err}"
        );
    }

    #[test]
    fn request_blocks_internal_ips() {
        let lua = test_lua_with_http();
        for url in [
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
        ] {
            let script = format!("return genesis.http.request('{url}')");
            let result: mlua::Result<mlua::Value> = lua.load(&script).eval();
            assert!(result.is_err(), "request to {url} should be blocked");
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("http: blocked URL"),
                "error for {url} should mention blocked URL, got: {err}"
            );
        }
    }

    #[test]
    fn request_to_unreachable_host_returns_error() {
        // Use a public but unreachable host:port to test the network error path
        // without being blocked by URL validation.
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://192.0.2.1:1/', {
                    timeout = 1
                })"#,
            )
            .eval();
        assert!(result.is_err());
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
        // goes through the same code path. Here we verify URL validation
        // fires before any method handling.
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load("return genesis.http.request('http://localhost:1/')")
            .eval();
        assert!(result.is_err());
    }

    #[test]
    fn request_with_headers_does_not_crash() {
        // Use a non-internal URL so we test header construction (blocked by
        // network timeout rather than URL validation).
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://192.0.2.1:1/', {
                    headers = { ["X-Custom"] = "value" },
                    timeout = 1
                })"#,
            )
            .eval();
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
        // Use a public URL so URL validation passes first.
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://192.0.2.1:1/', {
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
        // Use a non-internal URL so we exercise the body/header path.
        let lua = test_lua_with_http();
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://192.0.2.1:1/', {
                    method = "POST",
                    body = '{"key": "value"}',
                    headers = { ["Content-Type"] = "application/json" },
                    timeout = 1
                })"#,
            )
            .eval();
        // Timeout/connection error, but no crash from body/header construction.
        assert!(result.is_err());
    }

    #[test]
    fn allow_private_skips_ssrf_validation() {
        let lua = test_lua_with_http();
        // With allow_private = true, the request should NOT be blocked by URL
        // validation. It will fail with a network error instead (connection
        // refused) which proves SSRF validation was skipped.
        let result: mlua::Result<mlua::Value> = lua
            .load(
                r#"return genesis.http.request('http://192.168.1.1:8123/api/services/notify/me', {
                    allow_private = true,
                    timeout = 1
                })"#,
            )
            .eval();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Should be a network error, NOT an SSRF "blocked URL" error.
        assert!(
            err.contains("http: request failed"),
            "error should be a network error (not blocked URL), got: {err}"
        );
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
