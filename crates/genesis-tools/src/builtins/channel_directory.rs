use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use genesis_storage::{CachedChannel, ChannelStore};

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

/// Cache TTL: channels are refreshed if older than 5 minutes.
const CACHE_TTL_SECS: i64 = 300;

const TIMEOUT_SECS: u64 = 15;

fn api_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .user_agent("genesis-agent/0.1")
            .build()
            .expect("failed to build HTTP client")
    })
}

pub struct ListChannelsTool;

impl ToolHandler for ListChannelsTool {
    fn run(&self, call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let platform = call.arguments.get("platform").map(|s| s.to_lowercase());

        let refresh = call
            .arguments
            .get("refresh")
            .map(|v| v == "true")
            .unwrap_or(false);

        let db_path = context.db_path();
        let store = ChannelStore::new(&db_path);

        // Determine which platforms to query.
        let platforms: Vec<String> = if let Some(ref p) = platform {
            vec![p.clone()]
        } else {
            configured_platforms()
        };

        if platforms.is_empty() {
            return Ok(ToolOutput {
                content: "No messaging platforms configured. Set SLACK_BOT_TOKEN, TELEGRAM_BOT_TOKEN, or DISCORD_BOT_TOKEN to enable channel discovery.".to_owned(),
                metadata: BTreeMap::new(),
            });
        }

        // Fetch/refresh channels for each platform.
        for p in &platforms {
            let fresh = !refresh && store.is_fresh(p, CACHE_TTL_SECS).unwrap_or(false);

            if !fresh {
                let fetched = fetch_channels(p, &call.name)?;
                let _ = store.upsert_channels(p, &fetched);
            }
        }

        // Read from cache.
        let channels = store
            .list(platform.as_deref())
            .map_err(|e| ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("failed to read channel cache: {e}"),
            })?;

        if channels.is_empty() {
            return Ok(ToolOutput {
                content: format!(
                    "No channels found for {}.",
                    platform.as_deref().unwrap_or("any configured platform")
                ),
                metadata: BTreeMap::new(),
            });
        }

        let formatted = format_channels(&channels);
        Ok(ToolOutput {
            content: formatted,
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("count".to_owned(), channels.len().to_string()),
            ]),
        })
    }
}

/// Return the list of platforms that have API tokens configured.
fn configured_platforms() -> Vec<String> {
    let mut platforms = Vec::new();
    if genesis_config::env::is_present(genesis_config::env::SLACK_BOT_TOKEN) {
        platforms.push("slack".to_owned());
    }
    if genesis_config::env::is_present(genesis_config::env::TELEGRAM_BOT_TOKEN) {
        platforms.push("telegram".to_owned());
    }
    if genesis_config::env::is_present(genesis_config::env::DISCORD_BOT_TOKEN) {
        platforms.push("discord".to_owned());
    }
    if genesis_config::env::is_present(genesis_config::env::WHATSAPP_TOKEN) {
        platforms.push("whatsapp".to_owned());
    }
    if genesis_config::env::is_present(genesis_config::env::HOMEASSISTANT_URL) {
        platforms.push("homeassistant".to_owned());
    }
    platforms
}

fn fetch_channels(platform: &str, tool_name: &str) -> Result<Vec<CachedChannel>, ToolError> {
    match platform {
        "slack" => fetch_slack_channels(tool_name),
        "telegram" => Ok(Vec::new()), // Telegram doesn't have a list-channels API for bots
        "discord" => fetch_discord_channels(tool_name),
        "whatsapp" => fetch_whatsapp_info(tool_name),
        "homeassistant" => fetch_homeassistant_services(tool_name),
        _ => Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!(
                "unsupported platform: {platform}. Supported: slack, telegram, discord, whatsapp, homeassistant"
            ),
        }),
    }
}

/// Fetch Slack channels via conversations.list, paginating through all results.
fn fetch_slack_channels(tool_name: &str) -> Result<Vec<CachedChannel>, ToolError> {
    let token = genesis_config::env::get(genesis_config::env::SLACK_BOT_TOKEN).map_err(|_| {
        ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "SLACK_BOT_TOKEN environment variable not set".to_owned(),
        }
    })?;

    let mut all_channels = Vec::new();
    let mut cursor = String::new();

    loop {
        let mut url = "https://slack.com/api/conversations.list?types=public_channel,private_channel,mpim,im&limit=200".to_owned();
        if !cursor.is_empty() {
            url.push_str("&cursor=");
            url.push_str(&cursor);
        }

        let resp = api_client()
            .get(&url)
            .bearer_auth(&token)
            .send()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!("Slack API request failed: {e}"),
            })?;

        let body: serde_json::Value = resp.json().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to parse Slack response: {e}"),
        })?;

        if body["ok"].as_bool() != Some(true) {
            return Err(ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!(
                    "Slack API error: {}",
                    body["error"].as_str().unwrap_or("unknown")
                ),
            });
        }

        if let Some(channels) = body["channels"].as_array() {
            for ch in channels {
                let channel_type = if ch["is_im"].as_bool() == Some(true) {
                    "dm"
                } else if ch["is_mpim"].as_bool() == Some(true) {
                    "group_dm"
                } else if ch["is_private"].as_bool() == Some(true) {
                    "private"
                } else {
                    "channel"
                };

                let name = ch["name"]
                    .as_str()
                    .or_else(|| ch["user"].as_str())
                    .unwrap_or("unknown")
                    .to_owned();

                all_channels.push(CachedChannel {
                    platform: "slack".to_owned(),
                    channel_id: ch["id"].as_str().unwrap_or("").to_owned(),
                    channel_name: name,
                    channel_type: channel_type.to_owned(),
                    is_member: ch["is_member"].as_bool().unwrap_or(false),
                    cached_at: String::new(), // set by the store
                });
            }
        }

        // Paginate
        let next_cursor = body["response_metadata"]["next_cursor"]
            .as_str()
            .unwrap_or("");
        if next_cursor.is_empty() {
            break;
        }
        cursor = next_cursor.to_owned();
    }

    Ok(all_channels)
}

/// Fetch Discord channels: first get guilds the bot is in, then channels per guild.
fn fetch_discord_channels(tool_name: &str) -> Result<Vec<CachedChannel>, ToolError> {
    let token = genesis_config::env::get(genesis_config::env::DISCORD_BOT_TOKEN).map_err(|_| {
        ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "DISCORD_BOT_TOKEN environment variable not set".to_owned(),
        }
    })?;

    let auth = format!("Bot {token}");

    // Step 1: Get guilds
    let guilds_resp = api_client()
        .get("https://discord.com/api/v10/users/@me/guilds")
        .header("Authorization", &auth)
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Discord guilds request failed: {e}"),
        })?;

    if !guilds_resp.status().is_success() {
        let status = guilds_resp.status();
        let body = guilds_resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Discord guilds API error {status}: {body}"),
        });
    }

    let guilds: Vec<serde_json::Value> =
        guilds_resp.json().map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("failed to parse Discord guilds: {e}"),
        })?;

    let mut all_channels = Vec::new();

    // Step 2: Get channels for each guild
    for guild in &guilds {
        let guild_id = guild["id"].as_str().unwrap_or("");
        if guild_id.is_empty() {
            continue;
        }

        let channels_resp = api_client()
            .get(format!(
                "https://discord.com/api/v10/guilds/{guild_id}/channels"
            ))
            .header("Authorization", &auth)
            .send()
            .map_err(|e| ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: format!("Discord channels request failed: {e}"),
            })?;

        if !channels_resp.status().is_success() {
            continue; // Skip guilds we can't read
        }

        let channels: Vec<serde_json::Value> = channels_resp.json().unwrap_or_default();

        for ch in &channels {
            // type 0 = text, 2 = voice, 4 = category, 5 = announcement, 15 = forum
            let ch_type = ch["type"].as_u64().unwrap_or(0);
            let type_name = match ch_type {
                0 => "text",
                2 => "voice",
                5 => "announcement",
                15 => "forum",
                _ => continue, // Skip categories and other non-message types
            };

            let guild_name = guild["name"].as_str().unwrap_or("unknown");
            let ch_name = ch["name"].as_str().unwrap_or("unknown");

            all_channels.push(CachedChannel {
                platform: "discord".to_owned(),
                channel_id: ch["id"].as_str().unwrap_or("").to_owned(),
                channel_name: format!("{guild_name}/#{ch_name}"),
                channel_type: type_name.to_owned(),
                is_member: true, // Bot is in the guild
                cached_at: String::new(),
            });
        }
    }

    Ok(all_channels)
}

/// WhatsApp Cloud API doesn't support listing contacts, but we can show the
/// configured phone number ID so the agent knows the platform is available.
fn fetch_whatsapp_info(tool_name: &str) -> Result<Vec<CachedChannel>, ToolError> {
    let _token = genesis_config::env::get(genesis_config::env::WHATSAPP_TOKEN).map_err(|_| {
        ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "WHATSAPP_TOKEN environment variable not set".to_owned(),
        }
    })?;

    let phone_id = genesis_config::env::get(genesis_config::env::WHATSAPP_PHONE_NUMBER_ID)
        .map_err(|_| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "WHATSAPP_PHONE_NUMBER_ID environment variable not set".to_owned(),
        })?;

    // Return a single entry representing the configured WhatsApp business number.
    // Actual recipients must be specified by phone number.
    Ok(vec![CachedChannel {
        platform: "whatsapp".to_owned(),
        channel_id: phone_id.clone(),
        channel_name: format!("Business Phone (ID: {phone_id})"),
        channel_type: "phone".to_owned(),
        is_member: true,
        cached_at: String::new(),
    }])
}

/// Fetch available notification services from Home Assistant REST API.
fn fetch_homeassistant_services(tool_name: &str) -> Result<Vec<CachedChannel>, ToolError> {
    let ha_url =
        genesis_config::env::get(genesis_config::env::HOMEASSISTANT_URL).map_err(|_| {
            ToolError::ExecutionFailed {
                tool: tool_name.to_owned(),
                reason: "HOMEASSISTANT_URL environment variable not set".to_owned(),
            }
        })?;

    let ha_token = genesis_config::env::get(genesis_config::env::HOMEASSISTANT_LONG_LIVED_TOKEN)
        .map_err(|_| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: "HOMEASSISTANT_LONG_LIVED_TOKEN environment variable not set".to_owned(),
        })?;

    let base = ha_url.trim_end_matches('/');
    let url = format!("{base}/api/services");

    let resp = api_client()
        .get(&url)
        .bearer_auth(&ha_token)
        .send()
        .map_err(|e| ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Home Assistant API request failed: {e}"),
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(ToolError::ExecutionFailed {
            tool: tool_name.to_owned(),
            reason: format!("Home Assistant API error {status}: {body}"),
        });
    }

    let services: Vec<serde_json::Value> = resp.json().map_err(|e| ToolError::ExecutionFailed {
        tool: tool_name.to_owned(),
        reason: format!("failed to parse Home Assistant services: {e}"),
    })?;

    let mut channels = Vec::new();

    // Extract notify.* services which can receive messages
    for domain in &services {
        let domain_name = domain["domain"].as_str().unwrap_or("");
        if domain_name != "notify" {
            continue;
        }
        if let Some(service_list) = domain["services"].as_object() {
            for (service_name, _service_def) in service_list {
                channels.push(CachedChannel {
                    platform: "homeassistant".to_owned(),
                    channel_id: format!("notify.{service_name}"),
                    channel_name: format!("notify.{service_name}"),
                    channel_type: "service".to_owned(),
                    is_member: true,
                    cached_at: String::new(),
                });
            }
        }
    }

    Ok(channels)
}

fn format_channels(channels: &[CachedChannel]) -> String {
    let mut output = String::new();
    let mut current_platform = "";

    for ch in channels {
        if ch.platform != current_platform {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&format!("## {}\n", capitalize(&ch.platform)));
            current_platform = &ch.platform;
        }

        let member_marker = if ch.is_member { " [member]" } else { "" };
        output.push_str(&format!(
            "- {} (id: {}, type: {}){}\n",
            ch.channel_name, ch.channel_id, ch.channel_type, member_marker
        ));
    }

    output
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join("genesis-channel-test");
        std::fs::create_dir_all(&dir).ok();
        genesis_storage::bootstrap(&dir.join("genesis.db")).ok();
        ToolContext {
            data_dir: dir.to_string_lossy().to_string(),
            ..crate::test_utils::test_ctx()
        }
    }

    #[test]
    fn no_platforms_configured() {
        std::env::remove_var("SLACK_BOT_TOKEN");
        std::env::remove_var("TELEGRAM_BOT_TOKEN");
        std::env::remove_var("DISCORD_BOT_TOKEN");

        let tool = ListChannelsTool;
        let call = ToolCall {
            name: "list_channels".to_owned(),
            arguments: BTreeMap::new(),
        };
        let result = tool.run(&call, &ctx()).unwrap();
        assert!(result.content.contains("No messaging platforms configured"));
    }

    #[test]
    fn unsupported_platform_errors() {
        let tool = ListChannelsTool;
        let call = ToolCall {
            name: "list_channels".to_owned(),
            arguments: BTreeMap::from([("platform".to_owned(), "fax".to_owned())]),
        };
        let result = tool.run(&call, &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn format_channels_groups_by_platform() {
        let channels = vec![
            CachedChannel {
                platform: "slack".to_owned(),
                channel_id: "C123".to_owned(),
                channel_name: "general".to_owned(),
                channel_type: "channel".to_owned(),
                is_member: true,
                cached_at: "2025-01-01".to_owned(),
            },
            CachedChannel {
                platform: "slack".to_owned(),
                channel_id: "C456".to_owned(),
                channel_name: "random".to_owned(),
                channel_type: "channel".to_owned(),
                is_member: false,
                cached_at: "2025-01-01".to_owned(),
            },
            CachedChannel {
                platform: "discord".to_owned(),
                channel_id: "789".to_owned(),
                channel_name: "MyServer/#chat".to_owned(),
                channel_type: "text".to_owned(),
                is_member: true,
                cached_at: "2025-01-01".to_owned(),
            },
        ];

        let output = format_channels(&channels);
        assert!(output.contains("## Slack"));
        assert!(output.contains("## Discord"));
        assert!(output.contains("general (id: C123"));
        assert!(output.contains("[member]"));
        assert!(output.contains("MyServer/#chat"));
    }

    #[test]
    fn channel_store_upsert_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).unwrap();

        let store = ChannelStore::new(&db_path);

        let channels = vec![
            CachedChannel {
                platform: "slack".to_owned(),
                channel_id: "C001".to_owned(),
                channel_name: "alpha".to_owned(),
                channel_type: "channel".to_owned(),
                is_member: true,
                cached_at: String::new(),
            },
            CachedChannel {
                platform: "slack".to_owned(),
                channel_id: "C002".to_owned(),
                channel_name: "beta".to_owned(),
                channel_type: "private".to_owned(),
                is_member: false,
                cached_at: String::new(),
            },
        ];

        let count = store.upsert_channels("slack", &channels).unwrap();
        assert_eq!(count, 2);

        let listed = store.list(Some("slack")).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].channel_name, "alpha");
        assert_eq!(listed[1].channel_name, "beta");

        // Upsert again replaces old entries
        let updated = vec![CachedChannel {
            platform: "slack".to_owned(),
            channel_id: "C003".to_owned(),
            channel_name: "gamma".to_owned(),
            channel_type: "channel".to_owned(),
            is_member: true,
            cached_at: String::new(),
        }];
        store.upsert_channels("slack", &updated).unwrap();
        let listed = store.list(Some("slack")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].channel_name, "gamma");
    }

    #[test]
    fn channel_store_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("genesis.db");
        genesis_storage::bootstrap(&db_path).unwrap();

        let store = ChannelStore::new(&db_path);

        // No channels → not fresh
        assert!(!store.is_fresh("slack", 300).unwrap());

        // Insert a channel
        let channels = vec![CachedChannel {
            platform: "slack".to_owned(),
            channel_id: "C001".to_owned(),
            channel_name: "test".to_owned(),
            channel_type: "channel".to_owned(),
            is_member: true,
            cached_at: String::new(),
        }];
        store.upsert_channels("slack", &channels).unwrap();

        // Should be fresh (just inserted)
        assert!(store.is_fresh("slack", 300).unwrap());

        // With a very short TTL, still fresh because we just inserted
        assert!(store.is_fresh("slack", 1).unwrap());

        // Different platform → not fresh
        assert!(!store.is_fresh("discord", 300).unwrap());
    }

    #[test]
    fn slack_requires_token_for_fetch() {
        std::env::remove_var("SLACK_BOT_TOKEN");
        let result = fetch_slack_channels("list_channels");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("SLACK_BOT_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn discord_requires_token_for_fetch() {
        std::env::remove_var("DISCORD_BOT_TOKEN");
        let result = fetch_discord_channels("list_channels");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("DISCORD_BOT_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn telegram_returns_empty() {
        // Telegram bots can't list channels; ensure graceful empty return
        let result = fetch_channels("telegram", "list_channels").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn capitalize_works() {
        assert_eq!(capitalize("slack"), "Slack");
        assert_eq!(capitalize("discord"), "Discord");
        assert_eq!(capitalize(""), "");
    }
}
