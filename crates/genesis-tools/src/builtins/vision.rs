use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const TIMEOUT_SECS: u64 = 60;
const MAX_FILE_SIZE: u64 = 20 * 1024 * 1024; // 20 MB

/// Supported image file extensions.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "svg"];

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::http::build_blocking_client(Duration::from_secs(TIMEOUT_SECS), |b| b)
    })
}

/// Vision tool for analyzing images using multimodal LLM APIs.
///
/// Sends an image (file or URL) to an OpenAI-compatible vision endpoint and
/// returns the model's analysis. Useful for describing screenshots, reading
/// text from images, analyzing diagrams, and understanding visual content.
pub struct VisionTool;

impl ToolHandler for VisionTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let question = call
            .arguments
            .get("question")
            .map(|s| s.as_str())
            .unwrap_or("Describe this image in detail. If it contains text, transcribe it.");

        let model = call
            .arguments
            .get("model")
            .map(|s| s.as_str())
            .unwrap_or("gpt-4o");

        let max_tokens: u32 = call
            .arguments
            .get("max_tokens")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1024);

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

        // Build the image content part — either from file or URL.
        let image_content = if let Some(file_path) = call.arguments.get("file_path") {
            build_file_image_content(file_path, &call.name)?
        } else if let Some(image_url) = call.arguments.get("image_url") {
            serde_json::json!({
                "type": "image_url",
                "image_url": { "url": image_url }
            })
        } else {
            return Err(ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "file_path or image_url",
            });
        };

        let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));

        let body = serde_json::json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": question },
                    image_content
                ]
            }],
            "max_tokens": max_tokens
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

        let content = response_body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("(no response)")
            .to_owned();

        let mut metadata = BTreeMap::from([
            ("tool".to_owned(), call.name.clone()),
            ("model".to_owned(), model.to_owned()),
        ]);

        if let Some(fp) = call.arguments.get("file_path") {
            metadata.insert("file_path".to_owned(), fp.clone());
        }
        if let Some(url) = call.arguments.get("image_url") {
            metadata.insert("image_url".to_owned(), url.clone());
        }

        Ok(ToolOutput { content, metadata })
    }
}

/// Read and base64-encode an image file, returning the content part for the API.
fn build_file_image_content(
    file_path: &str,
    tool_name: &str,
) -> Result<serde_json::Value, ToolError> {
    let path = Path::new(file_path);

    if !path.is_file() {
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("`{file_path}` does not exist or is not a file"),
        });
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!(
                "unsupported image format '.{ext}'. Supported: {}",
                IMAGE_EXTENSIONS.join(", ")
            ),
        });
    }

    let metadata = std::fs::metadata(path).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!("failed to read file metadata: {e}"),
    })?;

    if metadata.len() > MAX_FILE_SIZE {
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!(
                "file is {} MB, exceeds 20 MB limit",
                metadata.len() / (1024 * 1024)
            ),
        });
    }

    let file_bytes = std::fs::read(path).map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!("failed to read image file: {e}"),
    })?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&file_bytes);

    let mime_type = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };

    Ok(serde_json::json!({
        "type": "image_url",
        "image_url": {
            "url": format!("data:{mime_type};base64,{b64}")
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        crate::test_utils::test_ctx()
    }

    fn make_call(args: Vec<(&str, &str)>) -> ToolCall {
        ToolCall {
            name: "vision".to_owned(),
            arguments: args
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn error_on_missing_image() {
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let tool = VisionTool;
        let call = make_call(vec![("question", "what is this?")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn error_on_nonexistent_file() {
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let tool = VisionTool;
        let call = make_call(vec![("file_path", "/nonexistent/image.png")]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn error_on_unsupported_format() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("data.pdf");
        fs::write(&file, "not an image").unwrap();

        std::env::set_var("OPENAI_API_KEY", "test-key");
        let tool = VisionTool;
        let call = make_call(vec![("file_path", &file.to_string_lossy())]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("unsupported image format"));
    }

    #[test]
    fn error_on_missing_api_key() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.png");
        fs::write(&file, "fake png data").unwrap();

        std::env::remove_var("OPENAI_API_KEY");
        let tool = VisionTool;
        let call = make_call(vec![("file_path", &file.to_string_lossy())]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("API key"));
    }

    #[test]
    fn build_file_image_content_encodes_correctly() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.png");
        let test_data = b"fake png data for testing";
        fs::write(&file, test_data).unwrap();

        let result = build_file_image_content(&file.to_string_lossy(), "test").unwrap();

        let url = result["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));

        // Verify the base64 decodes back to original data.
        use base64::Engine;
        let b64_part = url.strip_prefix("data:image/png;base64,").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_part)
            .unwrap();
        assert_eq!(decoded, test_data);
    }

    #[test]
    fn build_file_image_content_correct_mime_types() {
        let dir = tempdir().unwrap();

        let cases = [
            ("test.jpg", "image/jpeg"),
            ("test.jpeg", "image/jpeg"),
            ("test.png", "image/png"),
            ("test.gif", "image/gif"),
            ("test.webp", "image/webp"),
            ("test.svg", "image/svg+xml"),
        ];

        for (filename, expected_mime) in cases {
            let file = dir.path().join(filename);
            fs::write(&file, "data").unwrap();

            let result = build_file_image_content(&file.to_string_lossy(), "test").unwrap();
            let url = result["image_url"]["url"].as_str().unwrap();
            assert!(
                url.starts_with(&format!("data:{expected_mime};base64,")),
                "expected {expected_mime} for {filename}, got: {url}"
            );
        }
    }

    #[test]
    fn image_extensions_are_valid() {
        assert!(IMAGE_EXTENSIONS.contains(&"png"));
        assert!(IMAGE_EXTENSIONS.contains(&"jpg"));
        assert!(IMAGE_EXTENSIONS.contains(&"jpeg"));
        assert!(IMAGE_EXTENSIONS.contains(&"gif"));
        assert!(IMAGE_EXTENSIONS.contains(&"webp"));
        assert!(!IMAGE_EXTENSIONS.contains(&"pdf"));
        assert!(!IMAGE_EXTENSIONS.contains(&"txt"));
    }
}
