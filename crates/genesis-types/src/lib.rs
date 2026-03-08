use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationMessage {
    pub role: ConversationRole,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPlatform {
    Cli,
    Telegram,
    Discord,
    Slack,
    HomeAssistant,
    WhatsApp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    OpenAi,
    OpenRouter,
    Anthropic,
    Gemini,
    Local,
    Compatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider: ModelProviderKind,
    pub model: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderTurnRequest {
    pub session_id: String,
    pub model: ModelSelection,
    pub messages: Vec<ConversationMessage>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolCall,
    MaxTokens,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageStats {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderTurnResponse {
    pub message: ConversationMessage,
    pub stop_reason: StopReason,
    pub usage: UsageStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RuntimeEvent {
    SessionStarted { session_id: String },
    ModelSelected { provider: String, model: String },
    SessionPlanned {
        session_id: String,
        platform: DeliveryPlatform,
        provider: String,
        model: String,
    },
    TokenStream { chunk: String },
    ToolCallRequested { tool_name: String },
    ToolCallCompleted { tool_name: String, success: bool },
    SchedulerTick { job_id: String },
    DeliveryQueued { platform: DeliveryPlatform, destination: String },
}

#[cfg(test)]
mod tests {
    use super::{
        ConversationMessage, ConversationRole, DeliveryPlatform, ModelProviderKind, ModelSelection,
        ProviderTurnRequest, ProviderTurnResponse, RuntimeEvent, StopReason, ToolDefinition,
        UsageStats,
    };

    #[test]
    fn runtime_events_round_trip_through_json() {
        let event = RuntimeEvent::SessionPlanned {
            session_id: "session-123".to_owned(),
            platform: DeliveryPlatform::Slack,
            provider: "openrouter".to_owned(),
            model: "moonshotai/kimi-k2".to_owned(),
        };

        let encoded = serde_json::to_string(&event).expect("event should serialize");
        let decoded: RuntimeEvent =
            serde_json::from_str(&encoded).expect("event should deserialize");

        assert_eq!(decoded, event);
    }

    #[test]
    fn provider_turn_payloads_round_trip_through_json() {
        let request = ProviderTurnRequest {
            session_id: "session-123".to_owned(),
            model: ModelSelection {
                provider: ModelProviderKind::OpenRouter,
                model: "moonshotai/kimi-k2".to_owned(),
                base_url: Some("https://openrouter.ai/api/v1".to_owned()),
            },
            messages: vec![ConversationMessage {
                role: ConversationRole::User,
                content: "Summarize the last run.".to_owned(),
            }],
            tools: vec![ToolDefinition {
                name: "session_search".to_owned(),
                description: "Searches prior sessions".to_owned(),
            }],
        };

        let response = ProviderTurnResponse {
            message: ConversationMessage {
                role: ConversationRole::Assistant,
                content: "Here is the summary.".to_owned(),
            },
            stop_reason: StopReason::EndTurn,
            usage: UsageStats {
                input_tokens: 128,
                output_tokens: 64,
            },
        };

        let encoded_request =
            serde_json::to_string(&request).expect("request should serialize cleanly");
        let decoded_request: ProviderTurnRequest =
            serde_json::from_str(&encoded_request).expect("request should deserialize");
        let encoded_response =
            serde_json::to_string(&response).expect("response should serialize cleanly");
        let decoded_response: ProviderTurnResponse =
            serde_json::from_str(&encoded_response).expect("response should deserialize");

        assert_eq!(decoded_request, request);
        assert_eq!(decoded_response, response);
    }
}
