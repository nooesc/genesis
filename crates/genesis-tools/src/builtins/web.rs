use std::collections::BTreeMap;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const TIMEOUT_SECS: u64 = 30;

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

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .user_agent("genesis-agent/0.1")
            .build()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to create HTTP client: {e}"),
            })?;

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
            if headers_json.is_none()
                || !headers_json
                    .unwrap()
                    .to_lowercase()
                    .contains("content-type")
            {
                request = request.header("Content-Type", "application/json");
            }
            request = request.body(body_content.clone());
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
            let mut t = response_body[..MAX_RESPONSE_BYTES].to_string();
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
                ("url".to_owned(), "http://localhost:1".to_owned()),
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
                ("url".to_owned(), "http://127.0.0.1:1".to_owned()),
                ("method".to_owned(), "GET".to_owned()),
            ]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
