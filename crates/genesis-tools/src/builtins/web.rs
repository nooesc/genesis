use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const TIMEOUT_SECS: u64 = 30;

/// Shared blocking HTTP client for web requests.
fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::http::build_blocking_client(Duration::from_secs(TIMEOUT_SECS), |b| {
            b.user_agent("genesis-agent/0.1")
        })
    })
}

pub struct WebRequestTool;

impl ToolHandler for WebRequestTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let url = call
            .arguments
            .get("url")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "url",
            })?;

        let method = call
            .arguments
            .get("method")
            .map(|m| m.to_uppercase())
            .unwrap_or_else(|| "GET".to_owned());

        let body = call.arguments.get("body");
        let headers_json = call.arguments.get("headers");

        // SSRF protection: validate URL before making request
        crate::url_safety::validate_url(url).map_err(|reason| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason,
        })?;

        let client = http_client();

        let mut request = match method.as_str() {
            "GET" => client.get(url),
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "DELETE" => client.delete(url),
            "PATCH" => client.patch(url),
            "HEAD" => client.head(url),
            _ => {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("unsupported HTTP method: {method}"),
                })
            }
        };

        // Apply custom headers from JSON object: {"Authorization": "Bearer ...", ...}
        if let Some(headers_str) = headers_json {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(headers_str) {
                if let Some(obj) = parsed.as_object() {
                    for (key, value) in obj {
                        if let Some(val_str) = value.as_str() {
                            request = request.header(key.as_str(), val_str);
                        }
                    }
                }
            }
        }

        if let Some(body_content) = body {
            // Only set Content-Type if not already set via custom headers
            let has_content_type = headers_json
                .map(|h| h.to_ascii_lowercase().contains("content-type"))
                .unwrap_or(false);
            if !has_content_type {
                request = request.header("Content-Type", "application/json");
            }
            request = request.body(body_content.to_owned());
        }

        let response = request.send().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("HTTP request failed: {e}"),
        })?;

        let status = response.status().as_u16();
        let headers: Vec<String> = response
            .headers()
            .iter()
            .filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "content-type" | "content-length" | "location" | "server"
                )
            })
            .map(|(name, value)| {
                format!("{}: {}", name, value.to_str().unwrap_or("<binary>"))
            })
            .collect();

        let response_body = response.text().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read response body: {e}"),
        })?;

        let truncated = if response_body.len() > MAX_RESPONSE_BYTES {
            // Find the nearest char boundary at or before the byte limit
            let mut end = MAX_RESPONSE_BYTES;
            while end > 0 && !response_body.is_char_boundary(end) {
                end -= 1;
            }
            let mut t = response_body[..end].to_string();
            t.push_str("\n... (response truncated)");
            t
        } else {
            response_body
        };

        let mut content = format!("HTTP {status}\n");
        for header in &headers {
            content.push_str(header);
            content.push('\n');
        }
        content.push('\n');
        content.push_str(&truncated);

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("status".to_owned(), status.to_string()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    #[test]
    fn web_request_requires_url() {
        let tool = WebRequestTool;
        let call = ToolCall {
            name: "web_request".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn web_request_rejects_unsupported_method() {
        let tool = WebRequestTool;
        let call = ToolCall {
            name: "web_request".to_owned(),
            arguments: BTreeMap::from([
                ("url".to_owned(), "https://example.com".to_owned()),
                ("method".to_owned(), "TRACE".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unsupported HTTP method"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn web_request_handles_connection_failure() {
        let tool = WebRequestTool;
        let call = ToolCall {
            name: "web_request".to_owned(),
            arguments: BTreeMap::from([
                // Use a public IP with a closed port (not a private IP)
                ("url".to_owned(), "http://203.0.113.1:1".to_owned()),
                ("method".to_owned(), "GET".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn web_request_blocks_localhost() {
        let tool = WebRequestTool;
        let call = ToolCall {
            name: "web_request".to_owned(),
            arguments: BTreeMap::from([("url".to_owned(), "http://localhost:8080".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("blocked"));
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    #[test]
    fn web_request_blocks_private_ips() {
        let tool = WebRequestTool;
        for url in [
            "http://10.0.0.1",
            "http://172.16.0.1",
            "http://192.168.1.1",
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:8080",
        ] {
            let call = ToolCall {
                name: "web_request".to_owned(),
                arguments: BTreeMap::from([("url".to_owned(), url.to_owned())]),
            };
            let err = tool.run(&call, &ctx()).unwrap_err();
            assert!(
                matches!(err, ToolError::ExecutionFailed { .. }),
                "expected {url} to be blocked"
            );
        }
    }

    #[test]
    fn web_request_blocks_non_http_schemes() {
        let tool = WebRequestTool;
        let call = ToolCall {
            name: "web_request".to_owned(),
            arguments: BTreeMap::from([("url".to_owned(), "file:///etc/passwd".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
