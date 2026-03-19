//! OpenAI Responses API translation layer for the `openai-codex` backend.
//!
//! The rest of Genesis speaks OpenAI-compatible Chat Completions types
//! (`ChatCompletionRequest`, `ChatCompletionResponse`, etc.). This module
//! translates between those internal types and the Responses API wire format
//! used by the codex endpoint (`/responses`).

use serde_json::{json, Value};

use crate::api_types::{
    ChatChoice, ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, ChatCompletionRequest,
    ChatCompletionResponse, ChatMessage, ChatUsage, FunctionCall, MessageContent, ToolCallEntry,
};
use crate::error::ProviderError;

// ---------------------------------------------------------------------------
// Tool call ID encoding/decoding
// ---------------------------------------------------------------------------

/// Combine a Responses API `call_id` and response item `id` into a single
/// string for storage in `ToolCallEntry.id`.
pub(crate) fn combine_tool_id(call_id: &str, item_id: &str) -> String {
    format!("{}|{}", call_id, item_id)
}

/// Split a stored tool ID back into `(call_id, response_item_id)`.
///
/// Handles three cases:
/// - Contains `|`: split on it (round-trip from `combine_tool_id`)
/// - Starts with `fc_`: treat as item ID, derive call_id by replacing prefix
/// - Starts with `call_`: treat as call ID, derive item ID by replacing prefix
/// - Otherwise: duplicate the value for both
pub(crate) fn split_tool_id(id: &str) -> (String, String) {
    if let Some((call_id, item_id)) = id.split_once('|') {
        (call_id.to_owned(), item_id.to_owned())
    } else if let Some(suffix) = id.strip_prefix("fc_") {
        let call_id = format!("call_{suffix}");
        (call_id, id.to_owned())
    } else if let Some(suffix) = id.strip_prefix("call_") {
        let item_id = format!("fc_{suffix}");
        (id.to_owned(), item_id)
    } else {
        (id.to_owned(), id.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Translation: Chat Completions → Responses API
// ---------------------------------------------------------------------------

/// Convert an OpenAI-format `ChatCompletionRequest` into a Responses API
/// request body (`serde_json::Value`).
pub(crate) fn to_responses_request(req: &ChatCompletionRequest) -> Value {
    let mut instructions = String::from("You are a helpful assistant.");
    let mut input: Vec<Value> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(text) = msg.content_text() {
                    if !text.is_empty() {
                        instructions = text.to_owned();
                    }
                }
            }
            "user" => {
                let text = msg.content_text().unwrap_or_default();
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": text
                    }]
                }));
            }
            "assistant" => {
                // Replay reasoning items from provider_metadata before the
                // assistant message, enabling multi-turn reasoning continuity.
                if let Some(meta) = &msg.provider_metadata {
                    if let Some(items) = meta.get("codex_reasoning_items") {
                        if let Some(arr) = items.as_array() {
                            for item in arr {
                                input.push(item.clone());
                            }
                        }
                    }
                }

                // Emit the assistant text message (if any text content exists).
                let text = msg.content_text().unwrap_or_default();
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": text
                        }]
                    }));
                }

                // Emit function_call items for each tool call.
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let (call_id, item_id) = split_tool_id(&tc.id);
                        let mut fc = json!({
                            "type": "function_call",
                            "call_id": call_id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        });
                        // Attach the item ID if we have one that differs from call_id.
                        if !item_id.is_empty() {
                            fc["id"] = json!(item_id);
                        }
                        input.push(fc);
                    }
                }
            }
            "tool" => {
                let call_id_raw = msg.tool_call_id.as_deref().unwrap_or_default();
                let (call_id, _) = split_tool_id(call_id_raw);
                let output = msg.content_text().unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
            _ => {
                // Unknown role — pass through as user message to avoid losing data.
                let text = msg.content_text().unwrap_or_default();
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": text
                    }]
                }));
            }
        }
    }

    // Build tools array in Responses API flat format.
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            let mut tool = json!({
                "type": "function",
                "name": t.function.name,
                "description": t.function.description,
                "strict": false,
            });
            if let Some(params) = &t.function.parameters {
                tool["parameters"] = params.clone();
            }
            tool
        })
        .collect();

    let mut body = json!({
        "model": req.model,
        "instructions": instructions,
        "input": input,
        "store": false,
        "reasoning": {
            "effort": "medium",
            "summary": "auto",
        },
        "include": ["reasoning.encrypted_content"],
    });

    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }

    if let Some(max_tokens) = req.max_tokens {
        body["max_output_tokens"] = json!(max_tokens);
    }

    if let Some(temperature) = req.temperature {
        body["temperature"] = json!(temperature);
    }

    body
}

// ---------------------------------------------------------------------------
// Translation: Responses API → Chat Completions
// ---------------------------------------------------------------------------

/// Parse a Responses API response body into an OpenAI-format
/// `ChatCompletionResponse`.
pub(crate) fn from_responses_response(
    body: &Value,
) -> Result<ChatCompletionResponse, ProviderError> {
    let id = body["id"].as_str().unwrap_or_default().to_owned();

    let output = body["output"].as_array().cloned().unwrap_or_default();

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCallEntry> = Vec::new();
    let mut reasoning_items: Vec<Value> = Vec::new();

    for item in &output {
        let item_type = item["type"].as_str().unwrap_or_default();

        match item_type {
            "message" => {
                // Extract text from content parts.
                if let Some(content) = item["content"].as_array() {
                    for part in content {
                        let part_type = part["type"].as_str().unwrap_or_default();
                        if part_type == "output_text" || part_type == "text" {
                            if let Some(text) = part["text"].as_str() {
                                text_parts.push(text.to_owned());
                            }
                        }
                    }
                }
            }
            "reasoning" => {
                // Collect the entire reasoning item for persistence/replay.
                reasoning_items.push(item.clone());

                // Also extract summary text to include in the response.
                if let Some(summary) = item.get("summary") {
                    if let Some(arr) = summary.as_array() {
                        for entry in arr {
                            if entry["type"].as_str() == Some("summary_text") {
                                if let Some(text) = entry["text"].as_str() {
                                    // We don't add reasoning summaries to text_parts;
                                    // they go into provider_metadata for the consumer
                                    // to use as desired.
                                    let _ = text;
                                }
                            }
                        }
                    }
                }
            }
            "function_call" => {
                let call_id = item["call_id"].as_str().unwrap_or_default();
                let item_id = item["id"].as_str().unwrap_or_default();
                let name = item["name"].as_str().unwrap_or_default();
                let arguments = item["arguments"].as_str().unwrap_or_default();

                tool_calls.push(ToolCallEntry {
                    id: combine_tool_id(call_id, item_id),
                    call_type: "function".to_owned(),
                    function: FunctionCall {
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    },
                });
            }
            "custom_tool_call" => {
                // Same as function_call but uses `input` instead of `arguments`.
                let call_id = item["call_id"].as_str().unwrap_or_default();
                let item_id = item["id"].as_str().unwrap_or_default();
                let name = item["name"].as_str().unwrap_or_default();
                // `input` can be a string or an object; serialize to string if needed.
                let arguments = match &item["input"] {
                    Value::String(s) => s.clone(),
                    other if !other.is_null() => serde_json::to_string(other).unwrap_or_default(),
                    _ => String::new(),
                };

                tool_calls.push(ToolCallEntry {
                    id: combine_tool_id(call_id, item_id),
                    call_type: "function".to_owned(),
                    function: FunctionCall {
                        name: name.to_owned(),
                        arguments,
                    },
                });
            }
            _ => {
                // Ignore unknown item types.
            }
        }
    }

    // Determine finish_reason.
    let finish_reason = if !tool_calls.is_empty() {
        Some("tool_calls".to_owned())
    } else if body["status"].as_str() == Some("incomplete") {
        Some("incomplete".to_owned())
    } else {
        Some("stop".to_owned())
    };

    // Build content.
    let content = if text_parts.is_empty() {
        None
    } else {
        Some(MessageContent::Text(text_parts.join("")))
    };

    // Build provider_metadata with reasoning items if any.
    let provider_metadata = if reasoning_items.is_empty() {
        None
    } else {
        Some(json!({
            "codex_reasoning_items": reasoning_items
        }))
    };

    // Parse usage.
    let usage = if let Some(usage_obj) = body.get("usage") {
        let input_tokens = usage_obj["input_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = usage_obj["output_tokens"].as_u64().unwrap_or(0) as u32;
        Some(ChatUsage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
        })
    } else {
        None
    };

    let message = ChatMessage {
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
        provider_metadata,
    };

    Ok(ChatCompletionResponse {
        id,
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason,
        }],
        usage,
    })
}

// ---------------------------------------------------------------------------
// Streaming: Responses API SSE → ChatCompletionChunk
// ---------------------------------------------------------------------------

/// Parse a single Responses API SSE event and map it to a
/// `ChatCompletionChunk`, or `None` for events we ignore.
pub(crate) fn parse_responses_sse_event(
    event_type: &str,
    data: &Value,
) -> Result<Option<ChatCompletionChunk>, ProviderError> {
    let chunk_id = data["response"]["id"]
        .as_str()
        .unwrap_or("responses-stream")
        .to_owned();

    match event_type {
        "response.output_item.added" => {
            // When a function_call item is added, emit a chunk with the tool
            // call header (id + name, empty arguments).
            let item = &data["item"];
            let item_type = item["type"].as_str().unwrap_or_default();

            if item_type == "function_call" {
                let call_id = item["call_id"].as_str().unwrap_or_default();
                let item_id = item["id"].as_str().unwrap_or_default();
                let name = item["name"].as_str().unwrap_or_default();

                Ok(Some(ChatCompletionChunk {
                    id: chunk_id,
                    choices: vec![ChatChunkChoice {
                        index: 0,
                        delta: ChatChunkDelta {
                            role: None,
                            content: None,
                            tool_calls: Some(vec![ToolCallEntry {
                                id: combine_tool_id(call_id, item_id),
                                call_type: "function".to_owned(),
                                function: FunctionCall {
                                    name: name.to_owned(),
                                    arguments: String::new(),
                                },
                            }]),
                        },
                        finish_reason: None,
                    }],
                    usage: None,
                }))
            } else {
                Ok(None)
            }
        }

        "response.content_part.delta" | "response.output_text.delta" => {
            let delta_text = data["delta"].as_str().unwrap_or_default();
            Ok(Some(ChatCompletionChunk {
                id: chunk_id,
                choices: vec![ChatChunkChoice {
                    index: 0,
                    delta: ChatChunkDelta {
                        role: None,
                        content: Some(delta_text.to_owned()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            }))
        }

        "response.function_call_arguments.delta" => {
            let delta_args = data["delta"].as_str().unwrap_or_default();
            Ok(Some(ChatCompletionChunk {
                id: chunk_id,
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
                                arguments: delta_args.to_owned(),
                            },
                        }]),
                    },
                    finish_reason: None,
                }],
                usage: None,
            }))
        }

        "response.completed" => {
            // The final event carries the full response. Extract usage and
            // determine finish_reason from the completed response.
            let response = &data["response"];

            // Try to parse the full response for usage and finish_reason.
            let usage = response.get("usage").map(|u| {
                let input_tokens = u["input_tokens"].as_u64().unwrap_or(0) as u32;
                let output_tokens = u["output_tokens"].as_u64().unwrap_or(0) as u32;
                ChatUsage {
                    prompt_tokens: input_tokens,
                    completion_tokens: output_tokens,
                    total_tokens: input_tokens + output_tokens,
                }
            });

            // Determine finish_reason from the completed response.
            let has_tool_calls = response["output"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|i| i["type"].as_str() == Some("function_call"))
            });

            let finish_reason = if has_tool_calls {
                Some("tool_calls".to_owned())
            } else if response["status"].as_str() == Some("incomplete") {
                Some("incomplete".to_owned())
            } else {
                Some("stop".to_owned())
            };

            let resp_id = response["id"]
                .as_str()
                .unwrap_or("responses-stream")
                .to_owned();

            Ok(Some(ChatCompletionChunk {
                id: resp_id,
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

        // All other event types are ignored.
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_types::{
        ChatCompletionRequest, ChatMessage, ChatTool, ChatToolFunction, FunctionCall,
        MessageContent, ToolCallEntry,
    };

    #[test]
    fn test_to_responses_request_basic() {
        let req = ChatCompletionRequest::new(
            "o3-pro",
            vec![
                ChatMessage::system("You are Eve."),
                ChatMessage::user("Hello!"),
            ],
        );

        let body = to_responses_request(&req);

        assert_eq!(body["model"], "o3-pro");
        assert_eq!(body["instructions"], "You are Eve.");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");

        let input = body["input"].as_array().expect("input should be array");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "Hello!");
    }

    #[test]
    fn test_to_responses_request_default_instructions() {
        // No system message → default instructions.
        let req = ChatCompletionRequest::new("o3-pro", vec![ChatMessage::user("Hi")]);

        let body = to_responses_request(&req);
        assert_eq!(body["instructions"], "You are a helpful assistant.");
    }

    #[test]
    fn test_to_responses_request_with_tools() {
        let mut req = ChatCompletionRequest::new("o3-pro", vec![ChatMessage::user("search cats")]);
        req.tools = vec![
            ChatTool {
                tool_type: "function".to_owned(),
                function: ChatToolFunction {
                    name: "search".to_owned(),
                    description: "Search the web".to_owned(),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"]
                    })),
                    strict: Some(true),
                },
            },
            ChatTool {
                tool_type: "function".to_owned(),
                function: ChatToolFunction {
                    name: "echo".to_owned(),
                    description: "Echo text".to_owned(),
                    parameters: None,
                    strict: Some(true),
                },
            },
        ];

        let body = to_responses_request(&req);
        let tools = body["tools"].as_array().expect("tools should be array");
        assert_eq!(tools.len(), 2);

        // Flat format: type, name, description, parameters at top level.
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(tools[0]["description"], "Search the web");
        assert_eq!(tools[0]["strict"], false);
        assert_eq!(tools[0]["parameters"]["type"], "object");

        // Tool without parameters should not have parameters key.
        assert_eq!(tools[1]["type"], "function");
        assert_eq!(tools[1]["name"], "echo");
        assert!(tools[1].get("parameters").is_none());
    }

    #[test]
    fn test_to_responses_request_tool_result() {
        let req = ChatCompletionRequest::new(
            "o3-pro",
            vec![
                ChatMessage::user("search cats"),
                ChatMessage::assistant_with_tool_calls(
                    None,
                    vec![ToolCallEntry {
                        id: "call_abc|fc_abc".to_owned(),
                        call_type: "function".to_owned(),
                        function: FunctionCall {
                            name: "search".to_owned(),
                            arguments: r#"{"query":"cats"}"#.to_owned(),
                        },
                    }],
                ),
                ChatMessage::tool_result("call_abc|fc_abc", "Found 5 cats"),
            ],
        );

        let body = to_responses_request(&req);
        let input = body["input"].as_array().unwrap();

        // user, function_call, function_call_output = 3 items
        assert_eq!(input.len(), 3);

        // function_call item
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_abc");
        assert_eq!(input[1]["name"], "search");
        assert_eq!(input[1]["arguments"], r#"{"query":"cats"}"#);
        assert_eq!(input[1]["id"], "fc_abc");

        // function_call_output item
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_abc");
        assert_eq!(input[2]["output"], "Found 5 cats");
    }

    #[test]
    fn test_to_responses_request_reasoning_replay() {
        let reasoning_items = json!([
            {
                "type": "reasoning",
                "id": "rs_001",
                "encrypted_content": "base64blob==",
                "summary": [{"type": "summary_text", "text": "Thinking about it..."}]
            }
        ]);

        let mut assistant_msg = ChatMessage::assistant("The answer is 42.");
        assistant_msg.provider_metadata = Some(json!({
            "codex_reasoning_items": reasoning_items
        }));

        let req = ChatCompletionRequest::new(
            "o3-pro",
            vec![
                ChatMessage::user("What is the meaning of life?"),
                assistant_msg,
            ],
        );

        let body = to_responses_request(&req);
        let input = body["input"].as_array().unwrap();

        // user message, reasoning item (replayed), assistant text = 3 items
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");

        // The reasoning item should be replayed before the assistant message.
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["id"], "rs_001");
        assert_eq!(input[1]["encrypted_content"], "base64blob==");

        // Then the assistant message.
        assert_eq!(input[2]["type"], "message");
        assert_eq!(input[2]["role"], "assistant");
    }

    #[test]
    fn test_to_responses_request_max_tokens_and_temperature() {
        let mut req = ChatCompletionRequest::new("o3-pro", vec![ChatMessage::user("hi")]);
        req.max_tokens = Some(1024);
        req.temperature = Some(0.7);

        let body = to_responses_request(&req);
        assert_eq!(body["max_output_tokens"], 1024);
        assert_eq!(body["temperature"], 0.7);
    }

    #[test]
    fn test_from_responses_response_text() {
        let body = json!({
            "id": "resp_001",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Hello, world!"}
                    ]
                }
            ],
            "usage": {
                "input_tokens": 50,
                "output_tokens": 10
            }
        });

        let resp = from_responses_response(&body).unwrap();
        assert_eq!(resp.id, "resp_001");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(
            resp.choices[0].message.content_text(),
            Some("Hello, world!")
        );
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));

        let usage = resp.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.completion_tokens, 10);
        assert_eq!(usage.total_tokens, 60);
    }

    #[test]
    fn test_from_responses_response_tool_calls() {
        let body = json!({
            "id": "resp_002",
            "status": "completed",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_abc",
                    "call_id": "call_abc",
                    "name": "search",
                    "arguments": "{\"query\":\"cats\"}"
                },
                {
                    "type": "function_call",
                    "id": "fc_def",
                    "call_id": "call_def",
                    "name": "echo",
                    "arguments": "{\"text\":\"hello\"}"
                }
            ],
            "usage": {
                "input_tokens": 30,
                "output_tokens": 20
            }
        });

        let resp = from_responses_response(&body).unwrap();
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_calls"));

        let tcs = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 2);

        assert_eq!(tcs[0].id, "call_abc|fc_abc");
        assert_eq!(tcs[0].function.name, "search");
        assert!(tcs[0].function.arguments.contains("cats"));

        assert_eq!(tcs[1].id, "call_def|fc_def");
        assert_eq!(tcs[1].function.name, "echo");
    }

    #[test]
    fn test_from_responses_response_reasoning() {
        let body = json!({
            "id": "resp_003",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_001",
                    "encrypted_content": "encrypted_blob==",
                    "summary": [
                        {"type": "summary_text", "text": "Let me think about this..."}
                    ]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "The answer is 42."}
                    ]
                }
            ],
            "usage": {
                "input_tokens": 40,
                "output_tokens": 30
            }
        });

        let resp = from_responses_response(&body).unwrap();
        assert_eq!(
            resp.choices[0].message.content_text(),
            Some("The answer is 42.")
        );

        // Reasoning items should be in provider_metadata.
        let meta = resp.choices[0]
            .message
            .provider_metadata
            .as_ref()
            .expect("should have provider_metadata");
        let items = meta["codex_reasoning_items"]
            .as_array()
            .expect("should be array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["id"], "rs_001");
        assert_eq!(items[0]["encrypted_content"], "encrypted_blob==");
    }

    #[test]
    fn test_from_responses_response_incomplete_status() {
        let body = json!({
            "id": "resp_004",
            "status": "incomplete",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "I was cut off..."}
                    ]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let resp = from_responses_response(&body).unwrap();
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("incomplete"));
    }

    #[test]
    fn test_from_responses_response_custom_tool_call() {
        let body = json!({
            "id": "resp_005",
            "status": "completed",
            "output": [
                {
                    "type": "custom_tool_call",
                    "id": "fc_xyz",
                    "call_id": "call_xyz",
                    "name": "my_tool",
                    "input": {"key": "value"}
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let resp = from_responses_response(&body).unwrap();
        let tcs = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_xyz|fc_xyz");
        assert_eq!(tcs[0].function.name, "my_tool");
        assert!(tcs[0].function.arguments.contains("key"));
        assert!(tcs[0].function.arguments.contains("value"));
    }

    #[test]
    fn test_split_combine_tool_id() {
        // Round-trip with combined ID.
        let combined = combine_tool_id("call_abc", "fc_abc");
        assert_eq!(combined, "call_abc|fc_abc");

        let (call_id, item_id) = split_tool_id(&combined);
        assert_eq!(call_id, "call_abc");
        assert_eq!(item_id, "fc_abc");

        // fc_-prefixed ID without pipe.
        let (call_id, item_id) = split_tool_id("fc_123");
        assert_eq!(call_id, "call_123");
        assert_eq!(item_id, "fc_123");

        // call_-prefixed ID without pipe.
        let (call_id, item_id) = split_tool_id("call_456");
        assert_eq!(call_id, "call_456");
        assert_eq!(item_id, "fc_456");

        // Fallback — unknown format.
        let (call_id, item_id) = split_tool_id("something_else");
        assert_eq!(call_id, "something_else");
        assert_eq!(item_id, "something_else");
    }

    #[test]
    fn test_parse_sse_content_delta() {
        let data = json!({
            "delta": "Hello, ",
            "response": {"id": "resp_stream_001"}
        });

        let chunk = parse_responses_sse_event("response.output_text.delta", &data)
            .unwrap()
            .expect("should produce chunk");

        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello, "));
        assert!(chunk.choices[0].delta.tool_calls.is_none());
        assert_eq!(chunk.id, "resp_stream_001");
    }

    #[test]
    fn test_parse_sse_content_part_delta() {
        let data = json!({
            "delta": "world!",
            "response": {"id": "resp_stream_002"}
        });

        let chunk = parse_responses_sse_event("response.content_part.delta", &data)
            .unwrap()
            .expect("should produce chunk");

        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("world!"));
    }

    #[test]
    fn test_parse_sse_function_call_added() {
        let data = json!({
            "item": {
                "type": "function_call",
                "id": "fc_stream",
                "call_id": "call_stream",
                "name": "search"
            },
            "response": {"id": "resp_stream_003"}
        });

        let chunk = parse_responses_sse_event("response.output_item.added", &data)
            .unwrap()
            .expect("should produce chunk");

        let tcs = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_stream|fc_stream");
        assert_eq!(tcs[0].function.name, "search");
        assert_eq!(tcs[0].function.arguments, "");
    }

    #[test]
    fn test_parse_sse_function_call_delta() {
        let data = json!({
            "delta": "{\"query\":",
            "response": {"id": "resp_stream_004"}
        });

        let chunk = parse_responses_sse_event("response.function_call_arguments.delta", &data)
            .unwrap()
            .expect("should produce chunk");

        let tcs = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.arguments, "{\"query\":");
    }

    #[test]
    fn test_parse_sse_completed() {
        let data = json!({
            "response": {
                "id": "resp_done",
                "status": "completed",
                "output": [
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Done!"}]
                    }
                ],
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                }
            }
        });

        let chunk = parse_responses_sse_event("response.completed", &data)
            .unwrap()
            .expect("should produce chunk");

        assert_eq!(chunk.id, "resp_done");
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));

        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn test_parse_sse_completed_with_tool_calls() {
        let data = json!({
            "response": {
                "id": "resp_done_tc",
                "status": "completed",
                "output": [
                    {
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "search",
                        "arguments": "{}"
                    }
                ],
                "usage": {
                    "input_tokens": 20,
                    "output_tokens": 10
                }
            }
        });

        let chunk = parse_responses_sse_event("response.completed", &data)
            .unwrap()
            .expect("should produce chunk");

        assert_eq!(
            chunk.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
    }

    #[test]
    fn test_parse_sse_unknown_event_returns_none() {
        let data = json!({"something": "irrelevant"});
        let result = parse_responses_sse_event("response.some_unknown_event", &data).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_sse_output_item_added_non_function_returns_none() {
        let data = json!({
            "item": {
                "type": "message",
                "role": "assistant"
            }
        });

        let result = parse_responses_sse_event("response.output_item.added", &data).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_to_responses_request_assistant_text_and_tool_calls() {
        // Assistant message with both text content and tool calls.
        let req = ChatCompletionRequest::new(
            "o3-pro",
            vec![
                ChatMessage::user("search for cats and tell me about them"),
                ChatMessage {
                    role: "assistant".to_owned(),
                    content: Some(MessageContent::Text("Let me search for that.".to_owned())),
                    thinking: None,
                    tool_calls: Some(vec![ToolCallEntry {
                        id: "call_x|fc_x".to_owned(),
                        call_type: "function".to_owned(),
                        function: FunctionCall {
                            name: "search".to_owned(),
                            arguments: r#"{"q":"cats"}"#.to_owned(),
                        },
                    }]),
                    tool_call_id: None,
                    name: None,
                    provider_metadata: None,
                },
            ],
        );

        let body = to_responses_request(&req);
        let input = body["input"].as_array().unwrap();

        // user message + assistant text message + function_call = 3 items
        assert_eq!(input.len(), 3);
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["text"], "Let me search for that.");

        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["name"], "search");
    }
}
