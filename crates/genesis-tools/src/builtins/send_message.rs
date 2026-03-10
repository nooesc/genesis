use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const TIMEOUT_SECS: u64 = 15;

/// Shared blocking HTTP client for platform API calls.
fn platform_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .user_agent("genesis-agent/0.1")
            .build()
            .expect("failed to build HTTP client")
    })
}

pub struct SendMessageTool;

impl ToolHandler for SendMessageTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let platform = call
            .arguments
            .get("platform")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "platform",
            })?
            .to_lowercase();

        let channel = call
            .arguments
            .get("channel")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "channel",
            })?;

        let message = call
            .arguments
            .get("message")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "message",
            })?;

        let thread_id = call.arguments.get("thread_id");

        match platform.as_str() {
            "slack" => send_slack(channel, message, thread_id, &call.name),
            "telegram" => send_telegram(channel, message, thread_id, &call.name),
            "discord" => send_discord(channel, message, &call.name),
            "whatsapp" => send_whatsapp(channel, message, &call.name),
            "homeassistant" | "home_assistant" => {
                send_homeassistant(channel, message, &call.name)
            }
            _ => Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "unsupported platform: {platform}. Supported: slack, telegram, discord, whatsapp, homeassistant"
                ),
            }),
        }
    }
}

fn send_slack(
    channel: &str,
    text: &str,
    thread_ts: Option<&String>,
    tool_name: &str,
) -> Result<ToolOutput, ToolError> {
    let token = std::env::var("SLACK_BOT_TOKEN").map_err(|_| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: "SLACK_BOT_TOKEN environment variable not set".to_owned(),
    })?;

    let mut body = serde_json::json!({
        "channel": channel,
        "text": text,
    });

    if let Some(ts) = thread_ts {
        body["thread_ts"] = serde_json::Value::String(ts.clone());
    }

    let resp = platform_client()
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(&token)
        .json(&body)
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("HTTP request failed: {e}"),
        })?;

    let resp_body: serde_json::Value =
        resp.json().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to parse response: {e}"),
        })?;

    if resp_body["ok"].as_bool() != Some(true) {
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!(
                "Slack API error: {}",
                resp_body["error"].as_str().unwrap_or("unknown")
            ),
        });
    }

    let ts = resp_body["ts"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();

    Ok(ToolOutput {
        content: format!("Message sent to Slack channel {channel} (ts: {ts})"),
        metadata: BTreeMap::from([
            ("tool".to_owned(), tool_name.to_owned()),
            ("platform".to_owned(), "slack".to_owned()),
            ("channel".to_owned(), channel.to_owned()),
            ("ts".to_owned(), ts),
        ]),
    })
}

fn send_telegram(
    chat_id: &str,
    text: &str,
    reply_to: Option<&String>,
    tool_name: &str,
) -> Result<ToolOutput, ToolError> {
    let token =
        std::env::var("TELEGRAM_BOT_TOKEN").map_err(|_| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "TELEGRAM_BOT_TOKEN environment variable not set".to_owned(),
        })?;

    // Telegram messages have a 4096 character limit; split if needed.
    let chunks = split_message(text, 4096);
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");

    let mut last_message_id = String::new();

    for (i, chunk) in chunks.iter().enumerate() {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": chunk,
        });

        if i == 0 {
            if let Some(reply_id) = reply_to {
                body["reply_to_message_id"] = serde_json::Value::String(reply_id.clone());
            }
        }

        let resp = platform_client()
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!("HTTP request failed: {e}"),
            })?;

        let resp_body: serde_json::Value =
            resp.json().map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!("failed to parse response: {e}"),
            })?;

        if resp_body["ok"].as_bool() != Some(true) {
            return Err(ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!(
                    "Telegram API error: {}",
                    resp_body["description"].as_str().unwrap_or("unknown")
                ),
            });
        }

        if let Some(msg_id) = resp_body["result"]["message_id"].as_i64() {
            last_message_id = msg_id.to_string();
        }
    }

    let chunk_info = if chunks.len() > 1 {
        format!(" ({} messages)", chunks.len())
    } else {
        String::new()
    };

    Ok(ToolOutput {
        content: format!("Message sent to Telegram chat {chat_id}{chunk_info}"),
        metadata: BTreeMap::from([
            ("tool".to_owned(), tool_name.to_owned()),
            ("platform".to_owned(), "telegram".to_owned()),
            ("chat_id".to_owned(), chat_id.to_owned()),
            ("message_id".to_owned(), last_message_id),
        ]),
    })
}

fn send_discord(
    channel_id: &str,
    content: &str,
    tool_name: &str,
) -> Result<ToolOutput, ToolError> {
    let token =
        std::env::var("DISCORD_BOT_TOKEN").map_err(|_| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "DISCORD_BOT_TOKEN environment variable not set".to_owned(),
        })?;

    // Discord has a 2000 character limit
    let truncated = if content.len() > 2000 {
        format!("{}...", &content[..1997])
    } else {
        content.to_owned()
    };

    let resp = platform_client()
        .post(format!(
            "https://discord.com/api/v10/channels/{channel_id}/messages"
        ))
        .header("Authorization", format!("Bot {token}"))
        .json(&serde_json::json!({ "content": truncated }))
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("HTTP request failed: {e}"),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Discord API error {status}: {body}"),
        });
    }

    let resp_body: serde_json::Value =
        resp.json().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to parse response: {e}"),
        })?;

    let msg_id = resp_body["id"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();

    Ok(ToolOutput {
        content: format!("Message sent to Discord channel {channel_id} (id: {msg_id})"),
        metadata: BTreeMap::from([
            ("tool".to_owned(), tool_name.to_owned()),
            ("platform".to_owned(), "discord".to_owned()),
            ("channel_id".to_owned(), channel_id.to_owned()),
            ("message_id".to_owned(), msg_id),
        ]),
    })
}

fn send_whatsapp(
    recipient: &str,
    text: &str,
    tool_name: &str,
) -> Result<ToolOutput, ToolError> {
    let token = std::env::var("WHATSAPP_TOKEN").map_err(|_| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: "WHATSAPP_TOKEN environment variable not set".to_owned(),
    })?;

    let phone_number_id =
        std::env::var("WHATSAPP_PHONE_NUMBER_ID").map_err(|_| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "WHATSAPP_PHONE_NUMBER_ID environment variable not set".to_owned(),
        })?;

    // WhatsApp has a 4096 character limit
    let truncated = if text.len() > 4096 {
        format!("{}...", &text[..4093])
    } else {
        text.to_owned()
    };

    let url = format!(
        "https://graph.facebook.com/v21.0/{phone_number_id}/messages"
    );

    let body = serde_json::json!({
        "messaging_product": "whatsapp",
        "to": recipient,
        "type": "text",
        "text": { "body": truncated }
    });

    let resp = platform_client()
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("WhatsApp API request failed: {e}"),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let resp_body = resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("WhatsApp API error {status}: {resp_body}"),
        });
    }

    let resp_body: serde_json::Value =
        resp.json().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to parse WhatsApp response: {e}"),
        })?;

    let msg_id = resp_body["messages"][0]["id"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();

    Ok(ToolOutput {
        content: format!("Message sent to WhatsApp recipient {recipient} (id: {msg_id})"),
        metadata: BTreeMap::from([
            ("tool".to_owned(), tool_name.to_owned()),
            ("platform".to_owned(), "whatsapp".to_owned()),
            ("recipient".to_owned(), recipient.to_owned()),
            ("message_id".to_owned(), msg_id),
        ]),
    })
}

fn send_homeassistant(
    service_target: &str,
    message: &str,
    tool_name: &str,
) -> Result<ToolOutput, ToolError> {
    let ha_url =
        std::env::var("HOMEASSISTANT_URL").map_err(|_| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "HOMEASSISTANT_URL environment variable not set".to_owned(),
        })?;

    let ha_token = std::env::var("HOMEASSISTANT_LONG_LIVED_TOKEN").map_err(|_| {
        ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "HOMEASSISTANT_LONG_LIVED_TOKEN environment variable not set".to_owned(),
        }
    })?;

    // service_target can be:
    //   "notify.persistent_notification" — creates a HA notification
    //   "notify.mobile_app_<device>" — sends push to mobile app
    //   "tts.speak" — text-to-speech on a media player
    // Default to persistent_notification if just a plain name is given
    let (domain, service) = if let Some((d, s)) = service_target.split_once('.') {
        (d, s)
    } else {
        ("notify", service_target)
    };

    let base = ha_url.trim_end_matches('/');
    let url = format!("{base}/api/services/{domain}/{service}");

    let body = serde_json::json!({
        "message": message,
        "title": "Genesis Agent"
    });

    let resp = platform_client()
        .post(&url)
        .bearer_auth(&ha_token)
        .json(&body)
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Home Assistant API request failed: {e}"),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let resp_body = resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Home Assistant API error {status}: {resp_body}"),
        });
    }

    Ok(ToolOutput {
        content: format!(
            "Message sent via Home Assistant service {domain}.{service}"
        ),
        metadata: BTreeMap::from([
            ("tool".to_owned(), tool_name.to_owned()),
            ("platform".to_owned(), "homeassistant".to_owned()),
            ("service".to_owned(), format!("{domain}.{service}")),
        ]),
    })
}

/// Split a message into chunks respecting a max length.
/// Tries to split on newlines, then word boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_owned());
            break;
        }

        let split_at = remaining[..max_len]
            .rfind('\n')
            .or_else(|| remaining[..max_len].rfind(' '))
            .unwrap_or(max_len);

        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk.to_owned());
        remaining = rest.trim_start_matches('\n');
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test".to_owned(),
            profile: "test".to_owned(),
            data_dir: "/tmp".to_owned(),
            allow_destructive_tools: false,
            terminal_backend: None,
            default_working_dir: None,
            sandbox_manager: None,
        }
    }

    #[test]
    fn requires_platform() {
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn requires_channel() {
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([("platform".to_owned(), "slack".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "channel",
                ..
            }
        ));
    }

    #[test]
    fn requires_message() {
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "slack".to_owned()),
                ("channel".to_owned(), "C123".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(
            err,
            ToolError::MissingArgument {
                argument: "message",
                ..
            }
        ));
    }

    #[test]
    fn rejects_unsupported_platform() {
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "fax".to_owned()),
                ("channel".to_owned(), "C123".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("unsupported platform"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn slack_requires_token() {
        // Ensure the env var is NOT set for this test
        std::env::remove_var("SLACK_BOT_TOKEN");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "slack".to_owned()),
                ("channel".to_owned(), "C123".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("SLACK_BOT_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn telegram_requires_token() {
        std::env::remove_var("TELEGRAM_BOT_TOKEN");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "telegram".to_owned()),
                ("channel".to_owned(), "12345".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("TELEGRAM_BOT_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn discord_requires_token() {
        std::env::remove_var("DISCORD_BOT_TOKEN");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "discord".to_owned()),
                ("channel".to_owned(), "123456789".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("DISCORD_BOT_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn split_message_short_text() {
        let chunks = split_message("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_at_newline() {
        let text = "line1\nline2\nline3";
        let chunks = split_message(text, 10);
        assert_eq!(chunks[0], "line1");
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn split_message_at_word_boundary() {
        let text = "hello world foo bar";
        let chunks = split_message(text, 12);
        assert_eq!(chunks[0], "hello world");
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn whatsapp_requires_token() {
        std::env::remove_var("WHATSAPP_TOKEN");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "whatsapp".to_owned()),
                ("channel".to_owned(), "+1234567890".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("WHATSAPP_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn homeassistant_requires_url() {
        std::env::remove_var("HOMEASSISTANT_URL");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "homeassistant".to_owned()),
                ("channel".to_owned(), "persistent_notification".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("HOMEASSISTANT_URL"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn homeassistant_accepts_alternate_name() {
        std::env::remove_var("HOMEASSISTANT_URL");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "home_assistant".to_owned()),
                ("channel".to_owned(), "persistent_notification".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        // Should route to homeassistant handler, not unsupported platform
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("HOMEASSISTANT_URL"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn platform_name_is_case_insensitive() {
        std::env::remove_var("SLACK_BOT_TOKEN");
        let tool = SendMessageTool;
        let call = ToolCall {
            name: "send_message".to_owned(),
            arguments: BTreeMap::from([
                ("platform".to_owned(), "SLACK".to_owned()),
                ("channel".to_owned(), "C123".to_owned()),
                ("message".to_owned(), "hello".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        // Should fail with missing token, not unsupported platform
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("SLACK_BOT_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
