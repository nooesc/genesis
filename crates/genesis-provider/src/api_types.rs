use serde::{Deserialize, Serialize};

/// OpenAI Chat Completions API request body.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
}

/// A single message in the chat conversation (OpenAI wire format).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_owned(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_owned(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_with_tool_calls(
        content: Option<String>,
        tool_calls: Vec<ToolCallEntry>,
    ) -> Self {
        Self {
            role: "assistant".to_owned(),
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_owned(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }
}

/// A tool call entry inside an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// The function name and serialized arguments for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition in OpenAI format.
#[derive(Debug, Clone, Serialize)]
pub struct ChatTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ChatToolFunction,
}

/// The function schema inside a tool definition.
#[derive(Debug, Clone, Serialize)]
pub struct ChatToolFunction {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// OpenAI Chat Completions API response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

/// A single choice in the response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl ChatCompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            extra_body: None,
        }
    }
}

impl From<&genesis_types::ToolDefinition> for ChatTool {
    fn from(def: &genesis_types::ToolDefinition) -> Self {
        Self {
            tool_type: "function".to_owned(),
            function: ChatToolFunction {
                name: def.name.clone(),
                description: def.description.clone(),
                parameters: def.parameters.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_constructors_set_correct_roles() {
        assert_eq!(ChatMessage::system("hi").role, "system");
        assert_eq!(ChatMessage::user("hi").role, "user");
        assert_eq!(ChatMessage::assistant("hi").role, "assistant");
        assert_eq!(ChatMessage::tool_result("id", "result").role, "tool");
    }

    #[test]
    fn request_serializes_without_empty_tools() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hello")]);
        let json = serde_json::to_value(&request).expect("should serialize");
        assert!(!json.as_object().unwrap().contains_key("tools"));
    }

    #[test]
    fn tool_call_entry_round_trips_through_json() {
        let entry = ToolCallEntry {
            id: "call_abc123".to_owned(),
            call_type: "function".to_owned(),
            function: FunctionCall {
                name: "echo".to_owned(),
                arguments: r#"{"message":"hello"}"#.to_owned(),
            },
        };

        let json = serde_json::to_string(&entry).expect("should serialize");
        let decoded: ToolCallEntry = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn response_deserializes_from_openai_format() {
        let raw = r#"{
            "id": "chatcmpl-test",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        }"#;

        let response: ChatCompletionResponse =
            serde_json::from_str(raw).expect("should deserialize");
        assert_eq!(response.id, "chatcmpl-test");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("Hello!")
        );
        assert_eq!(
            response.choices[0].finish_reason.as_deref(),
            Some("stop")
        );
        assert_eq!(response.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn response_deserializes_tool_calls() {
        let raw = r#"{
            "id": "chatcmpl-tools",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "echo",
                            "arguments": "{\"message\":\"hi\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 10,
                "total_tokens": 30
            }
        }"#;

        let response: ChatCompletionResponse =
            serde_json::from_str(raw).expect("should deserialize");
        let tool_calls = response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("should have tool_calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "echo");
        assert_eq!(tool_calls[0].id, "call_abc");
    }

    #[test]
    fn tool_definition_converts_to_chat_tool() {
        let def = genesis_types::ToolDefinition {
            name: "search".to_owned(),
            description: "Search things".to_owned(),
            parameters: None,
        };
        let chat_tool = ChatTool::from(&def);
        assert_eq!(chat_tool.tool_type, "function");
        assert_eq!(chat_tool.function.name, "search");
    }

    #[test]
    fn tool_definition_with_parameters_serializes_to_wire_format() {
        let def = genesis_types::ToolDefinition {
            name: "shell_exec".to_owned(),
            description: "Runs a command".to_owned(),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run." }
                },
                "required": ["command"]
            })),
        };
        let chat_tool = ChatTool::from(&def);
        let json = serde_json::to_value(&chat_tool).expect("should serialize");

        let params = &json["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"]["command"]["type"], "string");
        assert_eq!(params["required"][0], "command");
    }
}
