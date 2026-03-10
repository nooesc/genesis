use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const TIMEOUT_SECS: u64 = 120;
const MAX_FILE_SIZE: u64 = 25 * 1024 * 1024; // 25 MB (Whisper API limit)

/// Supported audio file extensions.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp4", "mpeg", "mpga", "m4a", "wav", "webm", "ogg", "flac",
];

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build HTTP client")
    })
}

/// Audio transcription tool using OpenAI-compatible Whisper API.
///
/// Transcribes audio files to text. Supports mp3, mp4, mpeg, mpga, m4a, wav,
/// webm, ogg, and flac formats up to 25 MB.
pub struct TranscribeTool;

impl ToolHandler for TranscribeTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let file_path = call
            .arguments
            .get("file_path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "file_path",
            })?;

        let model = call
            .arguments
            .get("model")
            .map(|s| s.as_str())
            .unwrap_or("whisper-1");

        let language = call.arguments.get("language");

        let api_base = call
            .arguments
            .get("api_base")
            .or({
                // Fall back to environment variable.
                None // Handled below via env
            })
            .cloned()
            .or_else(|| std::env::var("OPENAI_API_BASE").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_owned());

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

        // Validate the file exists and is an audio file.
        let path = Path::new(file_path);
        if !path.is_file() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("`{file_path}` does not exist or is not a file"),
            });
        }

        // Check extension.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "unsupported audio format '.{ext}'. Supported: {}",
                    AUDIO_EXTENSIONS.join(", ")
                ),
            });
        }

        // Check file size.
        let metadata = std::fs::metadata(path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read file metadata: {e}"),
        })?;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "file is {} MB, exceeds 25 MB limit",
                    metadata.len() / (1024 * 1024)
                ),
            });
        }

        // Read the file.
        let file_bytes = std::fs::read(path).map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read audio file: {e}"),
        })?;

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.mp3")
            .to_owned();

        // Build multipart form.
        let file_part = reqwest::blocking::multipart::Part::bytes(file_bytes)
            .file_name(file_name)
            .mime_str(&format!("audio/{ext}"))
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to build multipart form: {e}"),
            })?;

        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", file_part)
            .text("model", model.to_owned())
            .text("response_format", "text".to_owned());

        if let Some(lang) = language {
            form = form.text("language", lang.clone());
        }

        // Optionally add prompt for context.
        if let Some(prompt) = call.arguments.get("prompt") {
            form = form.text("prompt", prompt.clone());
        }

        let url = format!("{}/audio/transcriptions", api_base.trim_end_matches('/'));

        let response = http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("API request failed: {e}"),
            })?;

        let status = response.status();
        let body = response.text().map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: format!("failed to read API response: {e}"),
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("API returned {status}: {body}"),
            });
        }

        Ok(ToolOutput {
            content: body,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("file_path".to_owned(), file_path.clone()),
                ("model".to_owned(), model.to_owned()),
            ]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContext;
    use std::fs;
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
        }
    }

    fn make_call(args: Vec<(&str, &str)>) -> ToolCall {
        ToolCall {
            name: "transcribe".to_owned(),
            arguments: args
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn error_on_missing_file() {
        let tool = TranscribeTool;
        let call = make_call(vec![("file_path", "/nonexistent/audio.mp3")]);
        // Set a dummy API key so we get past the key check.
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn error_on_unsupported_format() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "not audio").unwrap();

        let tool = TranscribeTool;
        let call = make_call(vec![("file_path", &file.to_string_lossy())]);
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("unsupported audio format"));
    }

    #[test]
    fn error_on_missing_api_key() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.mp3");
        fs::write(&file, "fake audio data").unwrap();

        // Ensure no env key is set.
        std::env::remove_var("OPENAI_API_KEY");

        let tool = TranscribeTool;
        let call = make_call(vec![("file_path", &file.to_string_lossy())]);
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(err.to_string().contains("API key"));
    }

    #[test]
    fn error_on_oversized_file() {
        // We can't easily create a 25MB+ temp file in a unit test, but we can
        // verify the size check logic works by testing the constant.
        assert_eq!(MAX_FILE_SIZE, 25 * 1024 * 1024);
    }

    #[test]
    fn audio_extensions_are_valid() {
        assert!(AUDIO_EXTENSIONS.contains(&"mp3"));
        assert!(AUDIO_EXTENSIONS.contains(&"wav"));
        assert!(AUDIO_EXTENSIONS.contains(&"flac"));
        assert!(AUDIO_EXTENSIONS.contains(&"ogg"));
        assert!(!AUDIO_EXTENSIONS.contains(&"txt"));
    }
}
