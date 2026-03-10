//! Home Assistant tools for controlling smart home devices via REST API.
//!
//! Four LLM-callable tools:
//! - `ha_list_entities`  — list/filter entities by domain or area
//! - `ha_get_state`      — get detailed state of a single entity
//! - `ha_list_services`  — list available services per domain
//! - `ha_call_service`   — call a service (turn_on, turn_off, set_temperature, etc.)
//!
//! Authentication via `HASS_TOKEN` env var (Long-Lived Access Token).
//! HA instance URL from `HASS_URL` (default: `http://homeassistant.local:8123`).

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::{ToolCall, ToolContext, ToolError, ToolHandler, ToolOutput};

const TIMEOUT_SECS: u64 = 15;
const MAX_RESPONSE_ENTITIES: usize = 500;

/// Blocked service domains that allow arbitrary code/command execution
/// or enable SSRF attacks on the HA host.
const BLOCKED_DOMAINS: &[&str] = &[
    "shell_command",
    "command_line",
    "python_script",
    "pyscript",
    "hassio",
    "rest_command",
];

fn is_blocked_domain(domain: &str) -> bool {
    BLOCKED_DOMAINS.contains(&domain)
}

fn ha_config() -> (String, String) {
    let url = std::env::var("HASS_URL")
        .unwrap_or_else(|_| "http://homeassistant.local:8123".to_owned());
    let token = std::env::var("HASS_TOKEN").unwrap_or_default();
    (url.trim_end_matches('/').to_owned(), token)
}

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .user_agent("genesis-agent/0.1")
            .build()
            .expect("failed to build HTTP client")
    })
}

fn validate_entity_id(entity_id: &str) -> bool {
    // Format: domain.object_id where both are lowercase alphanumeric + underscore
    let parts: Vec<&str> = entity_id.splitn(2, '.').collect();
    if parts.len() != 2 {
        return false;
    }
    let valid_part = |s: &str| -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    valid_part(parts[0]) && valid_part(parts[1])
}

fn ha_get(path: &str) -> Result<serde_json::Value, String> {
    let (url, token) = ha_config();
    if token.is_empty() {
        return Err("HASS_TOKEN environment variable is not set".to_owned());
    }

    let resp = http_client()
        .get(format!("{url}{path}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send()
        .map_err(|e| format!("HA request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .map_err(|e| format!("failed to read HA response: {e}"))?;

    if !status.is_success() {
        return Err(format!("HA API returned {status}: {body}"));
    }

    serde_json::from_str(&body).map_err(|e| format!("invalid JSON from HA: {e}"))
}

fn ha_post(path: &str, payload: &serde_json::Value) -> Result<serde_json::Value, String> {
    let (url, token) = ha_config();
    if token.is_empty() {
        return Err("HASS_TOKEN environment variable is not set".to_owned());
    }

    let resp = http_client()
        .post(format!("{url}{path}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(payload)
        .send()
        .map_err(|e| format!("HA request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .map_err(|e| format!("failed to read HA response: {e}"))?;

    if !status.is_success() {
        return Err(format!("HA API returned {status}: {body}"));
    }

    serde_json::from_str(&body).map_err(|e| format!("invalid JSON from HA: {e}"))
}

// ── ha_list_entities ──

pub struct HaListEntitiesTool;

impl ToolHandler for HaListEntitiesTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let domain = call.arguments.get("domain");
        let area = call.arguments.get("area");

        let states = ha_get("/api/states").map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: e,
        })?;

        let all_states = states.as_array().ok_or_else(|| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: "unexpected HA response format".to_owned(),
        })?;

        let filtered: Vec<serde_json::Value> = all_states
            .iter()
            .filter(|s| {
                if let Some(d) = domain {
                    let eid = s["entity_id"].as_str().unwrap_or("");
                    if !eid.starts_with(&format!("{d}.")) {
                        return false;
                    }
                }
                if let Some(a) = area {
                    let a_lower = a.to_lowercase();
                    let fname = s["attributes"]["friendly_name"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase();
                    let area_attr = s["attributes"]["area"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase();
                    if !fname.contains(&a_lower) && !area_attr.contains(&a_lower) {
                        return false;
                    }
                }
                true
            })
            .take(MAX_RESPONSE_ENTITIES)
            .map(|s| {
                serde_json::json!({
                    "entity_id": s["entity_id"],
                    "state": s["state"],
                    "friendly_name": s["attributes"]["friendly_name"],
                })
            })
            .collect();

        let result = serde_json::json!({
            "count": filtered.len(),
            "entities": filtered,
        });

        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&result).unwrap_or_default(),
            metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
        })
    }
}

// ── ha_get_state ──

pub struct HaGetStateTool;

impl ToolHandler for HaGetStateTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let entity_id =
            call.arguments
                .get("entity_id")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "entity_id",
                })?;

        if !validate_entity_id(entity_id) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!("invalid entity_id format: {entity_id}"),
            });
        }

        let data =
            ha_get(&format!("/api/states/{entity_id}")).map_err(|e| {
                ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: e,
                }
            })?;

        let result = serde_json::json!({
            "entity_id": data["entity_id"],
            "state": data["state"],
            "attributes": data["attributes"],
            "last_changed": data["last_changed"],
            "last_updated": data["last_updated"],
        });

        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&result).unwrap_or_default(),
            metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
        })
    }
}

// ── ha_list_services ──

pub struct HaListServicesTool;

impl ToolHandler for HaListServicesTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let domain_filter = call.arguments.get("domain");

        let services = ha_get("/api/services").map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: e,
        })?;

        let all_services = services.as_array().ok_or_else(|| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: "unexpected HA response format".to_owned(),
        })?;

        let domains: Vec<serde_json::Value> = all_services
            .iter()
            .filter(|s| {
                if let Some(df) = domain_filter {
                    s["domain"].as_str().unwrap_or("") == df.as_str()
                } else {
                    true
                }
            })
            .map(|s| {
                let domain = s["domain"].as_str().unwrap_or("");
                let services_obj = s["services"].as_object();
                let compact_services: BTreeMap<String, serde_json::Value> = services_obj
                    .map(|svcs| {
                        svcs.iter()
                            .map(|(name, info)| {
                                let desc = info["description"].as_str().unwrap_or("");
                                let fields: BTreeMap<String, String> = info["fields"]
                                    .as_object()
                                    .map(|f| {
                                        f.iter()
                                            .filter_map(|(k, v)| {
                                                v["description"]
                                                    .as_str()
                                                    .map(|d| (k.clone(), d.to_owned()))
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();

                                let mut entry = serde_json::json!({ "description": desc });
                                if !fields.is_empty() {
                                    entry["fields"] = serde_json::to_value(&fields).unwrap();
                                }
                                (name.clone(), entry)
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                serde_json::json!({
                    "domain": domain,
                    "services": compact_services,
                })
            })
            .collect();

        let result = serde_json::json!({
            "count": domains.len(),
            "domains": domains,
        });

        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&result).unwrap_or_default(),
            metadata: BTreeMap::from([("tool".to_owned(), call.name.clone())]),
        })
    }
}

// ── ha_call_service ──

pub struct HaCallServiceTool;

impl ToolHandler for HaCallServiceTool {
    fn run(&self, call: &ToolCall, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let domain = call
            .arguments
            .get("domain")
            .ok_or_else(|| ToolError::MissingArgument {
                tool: call.name.clone(),
                argument: "domain",
            })?;

        let service =
            call.arguments
                .get("service")
                .ok_or_else(|| ToolError::MissingArgument {
                    tool: call.name.clone(),
                    argument: "service",
                })?;

        if is_blocked_domain(domain) {
            return Err(ToolError::ExecutionFailed {
                tool: call.name.clone(),
                reason: format!(
                    "service domain '{domain}' is blocked for security. \
                     Blocked domains: {}",
                    BLOCKED_DOMAINS.join(", ")
                ),
            });
        }

        let entity_id = call.arguments.get("entity_id");
        if let Some(eid) = entity_id {
            if !validate_entity_id(eid) {
                return Err(ToolError::ExecutionFailed {
                    tool: call.name.clone(),
                    reason: format!("invalid entity_id format: {eid}"),
                });
            }
        }

        // Build payload from optional `data` JSON string + entity_id
        let mut payload = if let Some(data_str) = call.arguments.get("data") {
            serde_json::from_str::<serde_json::Value>(data_str).unwrap_or_else(|_| {
                serde_json::json!({})
            })
        } else {
            serde_json::json!({})
        };

        if let Some(eid) = entity_id {
            payload["entity_id"] = serde_json::Value::String(eid.clone());
        }

        let result = ha_post(
            &format!("/api/services/{domain}/{service}"),
            &payload,
        )
        .map_err(|e| ToolError::ExecutionFailed {
            tool: call.name.clone(),
            reason: e,
        })?;

        // Parse affected entities from response
        let affected: Vec<serde_json::Value> = result
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|s| {
                        serde_json::json!({
                            "entity_id": s["entity_id"],
                            "state": s["state"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let response = serde_json::json!({
            "success": true,
            "service": format!("{domain}.{service}"),
            "affected_entities": affected,
        });

        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&response).unwrap_or_default(),
            metadata: BTreeMap::from([
                ("tool".to_owned(), call.name.clone()),
                ("service".to_owned(), format!("{domain}.{service}")),
            ]),
        })
    }
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
    fn validate_entity_id_accepts_valid() {
        assert!(validate_entity_id("light.living_room"));
        assert!(validate_entity_id("sensor.temperature_1"));
        assert!(validate_entity_id("climate.thermostat"));
        assert!(validate_entity_id("binary_sensor.motion"));
    }

    #[test]
    fn validate_entity_id_rejects_invalid() {
        assert!(!validate_entity_id(""));
        assert!(!validate_entity_id("nodot"));
        assert!(!validate_entity_id("Light.room")); // uppercase
        assert!(!validate_entity_id(".missing_domain"));
        assert!(!validate_entity_id("domain."));
        assert!(!validate_entity_id("a.b.c")); // only first dot splits
    }

    #[test]
    fn blocked_domains_are_detected() {
        assert!(is_blocked_domain("shell_command"));
        assert!(is_blocked_domain("hassio"));
        assert!(is_blocked_domain("rest_command"));
        assert!(!is_blocked_domain("light"));
        assert!(!is_blocked_domain("climate"));
    }

    #[test]
    fn ha_get_state_requires_entity_id() {
        let tool = HaGetStateTool;
        let call = ToolCall {
            name: "ha_get_state".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn ha_get_state_rejects_invalid_entity_id() {
        let tool = HaGetStateTool;
        let call = ToolCall {
            name: "ha_get_state".to_owned(),
            arguments: BTreeMap::from([("entity_id".to_owned(), "INVALID".to_owned())]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("invalid entity_id"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ha_call_service_requires_domain_and_service() {
        let tool = HaCallServiceTool;
        let call = ToolCall {
            name: "ha_call_service".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        assert!(matches!(err, ToolError::MissingArgument { .. }));
    }

    #[test]
    fn ha_call_service_blocks_dangerous_domains() {
        let tool = HaCallServiceTool;
        let call = ToolCall {
            name: "ha_call_service".to_owned(),
            arguments: BTreeMap::from([
                ("domain".to_owned(), "shell_command".to_owned()),
                ("service".to_owned(), "run".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("blocked for security"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ha_call_service_rejects_invalid_entity_id() {
        let tool = HaCallServiceTool;
        let call = ToolCall {
            name: "ha_call_service".to_owned(),
            arguments: BTreeMap::from([
                ("domain".to_owned(), "light".to_owned()),
                ("service".to_owned(), "turn_on".to_owned()),
                ("entity_id".to_owned(), "BAD FORMAT".to_owned()),
            ]),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("invalid entity_id"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ha_list_entities_fails_without_token() {
        // Clear HASS_TOKEN to ensure it's not set
        std::env::remove_var("HASS_TOKEN");
        let tool = HaListEntitiesTool;
        let call = ToolCall {
            name: "ha_list_entities".to_owned(),
            arguments: BTreeMap::new(),
        };
        let err = tool.run(&call, &ctx()).unwrap_err();
        match err {
            ToolError::ExecutionFailed { reason, .. } => {
                assert!(reason.contains("HASS_TOKEN"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
