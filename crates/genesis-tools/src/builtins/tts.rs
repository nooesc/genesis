use std::collections::BTreeMap;
use std::process::Command;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const DEFAULT_VOICE: &str = "en-US-AriaNeural";

pub struct TextToSpeechTool;

impl ToolHandler for TextToSpeechTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let text = call
            .arguments
            .get("text")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "text",
            })?;

        let output_path = call
            .arguments
            .get("output_path")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "output_path",
            })?;

        let voice = call
            .arguments
            .get("voice")
            .map(|s| s.as_str())
            .unwrap_or(DEFAULT_VOICE);
        let rate = call.arguments.get("rate").map(|s| s.as_str());

        if let Some(parent) = std::path::Path::new(output_path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("failed to create output directory: {e}"),
                })?;
            }
        }

        let output = Command::new("edge-tts")
            .args(build_edge_tts_args(text, voice, output_path, rate))
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to spawn edge-tts: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: if stderr.is_empty() {
                    format!("edge-tts exited with status {}", output.status)
                } else {
                    format!("edge-tts failed: {stderr}")
                },
            });
        }

        Ok(ToolOutput {
            content: format!("audio written to {output_path}"),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("voice".to_owned(), voice.to_owned()),
                ("output_path".to_owned(), output_path.clone()),
            ]),
        })
    }
}

fn build_edge_tts_args(
    text: &str,
    voice: &str,
    output_path: &str,
    rate: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--text".to_owned(),
        text.to_owned(),
        "--voice".to_owned(),
        voice.to_owned(),
        "--write-media".to_owned(),
        output_path.to_owned(),
    ];

    if let Some(rate) = rate {
        args.push("--rate".to_owned());
        args.push(rate.to_owned());
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: true,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    #[test]
    fn tts_requires_text_argument() {
        let tool = TextToSpeechTool;
        let call = ToolCall {
            name: "text_to_speech".to_owned(),
            arguments: BTreeMap::from([("output_path".to_owned(), "out.mp3".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "text",
                ..
            }
        ));
    }

    #[test]
    fn tts_requires_output_path_argument() {
        let tool = TextToSpeechTool;
        let call = ToolCall {
            name: "text_to_speech".to_owned(),
            arguments: BTreeMap::from([("text".to_owned(), "hello".to_owned())]),
        };

        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "output_path",
                ..
            }
        ));
    }

    #[test]
    fn build_edge_tts_args_uses_defaults() {
        let args = build_edge_tts_args("hello", DEFAULT_VOICE, "out.mp3", None);
        assert_eq!(
            args,
            vec![
                "--text",
                "hello",
                "--voice",
                DEFAULT_VOICE,
                "--write-media",
                "out.mp3",
            ]
        );
    }

    #[test]
    fn build_edge_tts_args_includes_rate_when_present() {
        let args = build_edge_tts_args("hello", "en-GB-SoniaNeural", "out.mp3", Some("+20%"));
        assert_eq!(
            args,
            vec![
                "--text",
                "hello",
                "--voice",
                "en-GB-SoniaNeural",
                "--write-media",
                "out.mp3",
                "--rate",
                "+20%",
            ]
        );
    }
}
