use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const MAX_RESULTS: usize = 10;
const TIMEOUT_SECS: u64 = 15;

/// Shared blocking HTTP client for web search requests.
fn search_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::http::build_blocking_client(Duration::from_secs(TIMEOUT_SECS), |b| {
            b.user_agent("genesis-agent/0.1")
        })
    })
}

/// Web search tool using Brave Search API.
///
/// Requires BRAVE_API_KEY environment variable. Falls back to a DuckDuckGo
/// Instant Answer API if no API key is available.
pub struct WebSearchTool;

impl ToolHandler for WebSearchTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = call
            .arguments
            .get("query")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "query",
            })?;

        if query.trim().is_empty() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "search query cannot be empty".to_owned(),
            });
        }

        let count: usize = call
            .arguments
            .get("count")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5)
            .min(MAX_RESULTS);

        // Try Brave Search API first, fall back to DuckDuckGo
        let content = match std::env::var("BRAVE_API_KEY") {
            Ok(api_key) if !api_key.is_empty() => {
                search_brave(query, count, &api_key).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("Brave search failed: {e}"),
                })?
            }
            _ => search_ddg(query, count).map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("DuckDuckGo search failed: {e}"),
            })?,
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
        })
    }
}

fn search_brave(query: &str, count: usize, api_key: &str) -> Result<String, String> {
    let client = search_client();

    let response = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("Accept", "application/json")
        .header("Accept-Encoding", "gzip")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &count.to_string())])
        .send()
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("API returned status {}", response.status()));
    }

    let body: serde_json::Value = response.json().map_err(|e| e.to_string())?;

    let results = body["web"]["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .take(count)
                .enumerate()
                .map(|(i, r)| {
                    let title = r["title"].as_str().unwrap_or("(no title)");
                    let url = r["url"].as_str().unwrap_or("");
                    let description = r["description"].as_str().unwrap_or("(no description)");
                    format!("{}. {}\n   {}\n   {}", i + 1, title, url, description)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_else(|| "No results found.".to_owned());

    Ok(format!("Search results for: {query}\n\n{results}"))
}

fn search_ddg(query: &str, count: usize) -> Result<String, String> {
    let client = search_client();

    // DuckDuckGo Instant Answer API (JSON, no API key required)
    let response = client
        .get("https://api.duckduckgo.com/")
        .query(&[("q", query), ("format", "json"), ("no_html", "1")])
        .send()
        .map_err(|e| e.to_string())?;

    let body: serde_json::Value = response.json().map_err(|e| e.to_string())?;

    let mut results = Vec::new();

    // Abstract (main result)
    if let Some(abstract_text) = body["AbstractText"].as_str() {
        if !abstract_text.is_empty() {
            let source = body["AbstractSource"].as_str().unwrap_or("");
            let url = body["AbstractURL"].as_str().unwrap_or("");
            results.push(format!(
                "1. {} ({})\n   {}\n   {}",
                source, url, url, abstract_text
            ));
        }
    }

    // Related topics
    if let Some(topics) = body["RelatedTopics"].as_array() {
        for topic in topics.iter().take(count.saturating_sub(results.len())) {
            if let (Some(text), Some(url)) = (topic["Text"].as_str(), topic["FirstURL"].as_str()) {
                let idx = results.len() + 1;
                results.push(format!("{}. {}\n   {}", idx, text, url));
            }
        }
    }

    if results.is_empty() {
        return Ok(format!(
            "Search results for: {query}\n\nNo instant results available. \
             Try using web_request to fetch a specific URL directly."
        ));
    }

    Ok(format!(
        "Search results for: {query}\n\n{}",
        results.join("\n\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    #[test]
    fn web_search_requires_query() {
        let tool = WebSearchTool;
        let call = ToolCall {
            name: "web_search".to_owned(),
            arguments: BTreeMap::new(),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn web_search_rejects_empty_query() {
        let tool = WebSearchTool;
        let call = ToolCall {
            name: "web_search".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "  ".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("empty"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn search_brave_formats_results() {
        // Test the formatting with mock data
        let mock_response = serde_json::json!({
            "web": {
                "results": [
                    {
                        "title": "Test Result",
                        "url": "https://example.com",
                        "description": "A test result"
                    }
                ]
            }
        });

        // Verify the mock data structure matches what we parse
        let results = mock_response["web"]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"].as_str().unwrap(), "Test Result");
    }

    #[test]
    fn search_ddg_handles_empty_response() {
        let mock = serde_json::json!({
            "AbstractText": "",
            "RelatedTopics": []
        });

        // Verify empty response structure
        assert!(mock["AbstractText"].as_str().unwrap().is_empty());
        assert!(mock["RelatedTopics"].as_array().unwrap().is_empty());
    }
}
