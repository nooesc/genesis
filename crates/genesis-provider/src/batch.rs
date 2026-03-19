//! OpenAI Batch API client for cost-effective bulk completions.
//!
//! Sends chat completion requests via OpenAI's Batch API for a 50% cost
//! discount. Requests are uploaded as JSONL, processed asynchronously
//! (up to 24h), and results downloaded when ready.
//!
//! ## Flow
//! 1. Generate JSONL from `Vec<ChatCompletionRequest>`
//! 2. Upload JSONL file via `POST /v1/files`
//! 3. Create batch via `POST /v1/batches`
//! 4. Poll for completion via `GET /v1/batches/{id}`
//! 5. Download results via `GET /v1/files/{output_file_id}/content`
//! 6. Parse results back to `Vec<ChatCompletionResponse>`

use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::api_types::{ChatCompletionRequest, ChatCompletionResponse};
use crate::error::ProviderError;

/// A single line in the Batch API JSONL input file.
#[derive(Debug, Serialize)]
struct BatchRequestLine {
    custom_id: String,
    method: &'static str,
    url: &'static str,
    body: serde_json::Value,
}

/// Batch API status response.
#[derive(Debug, Deserialize)]
struct BatchStatus {
    id: String,
    status: String,
    #[serde(default)]
    output_file_id: Option<String>,
    #[serde(default)]
    error_file_id: Option<String>,
    #[serde(default)]
    request_counts: Option<BatchRequestCounts>,
}

#[derive(Debug, Deserialize)]
struct BatchRequestCounts {
    #[serde(default)]
    total: u32,
    #[serde(default)]
    completed: u32,
    #[serde(default)]
    #[allow(dead_code)]
    failed: u32,
}

/// A single line in the Batch API JSONL output file.
#[derive(Debug, Deserialize)]
struct BatchResultLine {
    custom_id: String,
    response: Option<BatchResultResponse>,
    error: Option<BatchResultError>,
}

#[derive(Debug, Deserialize)]
struct BatchResultResponse {
    status_code: u16,
    body: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct BatchResultError {
    code: String,
    message: String,
}

/// File upload response.
#[derive(Debug, Deserialize)]
struct FileUploadResponse {
    id: String,
}

/// Client for OpenAI's Batch API.
pub struct BatchClient {
    http: reqwest::Client,
    base_url: String,
}

impl BatchClient {
    /// Create a new Batch API client.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ProviderError> {
        let mut headers = HeaderMap::new();
        if !api_key.is_empty() {
            let auth_value = format!("Bearer {api_key}");
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_value).map_err(|_| ProviderError::MissingApiKey {
                    env_var: "API key contains invalid header characters".to_owned(),
                })?,
            );
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .build()?;

        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Execute a batch of chat completion requests via the Batch API.
    ///
    /// Returns results mapped by their index in the input vector.
    /// Polls for completion with exponential backoff up to `timeout`.
    pub async fn execute_batch(
        &self,
        requests: Vec<ChatCompletionRequest>,
        timeout: Duration,
    ) -> Result<Vec<Result<ChatCompletionResponse, ProviderError>>, ProviderError> {
        if requests.is_empty() {
            return Ok(vec![]);
        }

        let total = requests.len();
        info!(count = total, "submitting batch to OpenAI Batch API");

        // 1. Generate JSONL
        let jsonl = generate_jsonl(&requests)?;

        // 2. Upload file
        let file_id = self.upload_file(&jsonl).await?;
        debug!(file_id = file_id.as_str(), "uploaded batch input file");

        // 3. Create batch
        let batch_id = self.create_batch(&file_id).await?;
        info!(
            batch_id = batch_id.as_str(),
            file_id = file_id.as_str(),
            "batch created"
        );

        // 4. Poll for completion
        let status = self.poll_until_complete(&batch_id, timeout).await?;
        info!(
            batch_id = batch_id.as_str(),
            status = status.status.as_str(),
            "batch completed"
        );

        if let Some(ref error_file) = status.error_file_id {
            tracing::warn!(error_file_id = %error_file, "batch completed with errors — download error file for details");
        }

        // 5. Download and parse results
        let output_file_id = status.output_file_id.ok_or_else(|| {
            ProviderError::ApiError {
                status: 500,
                body: format!("batch {} completed but no output file", batch_id),
            }
        })?;

        let results = self.download_results(&output_file_id, total).await?;
        Ok(results)
    }

    /// Upload JSONL content as a file via the OpenAI Files API.
    async fn upload_file(&self, jsonl: &str) -> Result<String, ProviderError> {
        let form = reqwest::multipart::Form::new()
            .text("purpose", "batch")
            .part(
                "file",
                reqwest::multipart::Part::text(jsonl.to_owned())
                    .file_name("batch_input.jsonl")
                    .mime_str("application/jsonl")
                    .map_err(|e| ProviderError::ApiError {
                        status: 400,
                        body: format!("failed to create multipart: {e}"),
                    })?,
            );

        let resp = self
            .http
            .post(format!("{}/files", self.base_url))
            .multipart(form)
            .send()
            .await
?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        let upload: FileUploadResponse = resp.json().await.map_err(|e| {
            ProviderError::ApiError {
                status: 500,
                body: format!("failed to parse file upload response: {e}"),
            }
        })?;
        Ok(upload.id)
    }

    async fn create_batch(&self, file_id: &str) -> Result<String, ProviderError> {
        let body = serde_json::json!({
            "input_file_id": file_id,
            "endpoint": "/v1/chat/completions",
            "completion_window": "24h",
        });

        let resp = self
            .http
            .post(format!("{}/batches", self.base_url))
            .json(&body)
            .send()
            .await
?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        let batch: BatchStatus = resp.json().await.map_err(|e| {
            ProviderError::ApiError {
                status: 500,
                body: format!("failed to parse batch creation response: {e}"),
            }
        })?;
        Ok(batch.id)
    }

    async fn poll_until_complete(
        &self,
        batch_id: &str,
        timeout: Duration,
    ) -> Result<BatchStatus, ProviderError> {
        let started = std::time::Instant::now();
        let mut delay = Duration::from_secs(5);
        let max_delay = Duration::from_secs(60);

        loop {
            if started.elapsed() > timeout {
                return Err(ProviderError::ApiError {
                    status: 408,
                    body: format!(
                        "batch {batch_id} did not complete within {}s",
                        timeout.as_secs()
                    ),
                });
            }

            let resp = self
                .http
                .get(format!("{}/batches/{}", self.base_url, batch_id))
                .send()
                .await
    ?;

            let status_code = resp.status();
            if !status_code.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ApiError {
                    status: status_code.as_u16(),
                    body,
                });
            }

            let batch: BatchStatus = resp.json().await.map_err(|e| {
                ProviderError::ApiError {
                    status: 500,
                    body: format!("failed to parse batch status: {e}"),
                }
            })?;

            match batch.status.as_str() {
                "completed" => return Ok(batch),
                "failed" | "expired" | "cancelled" => {
                    return Err(ProviderError::ApiError {
                        status: 500,
                        body: format!(
                            "batch {batch_id} terminated with status: {}",
                            batch.status
                        ),
                    });
                }
                status => {
                    let counts = batch.request_counts.as_ref();
                    debug!(
                        batch_id,
                        status,
                        completed = counts.map(|c| c.completed).unwrap_or(0),
                        total = counts.map(|c| c.total).unwrap_or(0),
                        "batch in progress"
                    );
                }
            }

            // Exponential backoff capped at max_delay.
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(max_delay);
        }
    }

    async fn download_results(
        &self,
        output_file_id: &str,
        expected_count: usize,
    ) -> Result<Vec<Result<ChatCompletionResponse, ProviderError>>, ProviderError> {
        let resp = self
            .http
            .get(format!("{}/files/{}/content", self.base_url, output_file_id))
            .send()
            .await
?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        let content = resp.text().await.map_err(|e| {
            ProviderError::ApiError {
                status: 500,
                body: format!("failed to read batch output: {e}"),
            }
        })?;

        parse_batch_results(&content, expected_count)
    }
}

/// Generate JSONL from chat completion requests.
///
/// Each line has a `custom_id` that maps to the request's index.
pub fn generate_jsonl(requests: &[ChatCompletionRequest]) -> Result<String, ProviderError> {
    let mut lines = Vec::with_capacity(requests.len());
    for (i, request) in requests.iter().enumerate() {
        let body = serde_json::to_value(request).map_err(|e| ProviderError::ApiError {
            status: 400,
            body: format!("failed to serialize request {i}: {e}"),
        })?;
        let line = BatchRequestLine {
            custom_id: format!("req-{i}"),
            method: "POST",
            url: "/v1/chat/completions",
            body,
        };
        let json = serde_json::to_string(&line).map_err(|e| ProviderError::ApiError {
            status: 400,
            body: format!("failed to serialize JSONL line {i}: {e}"),
        })?;
        lines.push(json);
    }
    // JSONL spec requires each record to end with a newline.
    let mut output = lines.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

/// Parse JSONL batch output into ordered results.
pub fn parse_batch_results(
    content: &str,
    expected_count: usize,
) -> Result<Vec<Result<ChatCompletionResponse, ProviderError>>, ProviderError> {
    let mut result_map: HashMap<usize, Result<ChatCompletionResponse, ProviderError>> =
        HashMap::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let result_line: BatchResultLine = serde_json::from_str(line).map_err(|e| {
            ProviderError::ApiError {
                status: 500,
                body: format!("failed to parse batch result line: {e}"),
            }
        })?;

        // Extract index from custom_id "req-N"
        let index = match result_line
            .custom_id
            .strip_prefix("req-")
            .and_then(|s| s.parse::<usize>().ok())
        {
            Some(idx) => idx,
            None => {
                tracing::warn!(
                    custom_id = %result_line.custom_id,
                    "unrecognized custom_id in batch result — result will be discarded"
                );
                continue;
            }
        };

        if let Some(err) = result_line.error {
            result_map.insert(
                index,
                Err(ProviderError::ApiError {
                    status: 500,
                    body: format!("batch error [{}]: {}", err.code, err.message),
                }),
            );
        } else if let Some(resp) = result_line.response {
            if resp.status_code == 200 {
                match serde_json::from_value::<ChatCompletionResponse>(resp.body) {
                    Ok(completion) => {
                        result_map.insert(index, Ok(completion));
                    }
                    Err(e) => {
                        result_map.insert(
                            index,
                            Err(ProviderError::ApiError {
                                status: 500,
                                body: format!("failed to parse completion response: {e}"),
                            }),
                        );
                    }
                }
            } else {
                let body_str = serde_json::to_string(&resp.body)
                    .unwrap_or_else(|e| format!("<serialization error: {e}>"));
                result_map.insert(
                    index,
                    Err(ProviderError::ApiError {
                        status: resp.status_code,
                        body: body_str,
                    }),
                );
            }
        } else {
            warn!(
                custom_id = result_line.custom_id.as_str(),
                "batch result line has neither response nor error"
            );
        }
    }

    // Assemble ordered results.
    let mut results = Vec::with_capacity(expected_count);
    for i in 0..expected_count {
        results.push(result_map.remove(&i).unwrap_or_else(|| {
            Err(ProviderError::ApiError {
                status: 500,
                body: format!("missing result for request {i}"),
            })
        }));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::ChatMessage;

    #[test]
    fn generate_jsonl_produces_valid_lines() {
        let requests = vec![
            ChatCompletionRequest::new("gpt-4.1-mini", vec![ChatMessage::user("Hello")]),
            ChatCompletionRequest::new("gpt-4.1-mini", vec![ChatMessage::user("World")]),
        ];
        let jsonl = generate_jsonl(&requests).unwrap();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 2);

        // Parse each line as JSON and verify structure.
        for (i, line) in lines.iter().enumerate() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["custom_id"], format!("req-{i}"));
            assert_eq!(parsed["method"], "POST");
            assert_eq!(parsed["url"], "/v1/chat/completions");
            assert!(parsed["body"]["model"].is_string());
            assert!(parsed["body"]["messages"].is_array());
        }
    }

    #[test]
    fn generate_jsonl_empty_input() {
        let jsonl = generate_jsonl(&[]).unwrap();
        assert!(jsonl.is_empty());
    }

    #[test]
    fn parse_batch_results_success() {
        let output = r#"{"custom_id":"req-0","response":{"status_code":200,"body":{"id":"chatcmpl-abc","choices":[{"index":0,"message":{"role":"assistant","content":"Hello!"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}},"error":null}
{"custom_id":"req-1","response":{"status_code":200,"body":{"id":"chatcmpl-def","choices":[{"index":0,"message":{"role":"assistant","content":"World!"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}},"error":null}"#;

        let results = parse_batch_results(output, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());

        let r0 = results[0].as_ref().unwrap();
        assert_eq!(r0.choices[0].message.content_text().unwrap(), "Hello!");
    }

    #[test]
    fn parse_batch_results_with_error() {
        let output = r#"{"custom_id":"req-0","response":null,"error":{"code":"rate_limit","message":"Rate limit exceeded"}}"#;

        let results = parse_batch_results(output, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }

    #[test]
    fn parse_batch_results_missing_result() {
        // Only 1 result for 2 expected requests.
        let output = r#"{"custom_id":"req-1","response":{"status_code":200,"body":{"id":"chatcmpl-abc","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}]}},"error":null}"#;

        let results = parse_batch_results(output, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].is_err()); // Missing req-0
        assert!(results[1].is_ok()); // req-1 present
    }

    #[test]
    fn parse_batch_results_out_of_order() {
        let output = r#"{"custom_id":"req-1","response":{"status_code":200,"body":{"id":"b","choices":[{"index":0,"message":{"role":"assistant","content":"Second"},"finish_reason":"stop"}]}},"error":null}
{"custom_id":"req-0","response":{"status_code":200,"body":{"id":"a","choices":[{"index":0,"message":{"role":"assistant","content":"First"},"finish_reason":"stop"}]}},"error":null}"#;

        let results = parse_batch_results(output, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].as_ref().unwrap().choices[0]
                .message
                .content_text()
                .unwrap(),
            "First"
        );
        assert_eq!(
            results[1].as_ref().unwrap().choices[0]
                .message
                .content_text()
                .unwrap(),
            "Second"
        );
    }
}
