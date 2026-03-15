use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const TIMEOUT_SECS: u64 = 120;

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::http::build_blocking_client(Duration::from_secs(TIMEOUT_SECS), |b| b)
    })
}

/// Image generation tool using OpenAI-compatible DALL-E API.
///
/// Generates images from text prompts. Supports size, quality, and style
/// configuration. Saves the generated image to disk and returns the file path.
pub struct ImageGenerationTool;

impl ToolHandler for ImageGenerationTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let prompt = call
            .arguments
            .get("prompt")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "prompt",
            })?;

        let output_path =
            call.arguments
                .get("output_path")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "output_path",
                })?;

        let model = call
            .arguments
            .get("model")
            .map(|s| s.as_str())
            .unwrap_or("dall-e-3");

        let size = call
            .arguments
            .get("size")
            .map(|s| s.as_str())
            .unwrap_or("1024x1024");

        let quality = call
            .arguments
            .get("quality")
            .map(|s| s.as_str())
            .unwrap_or("standard");

        let style = call
            .arguments
            .get("style")
            .map(|s| s.as_str())
            .unwrap_or("vivid");

        // Validate size.
        let valid_sizes = ["256x256", "512x512", "1024x1024", "1024x1792", "1792x1024"];
        if !valid_sizes.contains(&size) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("invalid size '{size}'. Valid: {}", valid_sizes.join(", ")),
            });
        }

        let api_base = call
            .arguments
            .get("api_base")
            .cloned()
            .or_else(|| std::env::var("OPENAI_API_BASE").ok())
            .unwrap_or_else(|| genesis_provider::OPENAI_BASE_URL.to_owned());

        let api_key = call
            .arguments
            .get("api_key")
            .cloned()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "No API key provided. Set OPENAI_API_KEY or pass api_key argument."
                    .to_owned(),
            })?;

        let url = format!("{}/images/generations", api_base.trim_end_matches('/'));

        let body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
            "size": size,
            "quality": quality,
            "style": style,
            "response_format": "b64_json"
        });

        let response = http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("API request failed: {e}"),
            })?;

        let status = response.status();
        let response_body: serde_json::Value =
            response.json().map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to parse API response: {e}"),
            })?;

        if !status.is_success() {
            let error_msg = response_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("API returned {status}: {error_msg}"),
            });
        }

        // Extract base64-encoded image data.
        let b64_data = response_body["data"][0]["b64_json"]
            .as_str()
            .ok_or_else(|| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: "API response missing image data".to_owned(),
            })?;

        let revised_prompt = response_body["data"][0]["revised_prompt"]
            .as_str()
            .unwrap_or("")
            .to_owned();

        // Decode and save to file.
        use base64::Engine;
        let image_bytes = base64::engine::general_purpose::STANDARD
            .decode(b64_data)
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to decode image data: {e}"),
            })?;

        std::fs::write(output_path, &image_bytes).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to write image to {output_path}: {e}"),
        })?;

        let content = if revised_prompt.is_empty() {
            format!(
                "Image generated and saved to {output_path} ({} bytes, {size})",
                image_bytes.len()
            )
        } else {
            format!(
                "Image generated and saved to {output_path} ({} bytes, {size})\nRevised prompt: {revised_prompt}",
                image_bytes.len()
            )
        };

        Ok(ToolOutput {
            content,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("output_path".to_owned(), output_path.clone()),
                ("model".to_owned(), model.to_owned()),
                ("size".to_owned(), size.to_owned()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    fn make_call(args: Vec<(&str, &str)>) -> ToolCall {
        ToolCall {
            name: "image_generation".to_owned(),
            arguments: args
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn error_on_missing_prompt() {
        let tool = ImageGenerationTool;
        let call = make_call(vec![("output_path", "/tmp/test.png")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn error_on_missing_output_path() {
        let tool = ImageGenerationTool;
        let call = make_call(vec![("prompt", "a cat")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn error_on_invalid_size() {
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let tool = ImageGenerationTool;
        let call = make_call(vec![
            ("prompt", "a cat"),
            ("output_path", "/tmp/test.png"),
            ("size", "100x100"),
        ]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("invalid size"));
    }

    #[test]
    fn error_on_missing_api_key() {
        std::env::remove_var("OPENAI_API_KEY");
        let tool = ImageGenerationTool;
        let call = make_call(vec![("prompt", "a cat"), ("output_path", "/tmp/test.png")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("API key"));
    }

    #[test]
    fn valid_sizes_accepted() {
        let sizes = ["256x256", "512x512", "1024x1024", "1024x1792", "1792x1024"];
        for size in sizes {
            // This will fail at the API call (no real key), but should get past
            // size validation.
            std::env::set_var("OPENAI_API_KEY", "test-key");
            let tool = ImageGenerationTool;
            let call = make_call(vec![
                ("prompt", "a cat"),
                ("output_path", "/tmp/test.png"),
                ("size", size),
            ]);
            let err = tool.run(&call, &ctx()).unwrap_err();
            // Should fail at API call, NOT at size validation.
            assert!(
                !err.to_string().contains("invalid size"),
                "size {size} should be valid"
            );
        }
    }
}
