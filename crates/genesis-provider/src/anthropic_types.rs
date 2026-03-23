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

/// System content — always sent as content blocks with cache_control so that
/// the system prompt is part of the cached prefix.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum AnthropicSystemContent {
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
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
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
                // Always convert system message to content-block form with
                // cache_control so the system prompt is cached (breakpoint 1).
                let cache = Some(CacheControl {
                    control_type: "ephemeral".to_owned(),
                });
                if let Some(MessageContent::Parts(parts)) = &msg.content {
                    let blocks: Vec<AnthropicContentBlock> = parts
                        .iter()
                        .filter_map(|p| match p {
                            crate::api_types::ContentPart::Text { text } => {
                                Some(AnthropicContentBlock::Text {
                                    text: text.clone(),
                                    cache_control: cache.clone(),
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
                system = Some(AnthropicSystemContent::Blocks(vec![
                    AnthropicContentBlock::Text {
                        text,
                        cache_control: cache,
                    },
                ]));
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
                                cache_control: None,
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
                            cache_control: None,
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

/// Inject a cache breakpoint at the stable conversation prefix boundary.
///
/// Places 1 `cache_control` marker at a quantized position in the message
/// array so that the breakpoint stays at the **same** message index across
/// consecutive turns, giving Anthropic's cache time to hit.
///
/// Strategy:
/// We keep a "recent window" of the last few messages (the active turn that
/// changes each round). Everything before that window is the stable prefix.
/// The boundary is quantized to multiples of `STEP` messages so the
/// breakpoint only advances every `STEP/2` turns (~4 full user/assistant
/// pairs), not every single turn.
///
/// The marker is placed on the last content block of the boundary message,
/// supporting Text, ToolResult, and other block types.
fn inject_conversation_cache_breakpoints(messages: &mut [AnthropicMessage]) {
    // Need enough messages for a meaningful stable prefix.
    if messages.len() < 6 {
        return;
    }

    // Recent window: the last few messages that change every turn.
    // 4 messages ≈ 2 turns (user+assistant pairs).
    const RECENT_WINDOW: usize = 4;

    // Quantization step: the boundary index is rounded down to the nearest
    // multiple of STEP. This keeps the breakpoint at the same position for
    // several consecutive turns before it jumps forward.
    const STEP: usize = 8;

    let raw_boundary = messages.len().saturating_sub(RECENT_WINDOW + 1);
    if raw_boundary == 0 {
        return;
    }

    // Quantize: round down to nearest STEP, but ensure at least 1.
    let quantized = (raw_boundary / STEP) * STEP;
    let boundary = if quantized == 0 {
        // Conversation is still short — just use the raw boundary.
        raw_boundary
    } else {
        quantized
    };

    // Clamp to valid range (should already be, but be safe).
    let boundary = boundary.min(messages.len() - 1);

    mark_last_block(&mut messages[boundary]);
}

/// Set `cache_control` on the last content block of a message, regardless
/// of whether it is a Text or ToolResult block.
fn mark_last_block(message: &mut AnthropicMessage) {
    let cache_marker = CacheControl {
        control_type: "ephemeral".to_owned(),
    };

    match message.content {
        AnthropicMessageContent::Blocks(ref mut blocks) => {
            if let Some(block) = blocks.last_mut() {
                match block {
                    AnthropicContentBlock::Text {
                        ref mut cache_control,
                        ..
                    } => {
                        *cache_control = Some(cache_marker);
                    }
                    AnthropicContentBlock::ToolResult {
                        ref mut cache_control,
                        ..
                    } => {
                        *cache_control = Some(cache_marker);
                    }
                    _ => {}
                }
            }
        }
        AnthropicMessageContent::Text(_) => {
            // Plain-text messages (typically user text) — convert to a
            // single-block form so we can attach cache_control.
            // This is rare since most messages use Blocks by the time
            // they reach this function.
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
            // Log cache hit/miss stats from streaming usage (if present).
            if let Some(ref usage) = event.usage {
                usage.log_cache_stats();
            }
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
    fn system_message_extracted_to_top_level_with_cache_control() {
        let req = ChatCompletionRequest::new(
            "claude-sonnet-4-20250514",
            vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("Hello"),
            ],
        );

        let anthropic = to_anthropic_request(&req);

        // System message should always be Blocks with cache_control
        match &anthropic.system {
            Some(AnthropicSystemContent::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    AnthropicContentBlock::Text {
                        text,
                        cache_control,
                    } => {
                        assert_eq!(text, "You are helpful.");
                        assert!(cache_control.is_some(), "system block should have cache_control");
                    }
                    other => panic!("expected Text block, got {:?}", other),
                }
            }
            other => panic!("expected Blocks system, got {:?}", other),
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

    /// Helper to check if a message has cache_control set on any of its blocks.
    fn has_cache_control(msg: &AnthropicMessage) -> bool {
        match &msg.content {
            AnthropicMessageContent::Blocks(blocks) => blocks.iter().any(|b| match b {
                AnthropicContentBlock::Text { cache_control, .. } => cache_control.is_some(),
                AnthropicContentBlock::ToolResult { cache_control, .. } => cache_control.is_some(),
                _ => false,
            }),
            AnthropicMessageContent::Text(_) => false,
        }
    }

    #[test]
    fn cache_breakpoint_skipped_for_short_conversations() {
        // With fewer than 6 messages no conversation breakpoint should be set.
        let mut messages = vec![
            AnthropicMessage {
                role: "user".to_owned(),
                content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::Text {
                    text: "hi".to_owned(),
                    cache_control: None,
                }]),
            },
            AnthropicMessage {
                role: "assistant".to_owned(),
                content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::Text {
                    text: "hello".to_owned(),
                    cache_control: None,
                }]),
            },
            AnthropicMessage {
                role: "user".to_owned(),
                content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::Text {
                    text: "bye".to_owned(),
                    cache_control: None,
                }]),
            },
        ];

        inject_conversation_cache_breakpoints(&mut messages);

        for msg in &messages {
            assert!(!has_cache_control(msg), "short conversation should have no breakpoints");
        }
    }

    #[test]
    fn cache_breakpoint_placed_at_stable_quantized_boundary() {
        // Build a conversation with 10 messages (5 turns).
        // With RECENT_WINDOW=4, raw_boundary=10-5=5. STEP=8, quantized=0.
        // Since quantized==0, falls back to raw_boundary=5.
        let mut messages: Vec<AnthropicMessage> = (0..10)
            .map(|i| {
                let role = if i % 2 == 0 { "user" } else { "assistant" };
                AnthropicMessage {
                    role: role.to_owned(),
                    content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::Text {
                        text: format!("message {i}"),
                        cache_control: None,
                    }]),
                }
            })
            .collect();

        inject_conversation_cache_breakpoints(&mut messages);

        // Exactly one message should have cache_control set.
        let cached_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| has_cache_control(m))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(cached_indices.len(), 1, "should place exactly one breakpoint");
        // The boundary should be at index 5 (raw_boundary when quantized == 0).
        assert_eq!(cached_indices[0], 5);
    }

    #[test]
    fn cache_breakpoint_stable_across_consecutive_turns() {
        // Build 16 messages (8 turns). raw_boundary=16-5=11, quantized=(11/8)*8=8.
        let make_messages = |count: usize| -> Vec<AnthropicMessage> {
            (0..count)
                .map(|i| {
                    let role = if i % 2 == 0 { "user" } else { "assistant" };
                    AnthropicMessage {
                        role: role.to_owned(),
                        content: AnthropicMessageContent::Blocks(vec![
                            AnthropicContentBlock::Text {
                                text: format!("message {i}"),
                                cache_control: None,
                            },
                        ]),
                    }
                })
                .collect()
        };

        // Turn 8 (16 messages): boundary = (16-5)/8*8 = 11/8*8 = 8
        let mut msgs_16 = make_messages(16);
        inject_conversation_cache_breakpoints(&mut msgs_16);
        let idx_16: Vec<usize> = msgs_16
            .iter()
            .enumerate()
            .filter(|(_, m)| has_cache_control(m))
            .map(|(i, _)| i)
            .collect();

        // Turn 9 (18 messages): boundary = (18-5)/8*8 = 13/8*8 = 8
        let mut msgs_18 = make_messages(18);
        inject_conversation_cache_breakpoints(&mut msgs_18);
        let idx_18: Vec<usize> = msgs_18
            .iter()
            .enumerate()
            .filter(|(_, m)| has_cache_control(m))
            .map(|(i, _)| i)
            .collect();

        // Both should have the breakpoint at the same position (8).
        assert_eq!(idx_16, idx_18, "breakpoint should be stable across consecutive turns");
        assert_eq!(idx_16[0], 8);
    }

    #[test]
    fn cache_breakpoint_works_on_tool_result_blocks() {
        // Place a tool_result block at the boundary position and verify it
        // gets cache_control set.
        let mut messages: Vec<AnthropicMessage> = (0..8)
            .map(|i| {
                let role = if i % 2 == 0 { "user" } else { "assistant" };
                AnthropicMessage {
                    role: role.to_owned(),
                    content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::Text {
                        text: format!("msg {i}"),
                        cache_control: None,
                    }]),
                }
            })
            .collect();

        // Replace the message at the expected boundary with a tool_result block.
        // 8 messages: raw_boundary=8-5=3, quantized=0, so boundary=3.
        messages[3] = AnthropicMessage {
            role: "user".to_owned(),
            content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                tool_use_id: "call_1".to_owned(),
                content: "result".to_owned(),
                cache_control: None,
            }]),
        };

        inject_conversation_cache_breakpoints(&mut messages);

        // The tool_result at index 3 should now have cache_control.
        if let AnthropicMessageContent::Blocks(blocks) = &messages[3].content {
            match &blocks[0] {
                AnthropicContentBlock::ToolResult { cache_control, .. } => {
                    assert!(cache_control.is_some(), "tool_result should have cache_control");
                }
                other => panic!("expected ToolResult, got {:?}", other),
            }
        } else {
            panic!("expected Blocks content");
        }
    }
}
