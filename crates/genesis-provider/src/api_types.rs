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
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
}

/// Optional thinking/reasoning configuration for compatible providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThinkingConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
}

/// Controls which (if any) tool the model should call.
///
/// Maps to the OpenAI `tool_choice` parameter:
/// - `"none"` → model will not call any tool
/// - `"auto"` → model decides (default when tools are present)
/// - `"required"` → model must call at least one tool
/// - `{"type":"function","function":{"name":"..."}}` → call a specific tool
#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoice {
    None,
    Auto,
    Required,
    /// Force the model to call a specific named tool.
    Function(String),
}

impl Serialize for ToolChoice {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ToolChoice::None => serializer.serialize_str("none"),
            ToolChoice::Auto => serializer.serialize_str("auto"),
            ToolChoice::Required => serializer.serialize_str("required"),
            ToolChoice::Function(name) => {
                use serde::ser::SerializeMap;
                #[derive(Serialize)]
                struct FnRef<'a> {
                    name: &'a str,
                }
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "function")?;
                map.serialize_entry("function", &FnRef { name })?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolChoice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(s) => match s.as_str() {
                "none" => Ok(ToolChoice::None),
                "auto" => Ok(ToolChoice::Auto),
                "required" => Ok(ToolChoice::Required),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["none", "auto", "required"],
                )),
            },
            serde_json::Value::Object(obj) => {
                let name = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| {
                        serde::de::Error::custom("tool_choice object must have function.name")
                    })?;
                Ok(ToolChoice::Function(name.to_owned()))
            }
            _ => Err(serde::de::Error::custom(
                "tool_choice must be a string or object",
            )),
        }
    }
}

/// Specifies the format that the model must output.
///
/// Supported by OpenAI, Anthropic (via proxy), and most local providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseFormat {
    /// Model output will be valid JSON. The model is instructed to only
    /// produce JSON. Note: you should also instruct the model in the
    /// system/user message to produce JSON.
    #[serde(rename = "json_object")]
    JsonObject,
    /// Model output follows a specific JSON Schema.
    #[serde(rename = "json_schema")]
    JsonSchema { json_schema: JsonSchemaSpec },
    /// Default text mode — no format constraint.
    #[serde(rename = "text")]
    Text,
}

/// Options for streaming responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Message content that can be plain text or multimodal (text + images).
///
/// Serializes as a JSON string for text-only, or as an array of content parts
/// for multimodal messages. Deserializes from both formats.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageContent {
    /// Plain text content (serialized as a JSON string).
    Text(String),
    /// Multimodal content parts (serialized as a JSON array).
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Extract the text content, joining text parts for multimodal messages.
    pub fn text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(s) => Some(s.as_str()),
            MessageContent::Parts(parts) => {
                // Return the first text part
                parts.iter().find_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            }
        }
    }

    /// Check if this content contains any image parts.
    pub fn has_images(&self) -> bool {
        match self {
            MessageContent::Text(_) => false,
            MessageContent::Parts(parts) => parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
        }
    }
}

impl<S: Into<String>> From<S> for MessageContent {
    fn from(s: S) -> Self {
        MessageContent::Text(s.into())
    }
}

impl Serialize for MessageContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            MessageContent::Text(s) => serializer.serialize_str(s),
            MessageContent::Parts(parts) => parts.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(MessageContent::Text(s)),
            serde_json::Value::Array(_) => {
                let parts: Vec<ContentPart> =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(MessageContent::Parts(parts))
            }
            serde_json::Value::Null => Ok(MessageContent::Text(String::new())),
            _ => Err(serde::de::Error::custom(
                "content must be a string or array of content parts",
            )),
        }
    }
}

/// JSON Schema specification for structured output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchemaSpec {
    /// Name of the schema (used for identification).
    pub name: String,
    /// The JSON Schema definition.
    pub schema: serde_json::Value,
    /// Whether the model must strictly follow the schema (default: true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// A single content part in a multimodal message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentPart {
    /// Text content.
    #[serde(rename = "text")]
    Text { text: String },
    /// Image via URL or base64 data URI.
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

/// Image URL reference for vision models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    /// The image URL. Can be a regular URL or `data:image/png;base64,...`.
    pub url: String,
    /// Detail level: "auto", "low", or "high".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Parse a data URI into (media_type, base64_data).
///
/// Handles URIs like `data:image/png;base64,iVBOR...` returning `("image/png", "iVBOR...")`.
pub(crate) fn parse_data_uri(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(',')?;
    let media_type = media_type.trim_end_matches(";base64");
    Some((media_type, data))
}

/// A single message in the chat conversation (OpenAI wire format).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Provider-specific metadata (e.g. codex reasoning blobs).
    /// Skipped during serialization to external APIs; populated from storage.
    #[serde(skip_serializing, default)]
    pub provider_metadata: Option<serde_json::Value>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_owned(),
            content: Some(MessageContent::Text(content.into())),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_owned(),
            content: Some(MessageContent::Text(content.into())),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        }
    }

    /// Create a user message with text and one or more images.
    pub fn user_with_images(text: impl Into<String>, image_urls: Vec<ImageUrl>) -> Self {
        let mut parts = vec![ContentPart::Text { text: text.into() }];
        for img in image_urls {
            parts.push(ContentPart::ImageUrl { image_url: img });
        }
        Self {
            role: "user".to_owned(),
            content: Some(MessageContent::Parts(parts)),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: Some(MessageContent::Text(content.into())),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        }
    }

    pub fn assistant_with_tool_calls(
        content: Option<MessageContent>,
        tool_calls: Vec<ToolCallEntry>,
    ) -> Self {
        Self {
            role: "assistant".to_owned(),
            content,
            thinking: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_owned(),
            content: Some(MessageContent::Text(content.into())),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
            provider_metadata: None,
        }
    }

    /// Extract the text content from this message, regardless of content type.
    /// For multimodal messages, returns the first text part.
    pub fn content_text(&self) -> Option<&str> {
        self.content.as_ref().and_then(|c| c.text())
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// OpenAI Chat Completions API response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

/// OpenAI-compatible streaming chat chunk.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub choices: Vec<ChatChunkChoice>,
    /// Token usage stats included in the final chunk when `stream_options.include_usage` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
    /// Provider-specific metadata extracted from the final streaming event
    /// (e.g. reasoning items from the Responses API `response.completed` event).
    /// Populated only for backends that expose such data in their completion events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ChatChunkChoice {
    pub index: u32,
    pub delta: ChatChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct ChatChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallEntry>>,
}

/// A single choice in the response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
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
            stream: None,
            stream_options: None,
            response_format: None,
            tool_choice: None,
            thinking: None,
            extra_body: None,
        }
    }

    /// Force the model to call a specific tool by name.
    pub fn with_tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    /// Set the response format to JSON object mode.
    pub fn with_json_mode(mut self) -> Self {
        self.response_format = Some(ResponseFormat::JsonObject);
        self
    }

    /// Set the response format to a specific JSON schema.
    pub fn with_json_schema(mut self, name: impl Into<String>, schema: serde_json::Value) -> Self {
        self.response_format = Some(ResponseFormat::JsonSchema {
            json_schema: JsonSchemaSpec {
                name: name.into(),
                schema,
                strict: Some(true),
            },
        });
        self
    }
}

impl From<&genesis_types::ToolDefinition> for ChatTool {
    fn from(def: &genesis_types::ToolDefinition) -> Self {
        // OpenAI strict mode requires:
        // 1. A non-null `parameters` object (even for zero-argument tools)
        // 2. `additionalProperties: false` on the top-level parameters object
        let parameters = match def.parameters.clone() {
            Some(mut schema) => {
                // Inject `additionalProperties: false` if not already present
                if let Some(obj) = schema.as_object_mut() {
                    obj.entry("additionalProperties")
                        .or_insert(serde_json::Value::Bool(false));
                }
                Some(schema)
            }
            None => Some(serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
                "required": []
            })),
        };

        Self {
            tool_type: "function".to_owned(),
            function: ChatToolFunction {
                name: def.name.clone(),
                description: def.description.clone(),
                parameters,
                strict: Some(true),
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
    fn content_text_returns_text_for_simple_message() {
        let msg = ChatMessage::user("hello");
        assert_eq!(msg.content_text(), Some("hello"));
    }

    #[test]
    fn content_text_returns_text_for_multimodal_message() {
        let msg = ChatMessage::user_with_images(
            "describe this",
            vec![ImageUrl {
                url: "https://example.com/img.png".to_owned(),
                detail: None,
            }],
        );
        assert_eq!(msg.content_text(), Some("describe this"));
    }

    #[test]
    fn text_content_serializes_as_string() {
        let msg = ChatMessage::user("hello");
        let json = serde_json::to_value(&msg).expect("should serialize");
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn multimodal_content_serializes_as_array() {
        let msg = ChatMessage::user_with_images(
            "what is this?",
            vec![ImageUrl {
                url: "data:image/png;base64,abc".to_owned(),
                detail: Some("high".to_owned()),
            }],
        );
        let json = serde_json::to_value(&msg).expect("should serialize");
        let content = json["content"].as_array().expect("should be array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is this?");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,abc");
        assert_eq!(content[1]["image_url"]["detail"], "high");
    }

    #[test]
    fn text_content_round_trips_through_json() {
        let msg = ChatMessage::user("hello world");
        let json = serde_json::to_string(&msg).expect("should serialize");
        let decoded: ChatMessage = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(decoded.content_text(), Some("hello world"));
    }

    #[test]
    fn multimodal_content_round_trips_through_json() {
        let msg = ChatMessage::user_with_images(
            "describe",
            vec![ImageUrl {
                url: "https://example.com/cat.jpg".to_owned(),
                detail: None,
            }],
        );
        let json = serde_json::to_string(&msg).expect("should serialize");
        let decoded: ChatMessage = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(decoded.content_text(), Some("describe"));
        match &decoded.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(&parts[1], ContentPart::ImageUrl { .. }));
            }
            other => panic!("expected Parts, got {:?}", other),
        }
    }

    #[test]
    fn has_images_detects_image_parts() {
        let text_msg = ChatMessage::user("hello");
        assert!(!text_msg.content.as_ref().unwrap().has_images());

        let img_msg = ChatMessage::user_with_images(
            "look",
            vec![ImageUrl {
                url: "data:image/png;base64,xyz".to_owned(),
                detail: None,
            }],
        );
        assert!(img_msg.content.as_ref().unwrap().has_images());
    }

    #[test]
    fn request_serializes_without_empty_tools() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hello")]);
        let json = serde_json::to_value(&request).expect("should serialize");
        assert!(!json.as_object().unwrap().contains_key("tools"));
    }

    #[test]
    fn request_serializes_thinking_when_present() {
        let mut request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hello")]);
        request.thinking = Some(ThinkingConfig {
            budget_tokens: Some(2048),
        });

        let json = serde_json::to_value(&request).expect("should serialize");
        assert_eq!(json["thinking"]["budget_tokens"], 2048);
    }

    #[test]
    fn request_omits_thinking_when_absent() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hello")]);
        let json = serde_json::to_value(&request).expect("should serialize");
        assert!(!json.as_object().unwrap().contains_key("thinking"));
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
        assert_eq!(response.choices[0].message.content_text(), Some("Hello!"));
        assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));
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
    fn streaming_chunk_deserializes_delta_content() {
        let raw = r#"{
            "id": "chatcmpl-chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": "Hel"
                },
                "finish_reason": null
            }]
        }"#;

        let chunk: ChatCompletionChunk =
            serde_json::from_str(raw).expect("streaming chunk should deserialize");
        assert_eq!(chunk.id, "chatcmpl-chunk");
        assert_eq!(chunk.choices[0].delta.role.as_deref(), Some("assistant"));
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hel"));
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
    fn tool_definition_none_params_gets_empty_strict_schema() {
        let def = genesis_types::ToolDefinition {
            name: "no_args_tool".to_owned(),
            description: "A tool with no parameters".to_owned(),
            parameters: None,
        };
        let chat_tool = ChatTool::from(&def);
        let json = serde_json::to_value(&chat_tool).expect("should serialize");

        // strict mode must be set
        assert_eq!(json["function"]["strict"], true);

        // parameters must not be null — OpenAI strict mode requires a valid schema
        let params = &json["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"], serde_json::json!({}));
        assert_eq!(params["additionalProperties"], false);
        assert_eq!(params["required"], serde_json::json!([]));
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
        // strict mode requires additionalProperties: false
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn tool_definition_preserves_existing_additional_properties() {
        // If a tool explicitly sets additionalProperties: true, we should not override it
        let def = genesis_types::ToolDefinition {
            name: "flexible_tool".to_owned(),
            description: "A tool that allows extra properties".to_owned(),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            })),
        };
        let chat_tool = ChatTool::from(&def);
        let json = serde_json::to_value(&chat_tool).expect("should serialize");

        // The existing value should be preserved (not overwritten)
        assert_eq!(json["function"]["parameters"]["additionalProperties"], true);
    }

    #[test]
    fn null_content_deserializes() {
        let raw = r#"{"role": "assistant", "content": null}"#;
        let msg: ChatMessage = serde_json::from_str(raw).expect("should deserialize null content");
        assert!(msg.content.is_none());
    }

    #[test]
    fn chat_message_reasoning_field_round_trips() {
        let msg = ChatMessage {
            role: "assistant".to_owned(),
            content: Some(MessageContent::Text("answer".to_owned())),
            thinking: Some("hidden reasoning".to_owned()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            provider_metadata: None,
        };

        let json = serde_json::to_string(&msg).expect("should serialize");
        let decoded: ChatMessage = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(decoded.thinking.as_deref(), Some("hidden reasoning"));
        assert_eq!(decoded.content_text(), Some("answer"));
    }

    #[test]
    fn message_content_from_string() {
        let content: MessageContent = "hello".into();
        assert_eq!(content.text(), Some("hello"));
    }

    #[test]
    fn json_mode_serializes_correctly() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("return JSON")])
            .with_json_mode();
        let json = serde_json::to_value(&request).expect("should serialize");
        assert_eq!(json["response_format"]["type"], "json_object");
    }

    #[test]
    fn json_schema_serializes_correctly() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "answer": { "type": "string" },
                "confidence": { "type": "number" }
            },
            "required": ["answer", "confidence"]
        });
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("answer")])
            .with_json_schema("answer_schema", schema.clone());
        let json = serde_json::to_value(&request).expect("should serialize");
        assert_eq!(json["response_format"]["type"], "json_schema");
        assert_eq!(
            json["response_format"]["json_schema"]["name"],
            "answer_schema"
        );
        assert_eq!(json["response_format"]["json_schema"]["strict"], true);
        assert_eq!(json["response_format"]["json_schema"]["schema"], schema);
    }

    #[test]
    fn response_format_omitted_when_none() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hi")]);
        let json = serde_json::to_value(&request).expect("should serialize");
        assert!(!json.as_object().unwrap().contains_key("response_format"));
    }

    #[test]
    fn response_format_round_trips() {
        let fmt = ResponseFormat::JsonSchema {
            json_schema: JsonSchemaSpec {
                name: "test".to_owned(),
                schema: serde_json::json!({"type": "object"}),
                strict: Some(true),
            },
        };
        let json = serde_json::to_string(&fmt).expect("serialize");
        let decoded: ResponseFormat = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ResponseFormat::JsonSchema { json_schema } => {
                assert_eq!(json_schema.name, "test");
                assert_eq!(json_schema.strict, Some(true));
            }
            _ => panic!("expected JsonSchema variant"),
        }
    }

    #[test]
    fn tool_choice_none_serializes_as_string() {
        let json = serde_json::to_value(&ToolChoice::None).expect("serialize");
        assert_eq!(json, "none");
    }

    #[test]
    fn tool_choice_auto_serializes_as_string() {
        let json = serde_json::to_value(&ToolChoice::Auto).expect("serialize");
        assert_eq!(json, "auto");
    }

    #[test]
    fn tool_choice_required_serializes_as_string() {
        let json = serde_json::to_value(&ToolChoice::Required).expect("serialize");
        assert_eq!(json, "required");
    }

    #[test]
    fn tool_choice_function_serializes_as_object() {
        let json =
            serde_json::to_value(ToolChoice::Function("search".to_owned())).expect("serialize");
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "search");
    }

    #[test]
    fn tool_choice_round_trips() {
        for choice in [
            ToolChoice::None,
            ToolChoice::Auto,
            ToolChoice::Required,
            ToolChoice::Function("shell_exec".to_owned()),
        ] {
            let json = serde_json::to_string(&choice).expect("serialize");
            let decoded: ToolChoice = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, choice);
        }
    }

    #[test]
    fn tool_choice_omitted_when_none_in_request() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hi")]);
        let json = serde_json::to_value(&request).expect("serialize");
        assert!(!json.as_object().unwrap().contains_key("tool_choice"));
    }

    #[test]
    fn tool_choice_included_in_request_when_set() {
        let request = ChatCompletionRequest::new("gpt-4", vec![ChatMessage::user("hi")])
            .with_tool_choice(ToolChoice::Required);
        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["tool_choice"], "required");
    }

    #[test]
    fn provider_metadata_not_serialized() {
        let mut msg = ChatMessage::user("hello");
        msg.provider_metadata = Some(serde_json::json!({"codex_reasoning_items": []}));
        let json = serde_json::to_value(&msg).expect("should serialize");
        assert!(!json.as_object().unwrap().contains_key("provider_metadata"));
    }

    #[test]
    fn provider_metadata_deserializes_when_present() {
        let raw =
            r#"{"role": "assistant", "content": "hi", "provider_metadata": {"key": "value"}}"#;
        let msg: ChatMessage = serde_json::from_str(raw).expect("should deserialize");
        assert!(msg.provider_metadata.is_some());
        assert_eq!(msg.provider_metadata.unwrap()["key"], "value");
    }

    #[test]
    fn provider_metadata_defaults_to_none() {
        let raw = r#"{"role": "assistant", "content": "hi"}"#;
        let msg: ChatMessage = serde_json::from_str(raw).expect("should deserialize");
        assert!(msg.provider_metadata.is_none());
    }

    #[test]
    fn chat_tool_serialization_includes_strict() {
        let def = genesis_types::ToolDefinition {
            name: "test_tool".to_owned(),
            description: "A test tool".to_owned(),
            parameters: Some(serde_json::json!({"type": "object", "properties": {}})),
        };
        let tool = ChatTool::from(&def);
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["function"]["strict"], true);
    }
}
