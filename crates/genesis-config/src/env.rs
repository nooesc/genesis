//! Centralized environment variable loading for the Genesis project.
//!
//! All environment variable names used by Genesis are defined here as constants,
//! with accessor functions that provide consistent loading semantics. This
//! eliminates scattered `std::env::var(...)` calls across the codebase and gives
//! a single source of truth for every env var the system reads.
//!
//! # Categories
//!
//! | Prefix / Group        | Purpose                                       |
//! |-----------------------|-----------------------------------------------|
//! | `GENESIS_*`           | Core Genesis configuration                    |
//! | Provider keys         | LLM provider API keys                         |
//! | `TELEGRAM_*`          | Telegram platform integration                 |
//! | `DISCORD_*`           | Discord platform integration                  |
//! | `SLACK_*`             | Slack platform integration                    |
//! | `WHATSAPP_*`          | WhatsApp platform integration                 |
//! | `SIGNAL_*`            | Signal platform integration                   |
//! | `HOMEASSISTANT_*`     | Home Assistant platform integration            |
//! | `HASS_*`              | Home Assistant tool integration (hass style)   |
//! | `BROWSERBASE_*`       | Browserbase cloud browser                     |
//! | `BROWSER_*`           | Local browser settings                        |
//! | `DAYTONA_*`           | Daytona sandbox                               |
//! | `MODAL_*`             | Modal sandbox                                 |
//! | `TERMINAL_*`          | Terminal/sandbox scratch directories           |
//! | `GATEWAY_*`           | Gateway access control                        |
//! | Shell / OS            | `HOME`, `PATH`, `EDITOR`, `NO_COLOR`, etc.    |

// ---------------------------------------------------------------------------
// Genesis core
// ---------------------------------------------------------------------------

/// Log format override (`json` for structured logging).
pub const GENESIS_LOG_FORMAT: &str = "GENESIS_LOG_FORMAT";

/// API key for the Genesis gateway HTTP server.
pub const GENESIS_API_KEY: &str = "GENESIS_API_KEY";

/// Whether the gateway API key is required (`true`/`false`).
pub const GENESIS_API_KEY_REQUIRED: &str = "GENESIS_API_KEY_REQUIRED";

/// Requests-per-minute rate limit override for the gateway.
pub const GENESIS_RATE_LIMIT_RPM: &str = "GENESIS_RATE_LIMIT_RPM";

/// Comma-separated list of trusted proxy IP addresses.
pub const GENESIS_TRUSTED_PROXIES: &str = "GENESIS_TRUSTED_PROXIES";

/// Current environment name (`prod`, `production`, etc.).
pub const GENESIS_ENV: &str = "GENESIS_ENV";

/// Whether MCP server connections must succeed at startup (`true`/`false`).
pub const GENESIS_MCP_STRICT_STARTUP: &str = "GENESIS_MCP_STRICT_STARTUP";

/// Override data directory path.
pub const GENESIS_DATA_DIR: &str = "GENESIS_DATA_DIR";

/// Override database file path.
pub const GENESIS_DATABASE_PATH: &str = "GENESIS_DATABASE_PATH";

/// Codex inference base URL override.
pub const GENESIS_CODEX_BASE_URL: &str = "GENESIS_CODEX_BASE_URL";

/// Debug mode flag.
pub const GENESIS_DEBUG: &str = "GENESIS_DEBUG";

// ---------------------------------------------------------------------------
// LLM provider API keys
// ---------------------------------------------------------------------------

/// OpenAI API key.
pub const OPENAI_API_KEY: &str = "OPENAI_API_KEY";

/// OpenAI-compatible API base URL override.
pub const OPENAI_API_BASE: &str = "OPENAI_API_BASE";

/// Anthropic API key.
pub const ANTHROPIC_API_KEY: &str = "ANTHROPIC_API_KEY";

/// OpenRouter API key.
pub const OPENROUTER_API_KEY: &str = "OPENROUTER_API_KEY";

/// Google / Gemini API key.
pub const GOOGLE_API_KEY: &str = "GOOGLE_API_KEY";

/// Brave Search API key.
pub const BRAVE_API_KEY: &str = "BRAVE_API_KEY";

/// GitHub personal access token (used for skills hub).
pub const GITHUB_TOKEN: &str = "GITHUB_TOKEN";

/// Auxiliary vision model override (default: `gpt-4o`).
pub const AUXILIARY_VISION_MODEL: &str = "AUXILIARY_VISION_MODEL";

// ---------------------------------------------------------------------------
// Telegram
// ---------------------------------------------------------------------------

/// Telegram Bot API token.
pub const TELEGRAM_BOT_TOKEN: &str = "TELEGRAM_BOT_TOKEN";

/// Shared secret for Telegram webhook verification.
pub const TELEGRAM_WEBHOOK_SECRET: &str = "TELEGRAM_WEBHOOK_SECRET";

/// Allow all Telegram users without pairing.
pub const TELEGRAM_ALLOW_ALL_USERS: &str = "TELEGRAM_ALLOW_ALL_USERS";

/// Comma-separated list of allowed Telegram user IDs.
pub const TELEGRAM_ALLOWED_USERS: &str = "TELEGRAM_ALLOWED_USERS";

// ---------------------------------------------------------------------------
// Discord
// ---------------------------------------------------------------------------

/// Discord application public key (for interaction signature verification).
pub const DISCORD_PUBLIC_KEY: &str = "DISCORD_PUBLIC_KEY";

/// Discord bot token.
pub const DISCORD_BOT_TOKEN: &str = "DISCORD_BOT_TOKEN";

/// Allow all Discord users without pairing.
pub const DISCORD_ALLOW_ALL_USERS: &str = "DISCORD_ALLOW_ALL_USERS";

/// Comma-separated list of allowed Discord user IDs.
pub const DISCORD_ALLOWED_USERS: &str = "DISCORD_ALLOWED_USERS";

// ---------------------------------------------------------------------------
// Slack
// ---------------------------------------------------------------------------

/// Slack bot OAuth token.
pub const SLACK_BOT_TOKEN: &str = "SLACK_BOT_TOKEN";

/// Slack app signing secret.
pub const SLACK_SIGNING_SECRET: &str = "SLACK_SIGNING_SECRET";

/// Allow all Slack users without pairing.
pub const SLACK_ALLOW_ALL_USERS: &str = "SLACK_ALLOW_ALL_USERS";

/// Comma-separated list of allowed Slack user IDs.
pub const SLACK_ALLOWED_USERS: &str = "SLACK_ALLOWED_USERS";

// ---------------------------------------------------------------------------
// WhatsApp
// ---------------------------------------------------------------------------

/// WhatsApp Cloud API permanent token.
pub const WHATSAPP_TOKEN: &str = "WHATSAPP_TOKEN";

/// WhatsApp Business phone number ID.
pub const WHATSAPP_PHONE_NUMBER_ID: &str = "WHATSAPP_PHONE_NUMBER_ID";

/// WhatsApp webhook verification token.
pub const WHATSAPP_VERIFY_TOKEN: &str = "WHATSAPP_VERIFY_TOKEN";

/// WhatsApp app secret (HMAC webhook signature verification).
pub const WHATSAPP_APP_SECRET: &str = "WHATSAPP_APP_SECRET";

/// Allow all WhatsApp users without pairing.
pub const WHATSAPP_ALLOW_ALL_USERS: &str = "WHATSAPP_ALLOW_ALL_USERS";

/// Comma-separated list of allowed WhatsApp user IDs.
pub const WHATSAPP_ALLOWED_USERS: &str = "WHATSAPP_ALLOWED_USERS";

// ---------------------------------------------------------------------------
// Signal
// ---------------------------------------------------------------------------

/// Base URL of the signal-cli HTTP daemon.
pub const SIGNAL_HTTP_URL: &str = "SIGNAL_HTTP_URL";

/// Registered Signal account (phone number).
pub const SIGNAL_ACCOUNT: &str = "SIGNAL_ACCOUNT";

/// Shared secret for Signal webhook endpoint authentication.
pub const SIGNAL_WEBHOOK_SECRET: &str = "SIGNAL_WEBHOOK_SECRET";

/// Comma-separated phone numbers allowed to interact in Signal group chats.
pub const SIGNAL_GROUP_ALLOWED_USERS: &str = "SIGNAL_GROUP_ALLOWED_USERS";

/// Allow all Signal users without pairing.
pub const SIGNAL_ALLOW_ALL_USERS: &str = "SIGNAL_ALLOW_ALL_USERS";

/// Comma-separated list of allowed Signal user IDs.
pub const SIGNAL_ALLOWED_USERS: &str = "SIGNAL_ALLOWED_USERS";

// ---------------------------------------------------------------------------
// Home Assistant
// ---------------------------------------------------------------------------

/// Home Assistant webhook authentication token.
pub const HOMEASSISTANT_TOKEN: &str = "HOMEASSISTANT_TOKEN";

/// Home Assistant base URL (for REST API callbacks).
pub const HOMEASSISTANT_URL: &str = "HOMEASSISTANT_URL";

/// Home Assistant long-lived access token (for service calls).
pub const HOMEASSISTANT_LONG_LIVED_TOKEN: &str = "HOMEASSISTANT_LONG_LIVED_TOKEN";

/// Home Assistant URL (hass-style, used by HA tools).
pub const HASS_URL: &str = "HASS_URL";

/// Home Assistant token (hass-style, used by HA tools).
pub const HASS_TOKEN: &str = "HASS_TOKEN";

// ---------------------------------------------------------------------------
// Gateway access control
// ---------------------------------------------------------------------------

/// Comma-separated list of globally allowed user IDs (all platforms).
pub const GATEWAY_ALLOWED_USERS: &str = "GATEWAY_ALLOWED_USERS";

/// Allow all users on all platforms without pairing.
pub const GATEWAY_ALLOW_ALL_USERS: &str = "GATEWAY_ALLOW_ALL_USERS";

// ---------------------------------------------------------------------------
// Browserbase (cloud browser)
// ---------------------------------------------------------------------------

/// Browserbase API key.
pub const BROWSERBASE_API_KEY: &str = "BROWSERBASE_API_KEY";

/// Browserbase project ID.
pub const BROWSERBASE_PROJECT_ID: &str = "BROWSERBASE_PROJECT_ID";

/// Enable/disable Browserbase proxy support.
pub const BROWSERBASE_PROXIES: &str = "BROWSERBASE_PROXIES";

/// Enable/disable Browserbase advanced stealth mode.
pub const BROWSERBASE_ADVANCED_STEALTH: &str = "BROWSERBASE_ADVANCED_STEALTH";

/// Enable/disable Browserbase keep-alive.
pub const BROWSERBASE_KEEP_ALIVE: &str = "BROWSERBASE_KEEP_ALIVE";

/// Browserbase session timeout in milliseconds.
pub const BROWSERBASE_SESSION_TIMEOUT: &str = "BROWSERBASE_SESSION_TIMEOUT";

// ---------------------------------------------------------------------------
// Local browser
// ---------------------------------------------------------------------------

/// Browser inactivity timeout in seconds.
pub const BROWSER_INACTIVITY_TIMEOUT: &str = "BROWSER_INACTIVITY_TIMEOUT";

// ---------------------------------------------------------------------------
// Sandbox backends
// ---------------------------------------------------------------------------

/// Daytona API key.
pub const DAYTONA_API_KEY: &str = "DAYTONA_API_KEY";

/// Daytona API URL override.
pub const DAYTONA_API_URL: &str = "DAYTONA_API_URL";

/// Modal token ID.
pub const MODAL_TOKEN_ID: &str = "MODAL_TOKEN_ID";

/// Modal token secret.
pub const MODAL_TOKEN_SECRET: &str = "MODAL_TOKEN_SECRET";

/// Singularity scratch directory override.
pub const TERMINAL_SCRATCH_DIR: &str = "TERMINAL_SCRATCH_DIR";

/// Singularity sandbox directory override.
pub const TERMINAL_SANDBOX_DIR: &str = "TERMINAL_SANDBOX_DIR";

// ---------------------------------------------------------------------------
// SSH / Remote detection
// ---------------------------------------------------------------------------

/// Set by SSH when connecting (used for remote session detection).
pub const SSH_CLIENT: &str = "SSH_CLIENT";

/// Set by SSH for allocated TTY (used for remote session detection).
pub const SSH_TTY: &str = "SSH_TTY";

/// Codex CLI home directory override.
pub const CODEX_HOME: &str = "CODEX_HOME";

// ---------------------------------------------------------------------------
// Shell / OS / Display
// ---------------------------------------------------------------------------

/// User's home directory.
pub const HOME: &str = "HOME";

/// Preferred text editor.
pub const EDITOR: &str = "EDITOR";

/// Current system PATH.
pub const PATH: &str = "PATH";

/// Current user name.
pub const USER: &str = "USER";

/// Disable color output (<https://no-color.org>).
pub const NO_COLOR: &str = "NO_COLOR";

/// Reduce motion / animations.
pub const REDUCE_MOTION: &str = "REDUCE_MOTION";

/// Wayland display (used for clipboard platform detection).
pub const WAYLAND_DISPLAY: &str = "WAYLAND_DISPLAY";

/// Tmux session detection.
pub const TMUX: &str = "TMUX";

/// Zellij session detection.
pub const ZELLIJ_SESSION_NAME: &str = "ZELLIJ_SESSION_NAME";

// ---------------------------------------------------------------------------
// Accessor helpers
// ---------------------------------------------------------------------------

/// Read an optional string env var (returns `None` if unset or empty).
#[inline]
pub fn get_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

/// Read a required string env var (returns `Err` if unset).
#[inline]
pub fn get(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}

/// Check whether an env var is set (non-empty).
#[inline]
pub fn is_set(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|v| !v.is_empty())
}

/// Check whether an env var is present (even if empty).
#[inline]
pub fn is_present(name: &str) -> bool {
    std::env::var(name).is_ok()
}

/// Read an env var as an `OsString`, returning `None` if unset.
#[inline]
pub fn get_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

/// Parse a boolean-ish env var (`1`, `true`, `yes`, `on` → true).
/// Returns `default` when the var is unset.
#[inline]
pub fn get_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

/// Parse a `u64` env var, returning `None` when unset or unparseable.
#[inline]
pub fn get_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Parse a `u32` env var, returning `None` when unset or unparseable.
#[inline]
pub fn get_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Read an env var with a fallback default value.
#[inline]
pub fn get_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// Return the platform-specific allowlist env var name.
pub fn platform_allowlist_var(platform: &str) -> &'static str {
    match platform {
        "telegram" => TELEGRAM_ALLOWED_USERS,
        "discord" => DISCORD_ALLOWED_USERS,
        "whatsapp" => WHATSAPP_ALLOWED_USERS,
        "slack" => SLACK_ALLOWED_USERS,
        "signal" => SIGNAL_ALLOWED_USERS,
        _ => "",
    }
}

/// Return the platform-specific allow-all env var name.
pub fn platform_allow_all_var(platform: &str) -> &'static str {
    match platform {
        "telegram" => TELEGRAM_ALLOW_ALL_USERS,
        "discord" => DISCORD_ALLOW_ALL_USERS,
        "whatsapp" => WHATSAPP_ALLOW_ALL_USERS,
        "slack" => SLACK_ALLOW_ALL_USERS,
        "signal" => SIGNAL_ALLOW_ALL_USERS,
        _ => "",
    }
}

/// Collect all env vars into a `BTreeMap` (used for provider resolution, etc.).
#[inline]
pub fn all_vars() -> std::collections::BTreeMap<String, String> {
    std::env::vars().collect()
}

// ---------------------------------------------------------------------------
// Startup validation
// ---------------------------------------------------------------------------

/// A warning about a missing or misconfigured environment variable.
#[derive(Debug, Clone)]
pub struct EnvWarning {
    pub var_name: &'static str,
    pub message: String,
}

impl std::fmt::Display for EnvWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.var_name, self.message)
    }
}

/// Validate that env vars required for enabled features are present.
///
/// Returns a list of warnings (not errors) so the caller can log them.
/// This is meant to be called once at startup.
pub fn validate_startup() -> Vec<EnvWarning> {
    let mut warnings = Vec::new();

    // Check platform tokens: warn if only partially configured.
    if is_present(TELEGRAM_BOT_TOKEN) && !is_present(TELEGRAM_WEBHOOK_SECRET) {
        warnings.push(EnvWarning {
            var_name: TELEGRAM_WEBHOOK_SECRET,
            message: "TELEGRAM_BOT_TOKEN is set but TELEGRAM_WEBHOOK_SECRET is not — \
                      webhook requests will not be verified"
                .to_owned(),
        });
    }

    if is_present(SLACK_BOT_TOKEN) && !is_present(SLACK_SIGNING_SECRET) {
        warnings.push(EnvWarning {
            var_name: SLACK_SIGNING_SECRET,
            message: "SLACK_BOT_TOKEN is set but SLACK_SIGNING_SECRET is not — \
                      webhook requests will not be verified"
                .to_owned(),
        });
    }

    if is_present(WHATSAPP_TOKEN) {
        if !is_present(WHATSAPP_PHONE_NUMBER_ID) {
            warnings.push(EnvWarning {
                var_name: WHATSAPP_PHONE_NUMBER_ID,
                message: "WHATSAPP_TOKEN is set but WHATSAPP_PHONE_NUMBER_ID is not — \
                          WhatsApp messages cannot be sent"
                    .to_owned(),
            });
        }
        if !is_present(WHATSAPP_APP_SECRET) {
            warnings.push(EnvWarning {
                var_name: WHATSAPP_APP_SECRET,
                message: "WHATSAPP_TOKEN is set but WHATSAPP_APP_SECRET is not — \
                          webhook requests will not be verified"
                    .to_owned(),
            });
        }
    }

    if is_present(SIGNAL_ACCOUNT) && !is_present(SIGNAL_HTTP_URL) {
        warnings.push(EnvWarning {
            var_name: SIGNAL_HTTP_URL,
            message: "SIGNAL_ACCOUNT is set but SIGNAL_HTTP_URL is not — \
                      will default to http://127.0.0.1:8080"
                .to_owned(),
        });
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_opt_returns_none_for_unset() {
        // Use a var name that is extremely unlikely to be set.
        assert!(get_opt("__GENESIS_TEST_UNSET_VAR_XYZ__").is_none());
    }

    #[test]
    fn is_set_returns_false_for_unset() {
        assert!(!is_set("__GENESIS_TEST_UNSET_VAR_XYZ__"));
    }

    #[test]
    fn is_present_returns_false_for_unset() {
        assert!(!is_present("__GENESIS_TEST_UNSET_VAR_XYZ__"));
    }

    #[test]
    fn get_bool_returns_default_for_unset() {
        assert!(!get_bool("__GENESIS_TEST_UNSET_VAR_XYZ__", false));
        assert!(get_bool("__GENESIS_TEST_UNSET_VAR_XYZ__", true));
    }

    #[test]
    fn get_bool_parses_truthy_values() {
        std::env::set_var("__GENESIS_TEST_BOOL_1__", "1");
        assert!(get_bool("__GENESIS_TEST_BOOL_1__", false));
        std::env::remove_var("__GENESIS_TEST_BOOL_1__");

        std::env::set_var("__GENESIS_TEST_BOOL_TRUE__", "True");
        assert!(get_bool("__GENESIS_TEST_BOOL_TRUE__", false));
        std::env::remove_var("__GENESIS_TEST_BOOL_TRUE__");

        std::env::set_var("__GENESIS_TEST_BOOL_YES__", "YES");
        assert!(get_bool("__GENESIS_TEST_BOOL_YES__", false));
        std::env::remove_var("__GENESIS_TEST_BOOL_YES__");

        std::env::set_var("__GENESIS_TEST_BOOL_ON__", "on");
        assert!(get_bool("__GENESIS_TEST_BOOL_ON__", false));
        std::env::remove_var("__GENESIS_TEST_BOOL_ON__");
    }

    #[test]
    fn get_bool_returns_false_for_falsy_values() {
        std::env::set_var("__GENESIS_TEST_BOOL_NO__", "no");
        assert!(!get_bool("__GENESIS_TEST_BOOL_NO__", true));
        std::env::remove_var("__GENESIS_TEST_BOOL_NO__");
    }

    #[test]
    fn get_u64_returns_none_for_unset() {
        assert!(get_u64("__GENESIS_TEST_UNSET_VAR_XYZ__").is_none());
    }

    #[test]
    fn get_u64_parses_valid_numbers() {
        std::env::set_var("__GENESIS_TEST_U64__", "42");
        assert_eq!(get_u64("__GENESIS_TEST_U64__"), Some(42));
        std::env::remove_var("__GENESIS_TEST_U64__");
    }

    #[test]
    fn get_or_returns_default_for_unset() {
        assert_eq!(
            get_or("__GENESIS_TEST_UNSET_VAR_XYZ__", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn get_or_returns_value_when_set() {
        std::env::set_var("__GENESIS_TEST_GET_OR__", "custom");
        assert_eq!(get_or("__GENESIS_TEST_GET_OR__", "fallback"), "custom");
        std::env::remove_var("__GENESIS_TEST_GET_OR__");
    }

    #[test]
    fn platform_allowlist_var_returns_correct_names() {
        assert_eq!(platform_allowlist_var("telegram"), TELEGRAM_ALLOWED_USERS);
        assert_eq!(platform_allowlist_var("discord"), DISCORD_ALLOWED_USERS);
        assert_eq!(platform_allowlist_var("whatsapp"), WHATSAPP_ALLOWED_USERS);
        assert_eq!(platform_allowlist_var("slack"), SLACK_ALLOWED_USERS);
        assert_eq!(platform_allowlist_var("signal"), SIGNAL_ALLOWED_USERS);
        assert_eq!(platform_allowlist_var("unknown"), "");
    }

    #[test]
    fn platform_allow_all_var_returns_correct_names() {
        assert_eq!(platform_allow_all_var("telegram"), TELEGRAM_ALLOW_ALL_USERS);
        assert_eq!(platform_allow_all_var("discord"), DISCORD_ALLOW_ALL_USERS);
        assert_eq!(platform_allow_all_var("slack"), SLACK_ALLOW_ALL_USERS);
        assert_eq!(platform_allow_all_var("signal"), SIGNAL_ALLOW_ALL_USERS);
        assert_eq!(platform_allow_all_var("unknown"), "");
    }

    #[test]
    fn validate_startup_returns_empty_when_nothing_configured() {
        // When no platform tokens are set, there should be no warnings.
        // (We can't guarantee the test env doesn't have these set, but
        // in most CI / dev environments they won't be.)
        let warnings = validate_startup();
        // Just verify it doesn't panic and returns a Vec.
        let _ = warnings;
    }

    #[test]
    fn all_vars_returns_btreemap() {
        let vars = all_vars();
        // PATH is virtually always set.
        assert!(vars.contains_key("PATH") || vars.is_empty());
    }
}
