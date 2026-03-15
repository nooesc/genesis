//! Native Google Gemini API types and translation layer.
//!
//! Translates between OpenAI-compatible types (`ChatCompletionRequest`,
//! `ChatCompletionResponse`) and Gemini's `generateContent` wire format
//! so `ChatClient` can transparently support `backend: "gemini"`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_types::{
    ChatChoice, ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, ChatMessage, ChatUsage, FunctionCall, MessageContent, ToolCallEntry,
};

// ---------------------------------------------------------------------------
// Gemini request types
// ---------------------------------------------------------------------------

/// Gemini generateContent request body.
#[derive(Debug, Serialize)]
pub(crate) struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<GeminiToolSet>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<GeminiToolConfig>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
}

/// A content block in Gemini format (used for messages and system instructions).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<GeminiPart>,
}

/// A single part within a Gemini content block.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub(crate) enum GeminiPart {
    Text {
        text: String,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: GeminiInlineData,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct GeminiInlineData {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct GeminiFunctionCall {
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct GeminiFunctionResponse {
    pub name: String,
    pub response: Value,
}

/// A set of function declarations.
#[derive(Debug, Serialize)]
pub(crate) struct GeminiToolSet {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

/// A function declaration in Gemini format.
#[derive(Debug, Serialize)]
pub(crate) struct GeminiFunctionDeclaration {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// Tool configuration (function calling mode).
#[derive(Debug, Serialize)]
pub(crate) struct GeminiToolConfig {
    #[serde(rename = "functionCallingConfig")]
    pub function_calling_config: GeminiFunctionCallingConfig,
}

#[derive(Debug, Serialize)]
pub(crate) struct GeminiFunctionCallingConfig {
    pub mode: String,
    #[serde(
        rename = "allowedFunctionNames",
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_function_names: Option<Vec<String>>,
}

/// Generation configuration parameters.
#[derive(Debug, Serialize)]
pub(crate) struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Gemini response types
// ---------------------------------------------------------------------------

/// Gemini generateContent response.
#[derive(Debug, Deserialize)]
pub(crate) struct GeminiResponse {
    #[serde(default)]
    pub candidates: Vec<GeminiCandidate>,
    #[serde(rename = "usageMetadata", default)]
    pub usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GeminiCandidate {
    pub content: Option<GeminiContent>,
    #[serde(rename = "finishReason")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GeminiUsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    pub prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount", default)]
    pub candidates_token_count: u32,
    #[serde(rename = "totalTokenCount", default)]
    pub total_token_count: u32,
}

impl GeminiUsageMetadata {
    pub fn to_chat_usage(&self) -> ChatUsage {
        ChatUsage {
            prompt_tokens: self.prompt_token_count,
            completion_tokens: self.candidates_token_count,
            total_tokens: self.total_token_count,
        }
    }
}

/// Map Gemini finish reason to OpenAI equivalent.
fn map_finish_reason(reason: &str) -> String {
    match reason {
        "STOP" => "stop".to_owned(),
        "MAX_TOKENS" => "length".to_owned(),
        "SAFETY" => "content_filter".to_owned(),
        other => other.to_ascii_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// Translation: OpenAI → Gemini
// ---------------------------------------------------------------------------

/// Convert an OpenAI-format `ChatCompletionRequest` into a `GeminiRequest`.
pub(crate) fn to_gemini_request(req: &ChatCompletionRequest) -> GeminiRequest {
    let mut system_instruction: Option<GeminiContent> = None;
    let mut contents: Vec<GeminiContent> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                let text = msg.content_text().unwrap_or_default().to_owned();
                system_instruction = Some(GeminiContent {
                    role: None,
                    parts: vec![GeminiPart::Text { text }],
                });
            }
            "user" => {
                let parts = content_to_gemini_parts(&msg.content);
                contents.push(GeminiContent {
                    role: Some("user".to_owned()),
                    parts,
                });
            }
            "assistant" => {
                let mut parts = Vec::new();

                // Add text content
                if let Some(text) = msg.content_text() {
                    if !text.is_empty() {
                        parts.push(GeminiPart::Text {
                            text: text.to_owned(),
                        });
                    }
                }

                // Convert tool calls to functionCall parts
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let args: Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|e| {
                                tracing::warn!(arguments = %tc.function.arguments, error = %e, "invalid JSON in tool call arguments, using empty object");
                                Value::Object(serde_json::Map::new())
                            });
                        parts.push(GeminiPart::FunctionCall {
                            function_call: GeminiFunctionCall {
                                name: tc.function.name.clone(),
                                args,
                            },
                        });
                    }
                }

                if parts.is_empty() {
                    parts.push(GeminiPart::Text {
                        text: String::new(),
                    });
                }

                contents.push(GeminiContent {
                    role: Some("model".to_owned()),
                    parts,
                });
            }
            "tool" => {
                // Tool results become functionResponse parts.
                // Gemini expects them in role: "user" messages.
                let name = msg.name.clone().unwrap_or_default();
                let content_text = msg.content_text().unwrap_or_default();

                let part = GeminiPart::FunctionResponse {
                    function_response: GeminiFunctionResponse {
                        name,
                        response: serde_json::json!({ "result": content_text }),
                    },
                };

                // Coalesce consecutive tool results into one user message
                let should_append = contents.last().is_some_and(|c| {
                    c.role.as_deref() == Some("user")
                        && c.parts
                            .iter()
                            .all(|p| matches!(p, GeminiPart::FunctionResponse { .. }))
                });

                if should_append {
                    if let Some(last) = contents.last_mut() {
                        last.parts.push(part);
                        continue;
                    }
                }

                contents.push(GeminiContent {
                    role: Some("user".to_owned()),
                    parts: vec![part],
                });
            }
            _ => {
                let text = msg.content_text().unwrap_or_default().to_owned();
                contents.push(GeminiContent {
                    role: Some("user".to_owned()),
                    parts: vec![GeminiPart::Text { text }],
                });
            }
        }
    }

    // Convert tools
    let tools: Vec<GeminiToolSet> = if req.tools.is_empty() {
        Vec::new()
    } else {
        let declarations: Vec<GeminiFunctionDeclaration> = req
            .tools
            .iter()
            .map(|t| GeminiFunctionDeclaration {
                name: t.function.name.clone(),
                description: t.function.description.clone(),
                parameters: t.function.parameters.clone(),
            })
            .collect();
        vec![GeminiToolSet {
            function_declarations: declarations,
        }]
    };

    // Convert tool_choice
    let tool_config = req.tool_choice.as_ref().map(|tc| {
        let (mode, allowed) = match tc {
            crate::api_types::ToolChoice::None => ("NONE".to_owned(), None),
            crate::api_types::ToolChoice::Auto => ("AUTO".to_owned(), None),
            crate::api_types::ToolChoice::Required => ("ANY".to_owned(), None),
            crate::api_types::ToolChoice::Function(name) => {
                ("ANY".to_owned(), Some(vec![name.clone()]))
            }
        };
        GeminiToolConfig {
            function_calling_config: GeminiFunctionCallingConfig {
                mode,
                allowed_function_names: allowed,
            },
        }
    });

    // Generation config
    let generation_config =
        if req.temperature.is_some() || req.max_tokens.is_some() {
            Some(GeminiGenerationConfig {
                temperature: req.temperature,
                max_output_tokens: req.max_tokens,
            })
        } else {
            None
        };

    GeminiRequest {
        contents,
        system_instruction,
        tools,
        tool_config,
        generation_config,
    }
}

/// Convert message content to Gemini parts.
fn content_to_gemini_parts(content: &Option<MessageContent>) -> Vec<GeminiPart> {
    match content {
        Some(MessageContent::Text(text)) => {
            vec![GeminiPart::Text {
                text: text.clone(),
            }]
        }
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                crate::api_types::ContentPart::Text { text } => Some(GeminiPart::Text {
                    text: text.clone(),
                }),
                crate::api_types::ContentPart::ImageUrl { image_url } => {
                    crate::api_types::parse_data_uri(&image_url.url).map(
                        |(media_type, data)| GeminiPart::InlineData {
                            inline_data: GeminiInlineData {
                                mime_type: media_type.to_owned(),
                                data: data.to_owned(),
                            },
                        },
                    )
                }
            })
            .collect(),
        None => vec![GeminiPart::Text {
            text: String::new(),
        }],
    }
}

// ---------------------------------------------------------------------------
// Translation: Gemini → OpenAI
// ---------------------------------------------------------------------------

/// Convert a Gemini response into an OpenAI-format `ChatCompletionResponse`.
pub(crate) fn from_gemini_response(
    resp: GeminiResponse,
    response_id: &str,
) -> ChatCompletionResponse {
    let candidate = resp.candidates.into_iter().next();

    let (message, finish_reason) = match candidate {
        Some(c) => {
            let finish_reason = c.finish_reason.as_deref().map(map_finish_reason);

            let message = if let Some(content) = c.content {
                parts_to_chat_message(&content.parts)
            } else {
                ChatMessage::assistant("")
            };

            // If there are tool calls and no explicit finish reason for them,
            // set it to "tool_calls"
            let finish_reason = if message.tool_calls.is_some()
                && finish_reason.as_deref() == Some("stop")
            {
                Some("tool_calls".to_owned())
            } else {
                finish_reason
            };

            (message, finish_reason)
        }
        None => (ChatMessage::assistant(""), None),
    };

    let usage = resp.usage_metadata.map(|u| u.to_chat_usage());

    ChatCompletionResponse {
        id: response_id.to_owned(),
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason,
        }],
        usage,
    }
}

/// Convert Gemini content parts into a ChatMessage.
fn parts_to_chat_message(parts: &[GeminiPart]) -> ChatMessage {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCallEntry> = Vec::new();

    for (i, part) in parts.iter().enumerate() {
        match part {
            GeminiPart::Text { text } => {
                text_parts.push(text.clone());
            }
            GeminiPart::FunctionCall { function_call } => {
                tool_calls.push(ToolCallEntry {
                    id: format!("call_{}", i),
                    call_type: "function".to_owned(),
                    function: FunctionCall {
                        name: function_call.name.clone(),
                        arguments: serde_json::to_string(&function_call.args)
                            .unwrap_or_default(),
                    },
                });
            }
            _ => {}
        }
    }

    let content = if text_parts.is_empty() {
        None
    } else {
        Some(MessageContent::Text(text_parts.join("")))
    };

    ChatMessage {
        role: "assistant".to_owned(),
        content,
        thinking: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        name: None,
        provider_metadata: None,
    }
}

/// Convert a Gemini streaming response chunk into an OpenAI chunk.
pub(crate) fn from_gemini_stream_chunk(
    resp: GeminiResponse,
    chunk_id: &str,
) -> Option<ChatCompletionChunk> {
    let candidate = resp.candidates.first()?;
    let content = candidate.content.as_ref()?;

    let mut text_content: Option<String> = None;
    let mut tool_calls: Option<Vec<ToolCallEntry>> = None;

    for (i, part) in content.parts.iter().enumerate() {
        match part {
            GeminiPart::Text { text } => {
                text_content = Some(text.clone());
            }
            GeminiPart::FunctionCall { function_call } => {
                let entry = ToolCallEntry {
                    id: format!("call_{}", i),
                    call_type: "function".to_owned(),
                    function: FunctionCall {
                        name: function_call.name.clone(),
                        arguments: serde_json::to_string(&function_call.args)
                            .unwrap_or_default(),
                    },
                };
                tool_calls.get_or_insert_with(Vec::new).push(entry);
            }
            _ => {}
        }
    }

    let finish_reason = candidate.finish_reason.as_deref().map(|r| {
        let mapped = map_finish_reason(r);
        // Override "stop" to "tool_calls" when function calls are present
        if mapped == "stop" && tool_calls.is_some() {
            "tool_calls".to_owned()
        } else {
            mapped
        }
    });

    let usage = resp.usage_metadata.map(|u| u.to_chat_usage());

    Some(ChatCompletionChunk {
        id: chunk_id.to_owned(),
        choices: vec![ChatChunkChoice {
            index: 0,
            delta: ChatChunkDelta {
                role: Some("assistant".to_owned()),
                content: text_content,
                tool_calls,
            },
            finish_reason,
        }],
        usage,
    })
}

/// Build the Gemini generateContent URL.
pub(crate) fn generate_content_url(base: &str, model: &str, api_key: &str) -> String {
    format!(
        "{}/models/{}:generateContent?key={}",
        base.trim_end_matches('/'),
        model,
        api_key
    )
}

/// Build the Gemini streamGenerateContent URL.
pub(crate) fn stream_generate_content_url(base: &str, model: &str, api_key: &str) -> String {
    format!(
        "{}/models/{}:streamGenerateContent?alt=sse&key={}",
        base.trim_end_matches('/'),
        model,
        api_key
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{
        ChatCompletionRequest, ChatMessage, ChatTool, ChatToolFunction, ToolChoice,
    };

    #[test]
    fn system_message_becomes_system_instruction() {
        let req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("Hello"),
            ],
        );

        let gemini = to_gemini_request(&req);

        let si = gemini.system_instruction.unwrap();
        assert!(matches!(&si.parts[0], GeminiPart::Text { text } if text == "You are helpful."));
        assert_eq!(gemini.contents.len(), 1);
        assert_eq!(gemini.contents[0].role.as_deref(), Some("user"));
    }

    #[test]
    fn assistant_maps_to_model_role() {
        let req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![
                ChatMessage::user("Hi"),
                ChatMessage::assistant("Hello!"),
                ChatMessage::user("How are you?"),
            ],
        );

        let gemini = to_gemini_request(&req);

        assert_eq!(gemini.contents.len(), 3);
        assert_eq!(gemini.contents[0].role.as_deref(), Some("user"));
        assert_eq!(gemini.contents[1].role.as_deref(), Some("model"));
        assert_eq!(gemini.contents[2].role.as_deref(), Some("user"));
    }

    #[test]
    fn tool_calls_become_function_call_parts() {
        let req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![
                ChatMessage::user("search for cats"),
                ChatMessage::assistant_with_tool_calls(
                    None,
                    vec![ToolCallEntry {
                        id: "call_1".to_owned(),
                        call_type: "function".to_owned(),
                        function: FunctionCall {
                            name: "search".to_owned(),
                            arguments: r#"{"query":"cats"}"#.to_owned(),
                        },
                    }],
                ),
                ChatMessage::tool_result("call_1", "Found 5 cats"),
            ],
        );

        let gemini = to_gemini_request(&req);

        assert_eq!(gemini.contents.len(), 3);
        // Assistant with function call
        assert!(matches!(
            &gemini.contents[1].parts[0],
            GeminiPart::FunctionCall { function_call } if function_call.name == "search"
        ));
        // Tool result as function response
        assert!(matches!(
            &gemini.contents[2].parts[0],
            GeminiPart::FunctionResponse { .. }
        ));
    }

    #[test]
    fn tools_converted_to_function_declarations() {
        let mut req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![ChatMessage::user("hello")],
        );
        req.tools = vec![ChatTool {
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
        }];

        let gemini = to_gemini_request(&req);

        assert_eq!(gemini.tools.len(), 1);
        assert_eq!(gemini.tools[0].function_declarations.len(), 1);
        assert_eq!(gemini.tools[0].function_declarations[0].name, "echo");
    }

    #[test]
    fn tool_choice_translated() {
        let mut req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![ChatMessage::user("hello")],
        );

        req.tool_choice = Some(ToolChoice::Required);
        let gemini = to_gemini_request(&req);
        assert_eq!(
            gemini
                .tool_config
                .as_ref()
                .unwrap()
                .function_calling_config
                .mode,
            "ANY"
        );

        req.tool_choice = Some(ToolChoice::Function("search".to_owned()));
        let gemini = to_gemini_request(&req);
        let tc = gemini.tool_config.as_ref().unwrap();
        assert_eq!(tc.function_calling_config.mode, "ANY");
        assert_eq!(
            tc.function_calling_config.allowed_function_names,
            Some(vec!["search".to_owned()])
        );
    }

    #[test]
    fn generation_config_set() {
        let mut req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![ChatMessage::user("hello")],
        );
        req.temperature = Some(0.7);
        req.max_tokens = Some(1024);

        let gemini = to_gemini_request(&req);
        let gc = gemini.generation_config.unwrap();
        assert_eq!(gc.temperature, Some(0.7));
        assert_eq!(gc.max_output_tokens, Some(1024));
    }

    #[test]
    fn response_translates_text() {
        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: Some("model".to_owned()),
                    parts: vec![GeminiPart::Text {
                        text: "Hello!".to_owned(),
                    }],
                }),
                finish_reason: Some("STOP".to_owned()),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
            }),
        };

        let openai = from_gemini_response(resp, "resp-123");
        assert_eq!(openai.id, "resp-123");
        assert_eq!(openai.choices[0].message.content_text(), Some("Hello!"));
        assert_eq!(
            openai.choices[0].finish_reason.as_deref(),
            Some("stop")
        );
        let usage = openai.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn response_translates_function_calls() {
        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: Some("model".to_owned()),
                    parts: vec![GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: "search".to_owned(),
                            args: serde_json::json!({"query": "cats"}),
                        },
                    }],
                }),
                finish_reason: Some("STOP".to_owned()),
            }],
            usage_metadata: None,
        };

        let openai = from_gemini_response(resp, "resp-456");
        assert_eq!(
            openai.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        let tcs = openai.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "search");
        assert!(tcs[0].function.arguments.contains("cats"));
    }

    #[test]
    fn url_builders() {
        let base = "https://generativelanguage.googleapis.com/v1beta";
        let url = generate_content_url(base, "gemini-2.5-pro", "AIza-test");
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent?key=AIza-test"
        );

        let url = stream_generate_content_url(base, "gemini-2.5-flash", "AIza-test");
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse&key=AIza-test"
        );
    }

    #[test]
    fn empty_candidates_handled() {
        let resp = GeminiResponse {
            candidates: vec![],
            usage_metadata: None,
        };

        let openai = from_gemini_response(resp, "resp-empty");
        assert_eq!(openai.choices[0].message.content_text(), Some(""));
    }

    #[test]
    fn tool_result_name_used_for_function_response() {
        // Tool results in OpenAI format use tool_call_id and optionally name.
        // For Gemini, we need the function name in functionResponse.
        let mut msg = ChatMessage::tool_result("call_1", "result text");
        msg.name = Some("search".to_owned());

        let req = ChatCompletionRequest::new(
            "gemini-2.5-pro",
            vec![
                ChatMessage::user("search"),
                ChatMessage::assistant_with_tool_calls(
                    None,
                    vec![ToolCallEntry {
                        id: "call_1".to_owned(),
                        call_type: "function".to_owned(),
                        function: FunctionCall {
                            name: "search".to_owned(),
                            arguments: "{}".to_owned(),
                        },
                    }],
                ),
                msg,
            ],
        );

        let gemini = to_gemini_request(&req);

        if let GeminiPart::FunctionResponse { function_response } =
            &gemini.contents[2].parts[0]
        {
            assert_eq!(function_response.name, "search");
        } else {
            panic!("expected FunctionResponse part");
        }
    }

    #[test]
    fn stream_chunk_translates() {
        let resp = GeminiResponse {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: Some("model".to_owned()),
                    parts: vec![GeminiPart::Text {
                        text: "Hello".to_owned(),
                    }],
                }),
                finish_reason: None,
            }],
            usage_metadata: None,
        };

        let chunk = from_gemini_stream_chunk(resp, "chunk-1").unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
        assert!(chunk.choices[0].finish_reason.is_none());
    }
}
