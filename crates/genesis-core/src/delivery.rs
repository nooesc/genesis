//! Multi-destination delivery router for scheduled outputs.
//!
//! When a scheduled job fires, the output may need to be delivered to multiple
//! destinations — the origin platform, a local file log, or explicit platform
//! channels. This module handles parsing target specifications, deduplicating
//! them, and dispatching messages with per-target error isolation.
//!
//! # Target formats
//!
//! ```text
//! "origin"           → deliver to the platform that created the schedule
//! "local"            → save to local file only
//! "telegram"         → deliver to Telegram's configured home channel
//! "telegram:123456"  → deliver to a specific Telegram chat
//! "discord"          → deliver to Discord's configured home channel
//! "discord:987654"   → deliver to a specific Discord channel
//! "slack"            → deliver to Slack's configured home channel
//! "whatsapp"         → deliver to WhatsApp's configured home channel
//! ```

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use tracing::{error, info, warn};

/// The maximum message length for platform delivery before truncation.
const PLATFORM_MAX_LENGTH: usize = 4000;

/// The truncated message length when the output exceeds [`PLATFORM_MAX_LENGTH`].
const PLATFORM_TRUNCATED_LENGTH: usize = 3800;

/// A parsed delivery target describing where schedule output should go.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeliveryTarget {
    /// Deliver to the platform that originally created the schedule.
    Origin,
    /// Save the output to a local file.
    Local,
    /// Deliver to a specific platform, optionally with an explicit chat/channel ID.
    Platform {
        /// Platform name (e.g., "telegram", "discord", "slack").
        platform: String,
        /// Optional explicit chat/channel ID. When `None`, the platform's
        /// configured home channel is used.
        chat_id: Option<String>,
    },
}

impl DeliveryTarget {
    /// Parse a delivery target from a string specification.
    ///
    /// See module-level docs for supported formats.
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim().to_ascii_lowercase();

        if trimmed == "origin" {
            return Self::Origin;
        }
        if trimmed == "local" {
            return Self::Local;
        }

        // Check for "platform:chat_id" syntax
        if let Some((platform, chat_id)) = trimmed.split_once(':') {
            let platform = platform.trim().to_owned();
            let chat_id = chat_id.trim().to_owned();
            if platform.is_empty() {
                return Self::Local;
            }
            if chat_id.is_empty() {
                return Self::Platform {
                    platform,
                    chat_id: None,
                };
            }
            return Self::Platform {
                platform,
                chat_id: Some(chat_id),
            };
        }

        // Plain platform name
        Self::Platform {
            platform: trimmed,
            chat_id: None,
        }
    }

    /// Parse a comma-separated list of delivery target specifications.
    pub fn parse_multi(input: &str) -> Vec<Self> {
        input
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(Self::parse)
            .collect()
    }

    /// Whether this target refers to the origin source.
    pub fn is_origin(&self) -> bool {
        matches!(self, Self::Origin)
    }

    /// Whether this target is a local-only delivery.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

impl fmt::Display for DeliveryTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Origin => write!(f, "origin"),
            Self::Local => write!(f, "local"),
            Self::Platform {
                platform,
                chat_id: Some(id),
            } => write!(f, "{platform}:{id}"),
            Self::Platform {
                platform,
                chat_id: None,
            } => write!(f, "{platform}"),
        }
    }
}

/// A concrete resolved destination with platform and chat information.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedDestination {
    /// The platform name (e.g., "telegram", "local").
    pub platform: String,
    /// The chat/channel ID, if applicable.
    pub chat_id: Option<String>,
}

impl fmt::Display for ResolvedDestination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.chat_id {
            Some(id) => write!(f, "{}:{}", self.platform, id),
            None => write!(f, "{}", self.platform),
        }
    }
}

/// Result of delivering output to a single destination.
#[derive(Debug, Clone)]
pub struct DeliveryResult {
    /// The destination that was targeted.
    pub destination: ResolvedDestination,
    /// Whether the delivery succeeded.
    pub success: bool,
    /// Error message if delivery failed.
    pub error: Option<String>,
    /// When a platform send fails, whether the local fallback save succeeded.
    /// `None` means no fallback was attempted (either the send succeeded or
    /// this was already a local delivery).
    pub fallback_local: Option<bool>,
}

/// Summary of a full delivery operation across all targets.
#[derive(Debug, Clone)]
pub struct DeliveryReport {
    /// Per-target delivery results.
    pub results: Vec<DeliveryResult>,
}

impl DeliveryReport {
    /// How many targets were delivered to successfully.
    pub fn success_count(&self) -> usize {
        self.results.iter().filter(|r| r.success).count()
    }

    /// How many targets failed.
    pub fn failure_count(&self) -> usize {
        self.results.iter().filter(|r| !r.success).count()
    }

    /// Whether all deliveries succeeded.
    pub fn all_succeeded(&self) -> bool {
        self.results.iter().all(|r| r.success)
    }
}

/// Configuration for the delivery router.
#[derive(Debug, Clone)]
pub struct DeliveryConfig {
    /// Home channel IDs for each platform, keyed by platform name.
    /// For example: `{"telegram": "123456", "discord": "987654"}`.
    pub home_channels: std::collections::HashMap<String, String>,
    /// Whether to always include local delivery regardless of targets.
    pub always_log_locally: bool,
    /// The origin platform of the schedule (e.g., "telegram").
    /// Used to resolve `DeliveryTarget::Origin`.
    pub origin_platform: Option<String>,
    /// The origin chat ID, used when the origin target is resolved.
    pub origin_chat_id: Option<String>,
    /// Base directory for local output files. Defaults to `~/.genesis/cron/output`.
    pub output_dir: PathBuf,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        let output_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".genesis")
            .join("cron")
            .join("output");
        Self {
            home_channels: std::collections::HashMap::new(),
            always_log_locally: false,
            origin_platform: None,
            origin_chat_id: None,
            output_dir,
        }
    }
}

/// Routes scheduled output to one or more delivery destinations.
pub struct DeliveryRouter {
    config: DeliveryConfig,
}

impl DeliveryRouter {
    /// Create a new delivery router with the given configuration.
    pub fn new(config: DeliveryConfig) -> Self {
        Self { config }
    }

    /// Resolve a list of delivery targets into concrete, deduplicated destinations.
    ///
    /// If the origin platform is unavailable (not configured), origin targets
    /// fall back to local delivery. Unknown platform names also fall back to local.
    pub fn resolve(&self, targets: &[DeliveryTarget]) -> Vec<ResolvedDestination> {
        let mut seen = HashSet::new();
        let mut destinations = Vec::new();

        for target in targets {
            let resolved = self.resolve_single(target);
            if seen.insert(resolved.clone()) {
                destinations.push(resolved);
            }
        }

        // Enforce local logging if configured
        if self.config.always_log_locally {
            let local = ResolvedDestination {
                platform: "local".to_owned(),
                chat_id: None,
            };
            if !seen.contains(&local) {
                destinations.push(local);
            }
        }

        destinations
    }

    /// Resolve a single delivery target to a concrete destination.
    fn resolve_single(&self, target: &DeliveryTarget) -> ResolvedDestination {
        match target {
            DeliveryTarget::Origin => {
                if let Some(ref platform) = self.config.origin_platform {
                    ResolvedDestination {
                        platform: platform.clone(),
                        chat_id: self.config.origin_chat_id.clone(),
                    }
                } else {
                    // Graceful fallback: origin unavailable → local
                    warn!("origin platform not configured, falling back to local");
                    ResolvedDestination {
                        platform: "local".to_owned(),
                        chat_id: None,
                    }
                }
            }
            DeliveryTarget::Local => ResolvedDestination {
                platform: "local".to_owned(),
                chat_id: None,
            },
            DeliveryTarget::Platform { platform, chat_id } => {
                if !is_known_platform(platform) {
                    warn!(
                        platform = platform.as_str(),
                        "unknown platform, falling back to local"
                    );
                    return ResolvedDestination {
                        platform: "local".to_owned(),
                        chat_id: None,
                    };
                }

                let resolved_chat_id = chat_id
                    .clone()
                    .or_else(|| self.config.home_channels.get(platform).cloned());

                ResolvedDestination {
                    platform: platform.clone(),
                    chat_id: resolved_chat_id,
                }
            }
        }
    }

    /// Dispatch a scheduled output to all resolved destinations.
    ///
    /// Each destination is delivered independently with per-target error
    /// isolation — a failure on one target does not prevent delivery to others.
    pub async fn dispatch(
        &self,
        job_id: &str,
        output: &str,
        destinations: &[ResolvedDestination],
        sender: &dyn PlatformSender,
    ) -> DeliveryReport {
        let mut results = Vec::with_capacity(destinations.len());

        for dest in destinations {
            let result = if dest.platform == "local" {
                self.deliver_local(job_id, output).await
            } else {
                self.deliver_platform(job_id, output, dest, sender).await
            };
            results.push(result);
        }

        DeliveryReport { results }
    }

    /// Save output to a local file with metadata headers.
    async fn deliver_local(&self, job_id: &str, output: &str) -> DeliveryResult {
        let dest = ResolvedDestination {
            platform: "local".to_owned(),
            chat_id: None,
        };

        match save_local_output(&self.config.output_dir, job_id, output) {
            Ok(path) => {
                info!(
                    job_id = job_id,
                    path = %path.display(),
                    "schedule output saved locally"
                );
                DeliveryResult {
                    destination: dest,
                    success: true,
                    error: None,
                    fallback_local: None,
                }
            }
            Err(e) => {
                error!(
                    job_id = job_id,
                    error = %e,
                    "failed to save schedule output locally"
                );
                DeliveryResult {
                    destination: dest,
                    success: false,
                    error: Some(e.to_string()),
                    fallback_local: None,
                }
            }
        }
    }

    /// Deliver output to a platform, with truncation and local backup if needed.
    async fn deliver_platform(
        &self,
        job_id: &str,
        output: &str,
        dest: &ResolvedDestination,
        sender: &dyn PlatformSender,
    ) -> DeliveryResult {
        let message = if output.len() > PLATFORM_MAX_LENGTH {
            // Save the full output locally before truncating
            if let Err(e) = save_local_output(&self.config.output_dir, job_id, output) {
                warn!(
                    job_id = job_id,
                    error = %e,
                    "failed to save full output locally before truncation"
                );
            }
            truncate_message(output, PLATFORM_TRUNCATED_LENGTH)
        } else {
            output.to_owned()
        };

        match sender
            .send(&dest.platform, dest.chat_id.as_deref(), &message)
            .await
        {
            Ok(()) => {
                info!(
                    job_id = job_id,
                    destination = %dest,
                    "schedule output delivered to platform"
                );
                DeliveryResult {
                    destination: dest.clone(),
                    success: true,
                    error: None,
                    fallback_local: None,
                }
            }
            Err(e) => {
                error!(
                    job_id = job_id,
                    destination = %dest,
                    error = %e,
                    "failed to deliver schedule output to platform"
                );

                // Graceful fallback: try saving locally
                let fallback_ok =
                    match save_local_output(&self.config.output_dir, job_id, output) {
                        Ok(path) => {
                            info!(
                                job_id = job_id,
                                path = %path.display(),
                                "fallback local save succeeded"
                            );
                            true
                        }
                        Err(local_err) => {
                            error!(
                                job_id = job_id,
                                error = %local_err,
                                "fallback local save also failed"
                            );
                            false
                        }
                    };

                DeliveryResult {
                    destination: dest.clone(),
                    success: false,
                    error: Some(e),
                    fallback_local: Some(fallback_ok),
                }
            }
        }
    }
}

/// Trait for sending messages to platform channels.
///
/// This is implemented by the gateway layer and injected into the router
/// so that `genesis-core` does not depend on gateway-specific HTTP clients.
///
/// Uses manual future desugaring rather than `async_trait` to avoid the
/// external dependency.
pub trait PlatformSender: Send + Sync {
    /// Send a message to the specified platform and chat/channel.
    ///
    /// `chat_id` may be `None` if the platform has a single default destination.
    fn send<'a>(
        &'a self,
        platform: &'a str,
        chat_id: Option<&'a str>,
        message: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>;
}

/// Check whether a platform name is a known/supported platform.
fn is_known_platform(name: &str) -> bool {
    matches!(
        name,
        "telegram" | "discord" | "slack" | "whatsapp" | "homeassistant"
    )
}

/// Sanitize a job ID for safe use as a filesystem path component.
///
/// Strips path separators (`/`, `\`) and `..` components to prevent path
/// traversal attacks. If the sanitized result is empty, falls back to
/// `"_unnamed_"`.
fn sanitize_job_id(job_id: &str) -> String {
    let sanitized: String = job_id
        .replace(['/', '\\'], "_")
        .replace("..", "_");
    let sanitized = sanitized.trim_matches(|c: char| c == '_' || c == '.' || c.is_whitespace());
    if sanitized.is_empty() {
        "_unnamed_".to_owned()
    } else {
        sanitized.to_owned()
    }
}

/// Save schedule output to a local file with metadata headers.
///
/// Files are saved to `{output_dir}/{job_id}/{timestamp}.md` with a YAML
/// front-matter-style header containing the job ID and timestamp.
///
/// The `job_id` is sanitized to prevent path traversal.
fn save_local_output(output_dir: &Path, job_id: &str, content: &str) -> std::io::Result<PathBuf> {
    let safe_id = sanitize_job_id(job_id);
    let job_dir = output_dir.join(&safe_id);
    std::fs::create_dir_all(&job_dir)?;

    let now = chrono::Local::now();
    let timestamp = now.format("%Y%m%dT%H%M%S").to_string();
    let filename = format!("{timestamp}.md");
    let file_path = job_dir.join(&filename);

    let iso_timestamp = now.to_rfc3339();
    let full_content = format!(
        "---\njob_id: {job_id}\ntimestamp: {iso_timestamp}\n---\n\n{content}\n"
    );

    std::fs::write(&file_path, full_content)?;
    Ok(file_path)
}

/// Truncate a message to `max_len`, appending a truncation notice.
///
/// Tries to break at the last newline before the limit for cleaner output.
/// Uses `str::floor_char_boundary` to avoid panicking on multi-byte UTF-8
/// codepoints.
fn truncate_message(text: &str, max_len: usize) -> String {
    let suffix = "\n\n[Output truncated. Full output saved locally.]";
    let available = max_len.saturating_sub(suffix.len());

    if available == 0 {
        return suffix.to_owned();
    }

    // Clamp to a valid char boundary so we never slice inside a multi-byte
    // codepoint.
    let safe_end = text.floor_char_boundary(available.min(text.len()));

    // Find a clean break point (last newline within available range)
    let break_point = text[..safe_end].rfind('\n').unwrap_or(safe_end);

    let mut result = text[..break_point].to_owned();
    result.push_str(suffix);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ─── DeliveryTarget parsing ─────────────────────────────────────────

    #[test]
    fn parse_origin_target() {
        assert_eq!(DeliveryTarget::parse("origin"), DeliveryTarget::Origin);
        assert_eq!(DeliveryTarget::parse("ORIGIN"), DeliveryTarget::Origin);
        assert_eq!(DeliveryTarget::parse("  Origin  "), DeliveryTarget::Origin);
    }

    #[test]
    fn parse_local_target() {
        assert_eq!(DeliveryTarget::parse("local"), DeliveryTarget::Local);
        assert_eq!(DeliveryTarget::parse("LOCAL"), DeliveryTarget::Local);
    }

    #[test]
    fn parse_platform_without_chat_id() {
        assert_eq!(
            DeliveryTarget::parse("telegram"),
            DeliveryTarget::Platform {
                platform: "telegram".to_owned(),
                chat_id: None,
            }
        );
    }

    #[test]
    fn parse_platform_with_chat_id() {
        assert_eq!(
            DeliveryTarget::parse("telegram:123456"),
            DeliveryTarget::Platform {
                platform: "telegram".to_owned(),
                chat_id: Some("123456".to_owned()),
            }
        );
    }

    #[test]
    fn parse_platform_with_colon_but_empty_chat_id() {
        assert_eq!(
            DeliveryTarget::parse("discord:"),
            DeliveryTarget::Platform {
                platform: "discord".to_owned(),
                chat_id: None,
            }
        );
    }

    #[test]
    fn parse_empty_platform_falls_back_to_local() {
        assert_eq!(DeliveryTarget::parse(":123"), DeliveryTarget::Local);
    }

    #[test]
    fn parse_multi_targets() {
        let targets = DeliveryTarget::parse_multi("origin, telegram:123, local");
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0], DeliveryTarget::Origin);
        assert_eq!(
            targets[1],
            DeliveryTarget::Platform {
                platform: "telegram".to_owned(),
                chat_id: Some("123".to_owned()),
            }
        );
        assert_eq!(targets[2], DeliveryTarget::Local);
    }

    #[test]
    fn parse_multi_empty_string() {
        let targets = DeliveryTarget::parse_multi("");
        assert!(targets.is_empty());
    }

    #[test]
    fn parse_multi_skips_empty_segments() {
        let targets = DeliveryTarget::parse_multi("origin,,local,");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0], DeliveryTarget::Origin);
        assert_eq!(targets[1], DeliveryTarget::Local);
    }

    #[test]
    fn target_is_origin() {
        assert!(DeliveryTarget::Origin.is_origin());
        assert!(!DeliveryTarget::Local.is_origin());
    }

    #[test]
    fn target_is_local() {
        assert!(DeliveryTarget::Local.is_local());
        assert!(!DeliveryTarget::Origin.is_local());
    }

    #[test]
    fn target_display() {
        assert_eq!(DeliveryTarget::Origin.to_string(), "origin");
        assert_eq!(DeliveryTarget::Local.to_string(), "local");
        assert_eq!(
            DeliveryTarget::Platform {
                platform: "telegram".to_owned(),
                chat_id: Some("42".to_owned()),
            }
            .to_string(),
            "telegram:42"
        );
        assert_eq!(
            DeliveryTarget::Platform {
                platform: "slack".to_owned(),
                chat_id: None,
            }
            .to_string(),
            "slack"
        );
    }

    // ─── DeliveryRouter resolution ──────────────────────────────────────

    #[test]
    fn resolve_origin_with_configured_platform() {
        let config = DeliveryConfig {
            origin_platform: Some("telegram".to_owned()),
            origin_chat_id: Some("99".to_owned()),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Origin]);
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].platform, "telegram");
        assert_eq!(dests[0].chat_id.as_deref(), Some("99"));
    }

    #[test]
    fn resolve_origin_without_configured_platform_falls_back_to_local() {
        let config = DeliveryConfig {
            origin_platform: None,
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Origin]);
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].platform, "local");
    }

    #[test]
    fn resolve_platform_with_home_channel() {
        let mut home_channels = std::collections::HashMap::new();
        home_channels.insert("discord".to_owned(), "general-123".to_owned());

        let config = DeliveryConfig {
            home_channels,
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Platform {
            platform: "discord".to_owned(),
            chat_id: None,
        }]);
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].platform, "discord");
        assert_eq!(dests[0].chat_id.as_deref(), Some("general-123"));
    }

    #[test]
    fn resolve_platform_with_explicit_chat_id_overrides_home_channel() {
        let mut home_channels = std::collections::HashMap::new();
        home_channels.insert("telegram".to_owned(), "default-chat".to_owned());

        let config = DeliveryConfig {
            home_channels,
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Platform {
            platform: "telegram".to_owned(),
            chat_id: Some("override-chat".to_owned()),
        }]);
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].chat_id.as_deref(), Some("override-chat"));
    }

    #[test]
    fn resolve_unknown_platform_falls_back_to_local() {
        let config = DeliveryConfig::default();
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Platform {
            platform: "carrier_pigeon".to_owned(),
            chat_id: None,
        }]);
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].platform, "local");
    }

    #[test]
    fn resolve_deduplicates_destinations() {
        let config = DeliveryConfig {
            origin_platform: Some("telegram".to_owned()),
            origin_chat_id: Some("42".to_owned()),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let targets = vec![
            DeliveryTarget::Origin,
            DeliveryTarget::Platform {
                platform: "telegram".to_owned(),
                chat_id: Some("42".to_owned()),
            },
        ];
        let dests = router.resolve(&targets);
        assert_eq!(dests.len(), 1, "duplicate telegram:42 should be deduplicated");
    }

    #[test]
    fn resolve_enforces_local_when_always_log_locally() {
        let config = DeliveryConfig {
            always_log_locally: true,
            origin_platform: Some("telegram".to_owned()),
            origin_chat_id: Some("42".to_owned()),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Origin]);
        assert_eq!(dests.len(), 2);
        assert!(
            dests.iter().any(|d| d.platform == "local"),
            "local should be included when always_log_locally is true"
        );
    }

    #[test]
    fn resolve_always_log_locally_does_not_duplicate_existing_local() {
        let config = DeliveryConfig {
            always_log_locally: true,
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);

        let dests = router.resolve(&[DeliveryTarget::Local]);
        let local_count = dests.iter().filter(|d| d.platform == "local").count();
        assert_eq!(local_count, 1, "local should not be duplicated");
    }

    // ─── Local file output ──────────────────────────────────────────────

    #[test]
    fn save_local_output_creates_file_with_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = save_local_output(dir.path(), "job-1", "Hello, world!")
            .expect("should save");

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).expect("should read");
        assert!(content.starts_with("---\n"));
        assert!(content.contains("job_id: job-1"));
        assert!(content.contains("timestamp:"));
        assert!(content.contains("Hello, world!"));
    }

    #[test]
    fn save_local_output_creates_job_subdirectory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _ = save_local_output(dir.path(), "my-job", "content").expect("should save");
        assert!(dir.path().join("my-job").is_dir());
    }

    // ─── Message truncation ─────────────────────────────────────────────

    #[test]
    fn truncate_short_message_is_noop() {
        let msg = "short message";
        // truncate_message is only called when len > PLATFORM_MAX_LENGTH,
        // but we can test the function directly
        let truncated = truncate_message(msg, 500);
        // Should include the full text plus the suffix
        assert!(truncated.contains("short message"));
    }

    #[test]
    fn truncate_long_message_respects_limit() {
        let long = "a".repeat(5000);
        let truncated = truncate_message(&long, PLATFORM_TRUNCATED_LENGTH);
        assert!(
            truncated.len() <= PLATFORM_TRUNCATED_LENGTH,
            "truncated len {} should be <= {}",
            truncated.len(),
            PLATFORM_TRUNCATED_LENGTH,
        );
        assert!(truncated.contains("[Output truncated. Full output saved locally.]"));
    }

    #[test]
    fn truncate_prefers_newline_break() {
        let mut msg = String::new();
        for i in 0..100 {
            msg.push_str(&format!("line {i}\n"));
        }
        let truncated = truncate_message(&msg, 200);
        // Should end with our suffix
        assert!(truncated.ends_with("[Output truncated. Full output saved locally.]"));
        // The truncated message should be within the limit
        assert!(truncated.len() <= 200);
        // Verify it doesn't cut in the middle of a word — the content before
        // the suffix should end right before a \n boundary (since rfind finds
        // the last newline and slices up to it).
        let text_part = truncated
            .strip_suffix("\n\n[Output truncated. Full output saved locally.]")
            .expect("should have suffix");
        assert!(
            !text_part.is_empty(),
            "should preserve some content"
        );
        // Should contain complete lines
        assert!(text_part.contains("line 0"));
    }

    // ─── Resolved destination display ───────────────────────────────────

    #[test]
    fn resolved_destination_display() {
        let d = ResolvedDestination {
            platform: "telegram".to_owned(),
            chat_id: Some("42".to_owned()),
        };
        assert_eq!(d.to_string(), "telegram:42");

        let d2 = ResolvedDestination {
            platform: "local".to_owned(),
            chat_id: None,
        };
        assert_eq!(d2.to_string(), "local");
    }

    // ─── DeliveryReport ─────────────────────────────────────────────────

    #[test]
    fn delivery_report_counts() {
        let report = DeliveryReport {
            results: vec![
                DeliveryResult {
                    destination: ResolvedDestination {
                        platform: "telegram".to_owned(),
                        chat_id: None,
                    },
                    success: true,
                    error: None,
                    fallback_local: None,
                },
                DeliveryResult {
                    destination: ResolvedDestination {
                        platform: "discord".to_owned(),
                        chat_id: None,
                    },
                    success: false,
                    error: Some("connection refused".to_owned()),
                    fallback_local: Some(true),
                },
                DeliveryResult {
                    destination: ResolvedDestination {
                        platform: "local".to_owned(),
                        chat_id: None,
                    },
                    success: true,
                    error: None,
                    fallback_local: None,
                },
            ],
        };

        assert_eq!(report.success_count(), 2);
        assert_eq!(report.failure_count(), 1);
        assert!(!report.all_succeeded());
    }

    #[test]
    fn delivery_report_all_succeeded() {
        let report = DeliveryReport {
            results: vec![DeliveryResult {
                destination: ResolvedDestination {
                    platform: "local".to_owned(),
                    chat_id: None,
                },
                success: true,
                error: None,
                fallback_local: None,
            }],
        };
        assert!(report.all_succeeded());
    }

    // ─── Dispatch integration tests ─────────────────────────────────────

    /// A mock platform sender that records calls.
    struct MockSender {
        calls: Mutex<Vec<(String, Option<String>, String)>>,
        fail_platforms: Vec<String>,
    }

    impl MockSender {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_platforms: Vec::new(),
            }
        }

        fn with_failures(platforms: Vec<String>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_platforms: platforms,
            }
        }
    }

    impl PlatformSender for MockSender {
        fn send<'a>(
            &'a self,
            platform: &'a str,
            chat_id: Option<&'a str>,
            message: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            let platform_owned = platform.to_owned();
            let chat_id_owned = chat_id.map(ToOwned::to_owned);
            let message_owned = message.to_owned();
            let should_fail = self.fail_platforms.contains(&platform_owned);
            self.calls
                .lock()
                .expect("lock")
                .push((platform_owned.clone(), chat_id_owned, message_owned));
            Box::pin(async move {
                if should_fail {
                    Err(format!("{platform_owned} is down"))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn dispatch_local_saves_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DeliveryConfig {
            output_dir: dir.path().to_path_buf(),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::new();

        let dests = vec![ResolvedDestination {
            platform: "local".to_owned(),
            chat_id: None,
        }];

        let report = router.dispatch("job-1", "test output", &dests, &sender).await;
        assert_eq!(report.success_count(), 1);
        assert!(dir.path().join("job-1").is_dir());
    }

    #[tokio::test]
    async fn dispatch_platform_sends_via_sender() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DeliveryConfig {
            output_dir: dir.path().to_path_buf(),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::new();

        let dests = vec![ResolvedDestination {
            platform: "telegram".to_owned(),
            chat_id: Some("42".to_owned()),
        }];

        let report = router
            .dispatch("job-2", "hello telegram", &dests, &sender)
            .await;
        assert_eq!(report.success_count(), 1);

        let calls = sender.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "telegram");
        assert_eq!(calls[0].1.as_deref(), Some("42"));
        assert_eq!(calls[0].2, "hello telegram");
    }

    #[tokio::test]
    async fn dispatch_truncates_long_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DeliveryConfig {
            output_dir: dir.path().to_path_buf(),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::new();

        let long_output = "x".repeat(5000);
        let dests = vec![ResolvedDestination {
            platform: "telegram".to_owned(),
            chat_id: Some("1".to_owned()),
        }];

        let report = router
            .dispatch("job-3", &long_output, &dests, &sender)
            .await;
        assert_eq!(report.success_count(), 1);

        let calls = sender.calls.lock().expect("lock");
        assert!(
            calls[0].2.len() <= PLATFORM_TRUNCATED_LENGTH,
            "message should be truncated to {}",
            PLATFORM_TRUNCATED_LENGTH
        );
        assert!(calls[0].2.contains("[Output truncated"));

        // Full output should also be saved locally
        assert!(dir.path().join("job-3").is_dir());
    }

    #[tokio::test]
    async fn dispatch_platform_failure_falls_back_to_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DeliveryConfig {
            output_dir: dir.path().to_path_buf(),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::with_failures(vec!["telegram".to_owned()]);

        let dests = vec![ResolvedDestination {
            platform: "telegram".to_owned(),
            chat_id: Some("1".to_owned()),
        }];

        let report = router
            .dispatch("job-fail", "important output", &dests, &sender)
            .await;
        assert_eq!(report.failure_count(), 1);
        assert!(report.results[0].error.is_some());
        // Fallback should have been attempted and succeeded
        assert_eq!(report.results[0].fallback_local, Some(true));

        // Fallback local save should have happened
        assert!(dir.path().join("job-fail").is_dir());
    }

    #[tokio::test]
    async fn dispatch_multiple_destinations_with_mixed_results() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DeliveryConfig {
            output_dir: dir.path().to_path_buf(),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::with_failures(vec!["discord".to_owned()]);

        let dests = vec![
            ResolvedDestination {
                platform: "telegram".to_owned(),
                chat_id: Some("1".to_owned()),
            },
            ResolvedDestination {
                platform: "discord".to_owned(),
                chat_id: Some("2".to_owned()),
            },
            ResolvedDestination {
                platform: "local".to_owned(),
                chat_id: None,
            },
        ];

        let report = router
            .dispatch("job-multi", "output", &dests, &sender)
            .await;
        assert_eq!(report.success_count(), 2); // telegram + local
        assert_eq!(report.failure_count(), 1); // discord
        assert!(!report.all_succeeded());
    }

    // ─── Platform failure fallback_local field ──────────────────────────

    #[tokio::test]
    async fn dispatch_platform_success_has_no_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DeliveryConfig {
            output_dir: dir.path().to_path_buf(),
            ..DeliveryConfig::default()
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::new();

        let dests = vec![ResolvedDestination {
            platform: "telegram".to_owned(),
            chat_id: Some("1".to_owned()),
        }];

        let report = router
            .dispatch("ok-job", "some output", &dests, &sender)
            .await;
        assert_eq!(report.success_count(), 1);
        // No fallback was attempted on success
        assert_eq!(report.results[0].fallback_local, None);
    }

    // ─── UTF-8 safe truncation ─────────────────────────────────────────

    #[test]
    fn truncate_message_handles_multibyte_utf8() {
        // 'é' is 2 bytes, '你' is 3 bytes, '🎉' is 4 bytes
        let text = "aé你🎉bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        // Try truncating at various points that could land inside multi-byte chars
        for limit in 1..text.len() + 10 {
            // This must not panic
            let result = truncate_message(text, limit);
            // Verify it's valid UTF-8 (String guarantees this, but let's be explicit)
            assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        }
    }

    #[test]
    fn truncate_message_boundary_inside_multibyte_codepoint() {
        // Create a string where truncation boundary falls inside a multi-byte char.
        // '🎉' is 4 bytes at positions 0..4.
        let text = "🎉abcdef";
        let suffix = "\n\n[Output truncated. Full output saved locally.]";
        // available = 2 (inside the 4-byte emoji), should not panic
        let max_len = suffix.len() + 2;
        let result = truncate_message(text, max_len);
        assert!(result.contains("[Output truncated"));
    }

    // ─── Path traversal sanitization ───────────────────────────────────

    #[test]
    fn sanitize_job_id_strips_traversal() {
        assert_eq!(sanitize_job_id("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_job_id("../secret"), "secret");
        assert_eq!(sanitize_job_id("job/with/slashes"), "job_with_slashes");
        assert_eq!(sanitize_job_id("normal-job-123"), "normal-job-123");
    }

    #[test]
    fn sanitize_job_id_handles_empty_and_dots() {
        assert_eq!(sanitize_job_id(""), "_unnamed_");
        assert_eq!(sanitize_job_id(".."), "_unnamed_");
        assert_eq!(sanitize_job_id("..."), "_unnamed_");
        assert_eq!(sanitize_job_id("_"), "_unnamed_");
    }

    #[test]
    fn sanitize_job_id_handles_backslash_traversal() {
        assert_eq!(sanitize_job_id("..\\..\\Windows\\System32"), "Windows_System32");
    }

    #[test]
    fn save_local_output_rejects_path_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _ = save_local_output(dir.path(), "../../etc/passwd", "pwned")
            .expect("should save with sanitized path");
        // Must NOT create files outside the output directory
        assert!(!dir.path().join("../../etc/passwd").exists());
        // Should create the sanitized directory instead
        assert!(dir.path().join("etc_passwd").is_dir());
    }

    // ─── is_known_platform ──────────────────────────────────────────────

    #[test]
    fn known_platforms() {
        assert!(is_known_platform("telegram"));
        assert!(is_known_platform("discord"));
        assert!(is_known_platform("slack"));
        assert!(is_known_platform("whatsapp"));
        assert!(is_known_platform("homeassistant"));
        assert!(!is_known_platform("fax"));
        assert!(!is_known_platform(""));
    }

    // ─── End-to-end resolve + dispatch ──────────────────────────────────

    #[tokio::test]
    async fn end_to_end_resolve_and_dispatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut home_channels = std::collections::HashMap::new();
        home_channels.insert("telegram".to_owned(), "default-tg".to_owned());

        let config = DeliveryConfig {
            home_channels,
            always_log_locally: true,
            origin_platform: Some("slack".to_owned()),
            origin_chat_id: Some("slack-ch".to_owned()),
            output_dir: dir.path().to_path_buf(),
        };
        let router = DeliveryRouter::new(config);
        let sender = MockSender::new();

        let targets = DeliveryTarget::parse_multi("origin, telegram, local");
        let dests = router.resolve(&targets);

        // Should have: slack:slack-ch, telegram:default-tg, local
        // (local not duplicated even with always_log_locally because it's explicit)
        assert_eq!(dests.len(), 3);

        let report = router
            .dispatch("e2e-job", "end-to-end output", &dests, &sender)
            .await;
        assert!(report.all_succeeded());
        assert_eq!(report.success_count(), 3);
    }
}
