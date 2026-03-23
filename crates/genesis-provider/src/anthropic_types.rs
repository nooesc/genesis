//! Native Anthropic Messages API types and translation layer.
//!
//! The rest of Genesis speaks OpenAI-compatible types (`ChatCompletionRequest`,
//! `ChatCompletionResponse`, etc.). This module defines Anthropic's native wire
//! format and provides translation functions so `ChatClient` can transparently
//! support `backend: "anthropic"` without changing any callers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_types::{
    ChatChoice, ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, ChatMessage, ChatUsage, FunctionCall, MessageContent, ToolCallEntry,
};

// ---------------------------------------------------------------------------
// Anthropic request types
// ---------------------------------------------------------------------------

/// Anthropic Messages API request body.
#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystemContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinking>,
}

/// System content — can be a plain string or content blocks with cache_control.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum AnthropicSystemContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

/// Thinking/extended-thinking configuration for Anthropic.
#[derive(Debug, Serialize)]
pub(crate) struct AnthropicThinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    pub budget_tokens: u32,
}

/// A message in the Anthropic conversation.
#[derive(Debug, Serialize)]
pub(crate) struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicMessageContent,
}

/// Message content — string or array of content blocks.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum AnthropicMessageContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

/// A single content block in an Anthropic message.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub(crate) enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

/// Tool definition in Anthropic format.
#[derive(Debug, Serialize)]
pub(crate) struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Tool choice in Anthropic format.
#[derive(Debug, Serialize)]
pub(crate) struct AnthropicToolChoice {
    #[serde(rename = "type")]
    pub choice_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Anthropic response types
// ---------------------------------------------------------------------------

/// Anthropic Messages API response.
#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicResponse {
    pub id: String,
    pub content: Vec<AnthropicContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Tokens written to cache on this request (cache miss portion).
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    /// Tokens read from cache on this request (cache hit portion).
    #[serde(default)]
    pub cache_read_input_tokens: u32,
}

impl AnthropicUsage {
    pub fn to_chat_usage(&self) -> ChatUsage {
        ChatUsage {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: self.input_tokens + self.output_tokens,
        }
    }

    /// Log cache hit/miss statistics if any caching occurred.
    pub fn log_cache_stats(&self) {
        let cached = self.cache_read_input_tokens;
        let created = self.cache_creation_input_tokens;
        if cached > 0 || created > 0 {
            let total_input = self.input_tokens + cached + created;
            let hit_pct = if total_input > 0 {
                (cached as f64 / total_input as f64) * 100.0
            } else {
                0.0
            };
            tracing::debug!(
                cache_read_tokens = cached,
                cache_creation_tokens = created,
                input_tokens = self.input_tokens,
                cache_hit_pct = format!("{:.1}%", hit_pct),
                "anthropic prompt cache stats"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Anthropic streaming types
// ---------------------------------------------------------------------------

/// Top-level SSE event from Anthropic's streaming API.
#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    pub _event_type: String,
    #[serde(default)]
    pub message: Option<AnthropicStreamMessage>,
    #[serde(default)]
    pub content_block: Option<AnthropicContentBlock>,
    #[serde(default)]
    pub delta: Option<AnthropicStreamDelta>,
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamMessage {
    pub id: String,
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamDelta {
    #[serde(rename = "type")]
    pub delta_type: Option<String>,
    pub text: Option<String>,
    pub partial_json: Option<String>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub _usage: Option<AnthropicUsage>,
}

// ---------------------------------------------------------------------------
// Translation: OpenAI → Anthropic
// ---------------------------------------------------------------------------

/// Convert an OpenAI-format `ChatCompletionRequest` into an `AnthropicRequest`.
pub(crate) fn to_anthropic_request(req: &ChatCompletionRequest) -> AnthropicRequest {
    let mut system: Option<AnthropicSystemContent> = None;
    let mut messages: Vec<AnthropicMessage> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                // Check if content is already in content-block form (from cache injection)
                if let Some(MessageContent::Parts(parts)) = &msg.content {
                    let blocks: Vec<AnthropicContentBlock> = parts
                        .iter()
                        .filter_map(|p| match p {
                            crate::api_types::ContentPart::Text { text } => {
                                Some(AnthropicContentBlock::Text {
                                    text: text.clone(),
                                    cache_control: Some(CacheControl {
                                        control_type: "ephemeral".to_owned(),
                                    }),
                                })
                            }
                            _ => None,
                        })
                        .collect();
                    if !blocks.is_empty() {
                        system = Some(AnthropicSystemContent::Blocks(blocks));
                        continue;
                    }
                }
                let text = msg.content_text().unwrap_or_default().to_owned();
                system = Some(AnthropicSystemContent::Text(text));
            }
            "assistant" => {
                let mut blocks = Vec::new();

                // Add thinking block if present
                if let Some(thinking) = &msg.thinking {
                    blocks.push(AnthropicContentBlock::Thinking {
                        thinking: thinking.clone(),
                    });
                }

                // Add text content
                if let Some(text) = msg.content_text() {
                    if !text.is_empty() {
                        blocks.push(AnthropicContentBlock::Text {
                            text: text.to_owned(),
                            cache_control: None,
                        });
                    }
                }

                // Add tool calls as tool_use blocks
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let input: Value =
                            serde_json::from_str(&tc.function.arguments).unwrap_or_else(|e| {
                                tracing::warn!(arguments = %tc.function.arguments, error = %e, "invalid JSON in tool call arguments, using empty object");
                                Value::Object(serde_json::Map::new())
                            });
                        blocks.push(AnthropicContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            input,
                        });
                    }
                }

                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: String::new(),
                        cache_control: None,
                    });
                }

                messages.push(AnthropicMessage {
                    role: "assistant".to_owned(),
                    content: AnthropicMessageContent::Blocks(blocks),
                });
            }
            "tool" => {
                // OpenAI tool results become user messages with tool_result content blocks.
                // Anthropic requires tool results to be role: "user".
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                let content_text = msg.content_text().unwrap_or_default().to_owned();

                // If the last message is already a user message with tool_result blocks,
                // append to it (Anthropic requires all tool results for a turn in one message).
                let should_append = messages.last().is_some_and(|m| m.role == "user");
                if should_append {
                    if let Some(last) = messages.last_mut() {
                        if let AnthropicMessageContent::Blocks(ref mut blocks) = last.content {
                            blocks.push(AnthropicContentBlock::ToolResult {
                                tool_use_id,
                                content: content_text,
                            });
                            continue;
                        }
                    }
                }

                messages.push(AnthropicMessage {
                    role: "user".to_owned(),
                    content: AnthropicMessageContent::Blocks(vec![
                        AnthropicContentBlock::ToolResult {
                            tool_use_id,
                            content: content_text,
                        },
                    ]),
                });
            }
            "user" => {
                let content = match &msg.content {
                    Some(MessageContent::Text(text)) => AnthropicMessageContent::Text(text.clone()),
                    Some(MessageContent::Parts(parts)) => {
                        let blocks: Vec<AnthropicContentBlock> = parts
                            .iter()
                            .filter_map(|p| match p {
                                crate::api_types::ContentPart::Text { text } => {
                                    Some(AnthropicContentBlock::Text {
                                        text: text.clone(),
                                        cache_control: None,
                                    })
                                }
                                crate::api_types::ContentPart::ImageUrl { image_url } => {
                                    crate::api_types::parse_data_uri(&image_url.url).map(
                                        |(media_type, data)| AnthropicContentBlock::Image {
                                            source: ImageSource {
                                                source_type: "base64".to_owned(),
                                                media_type: media_type.to_owned(),
                                                data: data.to_owned(),
                                            },
                                        },
                                    )
                                }
                            })
                            .collect();
                        AnthropicMessageContent::Blocks(blocks)
                    }
                    None => AnthropicMessageContent::Text(String::new()),
                };
                messages.push(AnthropicMessage {
                    role: "user".to_owned(),
                    content,
                });
            }
            _ => {
                // Unknown role — pass through as-is
                let text = msg.content_text().unwrap_or_default().to_owned();
                messages.push(AnthropicMessage {
                    role: msg.role.clone(),
                    content: AnthropicMessageContent::Text(text),
                });
            }
        }
    }

    // Add cache breakpoints on the conversation prefix (breakpoints 3 & 4).
    // Breakpoints 1 & 2 are on system message and last tool definition.
    //
    // Strategy: place breakpoints at the boundary of the stable conversation
    // prefix. Messages before these points are likely cached from prior turns.
    // We target the second-to-last user message (the most recent "stable"
    // boundary) and the last tool_result block (often the largest content).
    inject_conversation_cache_breakpoints(&mut messages);

    // Convert tools
    let tools: Vec<AnthropicTool> = req
        .tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let is_last = i == req.tools.len() - 1;
            AnthropicTool {
                name: t.function.name.clone(),
                description: t.function.description.clone(),
                input_schema: t
                    .function
                    .parameters
                    .clone()
                    .unwrap_or(serde_json::json!({"type": "object", "properties": {}})),
                cache_control: if is_last {
                    Some(CacheControl {
                        control_type: "ephemeral".to_owned(),
                    })
                } else {
                    None
                },
            }
        })
        .collect();

    // Convert tool_choice
    let tool_choice = req.tool_choice.as_ref().map(|tc| match tc {
        crate::api_types::ToolChoice::None => AnthropicToolChoice {
            choice_type: "none".to_owned(),
            name: None,
        },
        crate::api_types::ToolChoice::Auto => AnthropicToolChoice {
            choice_type: "auto".to_owned(),
            name: None,
        },
        crate::api_types::ToolChoice::Required => AnthropicToolChoice {
            choice_type: "any".to_owned(),
            name: None,
        },
        crate::api_types::ToolChoice::Function(name) => AnthropicToolChoice {
            choice_type: "tool".to_owned(),
            name: Some(name.clone()),
        },
    });

    // Convert thinking config
    let thinking = req.thinking.as_ref().and_then(|t| {
        t.budget_tokens.map(|budget| AnthropicThinking {
            thinking_type: "enabled".to_owned(),
            budget_tokens: budget,
        })
    });

    AnthropicRequest {
        model: req.model.clone(),
        messages,
        max_tokens: req.max_tokens.unwrap_or(4096),
        system,
        temperature: req.temperature,
        stream: req.stream,
        tools,
        tool_choice,
        thinking,
    }
}

/// Inject cache breakpoints on the conversation message prefix.
///
/// Places up to 2 additional `cache_control` markers on messages that form
/// the stable prefix of the conversation. This maximizes cache hit rates
/// across turns since earlier messages don't change.
///
/// Strategy:
/// - Find the second-to-last user message (the "turn boundary" before
///   the current turn) and mark its last content block.
/// - Find the last tool_result-bearing message (often large, benefits
///   most from caching) and mark its last content block.
fn inject_conversation_cache_breakpoints(messages: &mut [AnthropicMessage]) {
    if messages.len() < 4 {
        // Too few messages to benefit from conversation caching.
        return;
    }

    let cache_marker = CacheControl {
        control_type: "ephemeral".to_owned(),
    };

    // Find the second-to-last user message index. The last user message is
    // the current turn (dynamic), but the one before it is stable.
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();

    // Mark the second-to-last user message's last content block.
    if user_indices.len() >= 2 {
        let target_idx = user_indices[user_indices.len() - 2];
        if let AnthropicMessageContent::Blocks(ref mut blocks) = messages[target_idx].content {
            if let Some(AnthropicContentBlock::Text {
                ref mut cache_control,
                ..
            }) = blocks.last_mut()
            {
                *cache_control = Some(cache_marker.clone());
            }
        }
    }

    // Mark the last assistant message before the final user message.
    // This captures the "stable response prefix" that doesn't change.
    let last_user_idx = user_indices.last().copied().unwrap_or(messages.len());
    if last_user_idx > 0 {
        for i in (0..last_user_idx).rev() {
            if messages[i].role == "assistant" {
                if let AnthropicMessageContent::Blocks(ref mut blocks) = messages[i].content {
                    if let Some(AnthropicContentBlock::Text {
                        ref mut cache_control,
                        ..
                    }) = blocks.last_mut()
                    {
                        *cache_control = Some(cache_marker);
                    }
                }
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Translation: Anthropic → OpenAI
// ---------------------------------------------------------------------------

/// Convert an Anthropic response into an OpenAI-format `ChatCompletionResponse`.
pub(crate) fn from_anthropic_response(resp: AnthropicResponse) -> ChatCompletionResponse {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCallEntry> = Vec::new();
    let mut thinking: Option<String> = None;

    for block in resp.content {
        match block {
            AnthropicContentBlock::Text {
                text,
                cache_control: _,
            } => {
                text_parts.push(text);
            }
            AnthropicContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCallEntry {
                    id,
                    call_type: "function".to_owned(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_default(),
                    },
                });
            }
            AnthropicContentBlock::Thinking {
                thinking: think_text,
            } => {
                thinking = Some(think_text);
            }
            _ => {}
        }
    }

    let content_text = if text_parts.is_empty() {
        None
    } else {
        Some(MessageContent::Text(text_parts.join("")))
    };

    let finish_reason = resp.stop_reason.as_deref().map(|r| match r {
        "end_turn" => "stop".to_owned(),
        "tool_use" => "tool_calls".to_owned(),
        "max_tokens" => "length".to_owned(),
        other => other.to_owned(),
    });

    let message = ChatMessage {
        role: "assistant".to_owned(),
        content: content_text,
        thinking,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        name: None,
        provider_metadata: None,
    };

    // Log cache hit/miss statistics for observability.
    resp.usage.log_cache_stats();

    ChatCompletionResponse {
        id: resp.id,
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason,
        }],
        usage: Some(resp.usage.to_chat_usage()),
    }
}

/// Convert an Anthropic SSE stream event into an OpenAI-format `ChatCompletionChunk`.
///
/// Returns `None` for events that don't map to a chunk (e.g., `message_stop`).
pub(crate) fn anthropic_event_to_chunk(
    event_type: &str,
    data: &str,
    msg_id: &mut String,
) -> Result<Option<ChatCompletionChunk>, crate::error::ProviderError> {
    let event: AnthropicStreamEvent = serde_json::from_str(data)?;

    match event_type {
        "message_start" => {
            if let Some(ref message) = event.message {
                *msg_id = message.id.clone();
            }
            // Emit an initial chunk with role
            Ok(Some(ChatCompletionChunk {
                id: msg_id.clone(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: Some("assistant".to_owned()),
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: event
                    .message
                    .as_ref()
                    .and_then(|m| m.usage.as_ref().map(|u| u.to_chat_usage())),
                provider_metadata: None,
            }))
        }
        "content_block_start" => {
            match &event.content_block {
                Some(AnthropicContentBlock::ToolUse { id, name, .. }) => {
                    // Start of a tool call — emit the tool call header
                    Ok(Some(ChatCompletionChunk {
                        id: msg_id.clone(),
                        choices: vec![ChatChunkChoice {
                            index: 0,
                            delta: ChatChunkDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(vec![ToolCallEntry {
                                    id: id.clone(),
                                    call_type: "function".to_owned(),
                                    function: FunctionCall {
                                        name: name.clone(),
                                        arguments: String::new(),
                                    },
                                }]),
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                        provider_metadata: None,
                    }))
                }
                _ => Ok(None),
            }
        }
        "content_block_delta" => {
            if let Some(ref delta) = event.delta {
                match delta.delta_type.as_deref() {
                    Some("text_delta") => Ok(Some(ChatCompletionChunk {
                        id: msg_id.clone(),
                        choices: vec![ChatChunkChoice {
                            index: 0,
                            delta: ChatChunkDelta {
                                role: None,
                                content: delta.text.clone(),
                                tool_calls: None,
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                        provider_metadata: None,
                    })),
                    Some("input_json_delta") => {
                        // Streaming tool call arguments
                        Ok(Some(ChatCompletionChunk {
                            id: msg_id.clone(),
                            choices: vec![ChatChunkChoice {
                                index: 0,
                                delta: ChatChunkDelta {
                                    role: None,
                                    content: None,
                                    tool_calls: Some(vec![ToolCallEntry {
                                        id: String::new(),
                                        call_type: "function".to_owned(),
                                        function: FunctionCall {
                                            name: String::new(),
                                            arguments: delta
                                                .partial_json
                                                .clone()
                                                .unwrap_or_default(),
                                        },
                                    }]),
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                            provider_metadata: None,
                        }))
                    }
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            }
        }
        "message_delta" => {
            let finish_reason = event.delta.as_ref().and_then(|d| {
                d.stop_reason.as_deref().map(|r| match r {
                    "end_turn" => "stop".to_owned(),
                    "tool_use" => "tool_calls".to_owned(),
                    "max_tokens" => "length".to_owned(),
                    other => other.to_owned(),
                })
            });
            let usage = event.usage.as_ref().map(|u| u.to_chat_usage());
            Ok(Some(ChatCompletionChunk {
                id: msg_id.clone(),
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                    },
                    finish_reason,
                }],
                usage,
                provider_metadata: None,
            }))
        }
        "message_stop" | "content_block_stop" | "ping" => Ok(None),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{ChatTool, ChatToolFunction, ThinkingConfig, ToolChoice};

    #[test]
    fn system_message_extracted_to_top_level() {
        let req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("Hello"),
            ],
        );

        let anthropic = to_anthropic_request(&req);

        match &anthropic.system {
            Some(AnthropicSystemContent::Text(text)) => assert_eq!(text, "You are helpful."),
            other => panic!("expected Text system, got {:?}", other),
        }
        assert_eq!(anthropic.messages.len(), 1);
        assert_eq!(anthropic.messages[0].role, "user");
    }

    #[test]
    fn tool_calls_translated_to_tool_use_blocks() {
        let req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![
                ChatMessage::user("search for cats"),
                ChatMessage::assistant_with_tool_calls(
                    None,
                    vec![ToolCallEntry {
                        id: "call_123".to_owned(),
                        call_type: "function".to_owned(),
                        function: FunctionCall {
                            name: "search".to_owned(),
                            arguments: r#"{"query":"cats"}"#.to_owned(),
                        },
                    }],
                ),
                ChatMessage::tool_result("call_123", "Found 5 cats"),
            ],
        );

        let anthropic = to_anthropic_request(&req);

        assert_eq!(anthropic.messages.len(), 3);
        // Assistant message should have tool_use block
        if let AnthropicMessageContent::Blocks(blocks) = &anthropic.messages[1].content {
            assert!(matches!(blocks[0], AnthropicContentBlock::ToolUse { .. }));
        } else {
            panic!("expected Blocks content for assistant");
        }
        // Tool result should be user role with tool_result block
        assert_eq!(anthropic.messages[2].role, "user");
        if let AnthropicMessageContent::Blocks(blocks) = &anthropic.messages[2].content {
            assert!(matches!(
                blocks[0],
                AnthropicContentBlock::ToolResult { .. }
            ));
        } else {
            panic!("expected Blocks content for tool result");
        }
    }

    #[test]
    fn tools_converted_to_anthropic_format() {
        let mut req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![ChatMessage::user("hello")],
        );
        req.tools = vec![
            ChatTool {
                tool_type: "function".to_owned(),
                function: ChatToolFunction {
                    name: "echo".to_owned(),
                    description: "Echoes text".to_owned(),
                    parameters: Some(serde_json::json!({
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"]
                    })),
                    strict: Some(true),
                },
            },
            ChatTool {
                tool_type: "function".to_owned(),
                function: ChatToolFunction {
                    name: "search".to_owned(),
                    description: "Searches".to_owned(),
                    parameters: None,
                    strict: Some(true),
                },
            },
        ];

        let anthropic = to_anthropic_request(&req);

        assert_eq!(anthropic.tools.len(), 2);
        assert_eq!(anthropic.tools[0].name, "echo");
        assert!(anthropic.tools[0].cache_control.is_none());
        assert_eq!(anthropic.tools[1].name, "search");
        assert!(anthropic.tools[1].cache_control.is_some()); // last tool gets cache
    }

    #[test]
    fn tool_choice_translated() {
        let mut req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![ChatMessage::user("hello")],
        );
        req.tool_choice = Some(ToolChoice::Required);
        let anthropic = to_anthropic_request(&req);
        assert_eq!(anthropic.tool_choice.as_ref().unwrap().choice_type, "any");

        req.tool_choice = Some(ToolChoice::Function("search".to_owned()));
        let anthropic = to_anthropic_request(&req);
        let tc = anthropic.tool_choice.as_ref().unwrap();
        assert_eq!(tc.choice_type, "tool");
        assert_eq!(tc.name.as_deref(), Some("search"));
    }

    #[test]
    fn thinking_config_translated() {
        let mut req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![ChatMessage::user("think hard")],
        );
        req.thinking = Some(ThinkingConfig {
            budget_tokens: Some(2048),
        });

        let anthropic = to_anthropic_request(&req);
        let t = anthropic.thinking.as_ref().unwrap();
        assert_eq!(t.thinking_type, "enabled");
        assert_eq!(t.budget_tokens, 2048);
    }

    #[test]
    fn default_max_tokens() {
        let req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![ChatMessage::user("hello")],
        );
        let anthropic = to_anthropic_request(&req);
        assert_eq!(anthropic.max_tokens, 4096);
    }

    #[test]
    fn response_translates_text() {
        let resp = AnthropicResponse {
            id: "msg_123".to_owned(),
            content: vec![AnthropicContentBlock::Text {
                text: "Hello!".to_owned(),
                cache_control: None,
            }],
            stop_reason: Some("end_turn".to_owned()),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };

        let openai = from_anthropic_response(resp);
        assert_eq!(openai.id, "msg_123");
        assert_eq!(openai.choices[0].message.content_text(), Some("Hello!"));
        assert_eq!(openai.choices[0].finish_reason.as_deref(), Some("stop"));
        let usage = openai.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn response_translates_tool_calls() {
        let resp = AnthropicResponse {
            id: "msg_456".to_owned(),
            content: vec![AnthropicContentBlock::ToolUse {
                id: "toolu_abc".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({"query": "cats"}),
            }],
            stop_reason: Some("tool_use".to_owned()),
            usage: AnthropicUsage {
                input_tokens: 20,
                output_tokens: 15,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };

        let openai = from_anthropic_response(resp);
        assert_eq!(
            openai.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        let tcs = openai.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_abc");
        assert_eq!(tcs[0].function.name, "search");
        assert!(tcs[0].function.arguments.contains("cats"));
    }

    #[test]
    fn response_translates_thinking() {
        let resp = AnthropicResponse {
            id: "msg_789".to_owned(),
            content: vec![
                AnthropicContentBlock::Thinking {
                    thinking: "Let me think...".to_owned(),
                },
                AnthropicContentBlock::Text {
                    text: "The answer is 42.".to_owned(),
                    cache_control: None,
                },
            ],
            stop_reason: Some("end_turn".to_owned()),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };

        let openai = from_anthropic_response(resp);
        assert_eq!(
            openai.choices[0].message.thinking.as_deref(),
            Some("Let me think...")
        );
        assert_eq!(
            openai.choices[0].message.content_text(),
            Some("The answer is 42.")
        );
    }

    #[test]
    fn stream_message_start_emits_role_chunk() {
        let data = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let mut msg_id = String::new();

        let chunk = anthropic_event_to_chunk("message_start", data, &mut msg_id)
            .unwrap()
            .unwrap();

        assert_eq!(msg_id, "msg_1");
        assert_eq!(chunk.choices[0].delta.role.as_deref(), Some("assistant"));
    }

    #[test]
    fn stream_text_delta_emits_content_chunk() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let mut msg_id = "msg_1".to_owned();

        let chunk = anthropic_event_to_chunk("content_block_delta", data, &mut msg_id)
            .unwrap()
            .unwrap();

        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn stream_message_stop_returns_none() {
        let data = r#"{"type":"message_stop"}"#;
        let mut msg_id = "msg_1".to_owned();

        let result = anthropic_event_to_chunk("message_stop", data, &mut msg_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn multiple_tool_results_coalesced() {
        let req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![
                ChatMessage::user("do stuff"),
                ChatMessage::assistant_with_tool_calls(
                    None,
                    vec![
                        ToolCallEntry {
                            id: "call_1".to_owned(),
                            call_type: "function".to_owned(),
                            function: FunctionCall {
                                name: "a".to_owned(),
                                arguments: "{}".to_owned(),
                            },
                        },
                        ToolCallEntry {
                            id: "call_2".to_owned(),
                            call_type: "function".to_owned(),
                            function: FunctionCall {
                                name: "b".to_owned(),
                                arguments: "{}".to_owned(),
                            },
                        },
                    ],
                ),
                ChatMessage::tool_result("call_1", "result A"),
                ChatMessage::tool_result("call_2", "result B"),
            ],
        );

        let anthropic = to_anthropic_request(&req);

        // The two tool results should be coalesced into one user message
        assert_eq!(anthropic.messages.len(), 3); // user, assistant, user(tool_results)
        if let AnthropicMessageContent::Blocks(blocks) = &anthropic.messages[2].content {
            assert_eq!(blocks.len(), 2);
            assert!(matches!(
                blocks[0],
                AnthropicContentBlock::ToolResult { .. }
            ));
            assert!(matches!(
                blocks[1],
                AnthropicContentBlock::ToolResult { .. }
            ));
        } else {
            panic!("expected Blocks content for coalesced tool results");
        }
    }
}
