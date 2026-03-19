//! Adaptive model routing — rule-based task classification.
//!
//! Analyzes conversation context to select the optimal model tier.
//! Phase 1 uses heuristic rules; Phase 2 will use learned routing.

/// Complexity tier for model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Simple tasks: greetings, short factual Q&A, simple lookups.
    Cheap,
    /// Moderate tasks: code editing, tool use, standard development.
    Mid,
    /// Complex tasks: multi-step reasoning, architecture, analysis.
    Top,
}

impl Tier {
    /// Parse from string, defaulting to Mid for unknown values.
    pub fn from_str_or_mid(s: &str) -> Self {
        match s {
            "cheap" | "low" => Tier::Cheap,
            "mid" | "medium" | "default" => Tier::Mid,
            "top" | "high" => Tier::Top,
            _ => Tier::Mid,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Cheap => "cheap",
            Tier::Mid => "mid",
            Tier::Top => "top",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Context signals extracted from conversation history.
struct ContextSignals {
    /// Length of the latest user message in characters.
    message_length: usize,
    /// Number of code blocks (``` fences) in the latest message.
    code_block_count: usize,
    /// Whether the message mentions file paths.
    has_file_paths: bool,
    /// Number of tool calls made in the conversation so far.
    tool_calls_so_far: usize,
    /// Number of turns used so far in this user request.
    turns_used: usize,
    /// Whether a previous turn in this request failed.
    had_failure: bool,
    /// Whether the message contains reasoning-heavy keywords.
    has_reasoning_keywords: bool,
    /// Whether the message is a simple greeting or acknowledgment.
    is_greeting: bool,
}

/// Classify the current turn into a complexity tier.
///
/// This is the Phase 1 rule-based classifier. It examines the latest
/// user message and conversation context to determine complexity.
pub fn classify(
    latest_message: &str,
    tool_calls_so_far: usize,
    turns_used: usize,
    had_failure: bool,
    default_tier: Tier,
) -> Tier {
    let signals = extract_signals(latest_message, tool_calls_so_far, turns_used, had_failure);

    // Escalation: if a previous turn failed, bump up one tier
    if signals.had_failure && signals.turns_used > 1 {
        return Tier::Top;
    }

    // Complex reasoning signals → Top (checked early so context-heavy
    // conversations are not short-circuited by a brief follow-up message)
    if signals.code_block_count >= 3 {
        return Tier::Top;
    }
    if signals.has_reasoning_keywords && signals.message_length > 200 {
        return Tier::Top;
    }
    if signals.tool_calls_so_far >= 5 && signals.turns_used >= 3 {
        return Tier::Top;
    }

    // Code editing or moderate complexity → Mid (checked before greetings
    // so that file paths and code blocks are not masked by greeting words)
    if signals.code_block_count > 0 || signals.has_file_paths {
        return Tier::Mid;
    }
    if signals.message_length >= 200 {
        return Tier::Mid;
    }

    // Greetings and very short messages → Cheap
    if signals.is_greeting {
        return Tier::Cheap;
    }
    if signals.message_length < 50 && signals.code_block_count == 0 && !signals.has_file_paths {
        return Tier::Cheap;
    }

    // Default: use the configured default tier
    default_tier
}

fn extract_signals(
    message: &str,
    tool_calls_so_far: usize,
    turns_used: usize,
    had_failure: bool,
) -> ContextSignals {
    let message_lower = message.to_lowercase();

    let code_block_count = message.matches("```").count() / 2; // pairs

    let has_file_paths = message.contains('/')
        && (message.contains(".rs")
            || message.contains(".py")
            || message.contains(".ts")
            || message.contains(".js")
            || message.contains(".go")
            || message.contains(".java")
            || message.contains("src/")
            || message.contains("crates/"));

    let reasoning_keywords = [
        "analyze",
        "architect",
        "compare",
        "contrast",
        "design",
        "evaluate",
        "explain why",
        "implications",
        "optimize",
        "refactor",
        "review",
        "strategy",
        "trade-off",
        "tradeoff",
    ];
    let has_reasoning_keywords = reasoning_keywords
        .iter()
        .any(|kw| message_lower.contains(kw));

    let greetings = [
        "hi",
        "hello",
        "hey",
        "thanks",
        "thank you",
        "ok",
        "okay",
        "yes",
        "no",
        "sure",
    ];
    let is_greeting = message.split_whitespace().count() <= 5
        && greetings
            .iter()
            .any(|g| message_lower.trim() == *g || message_lower.starts_with(g));

    ContextSignals {
        message_length: message.len(),
        code_block_count,
        has_file_paths,
        tool_calls_so_far,
        turns_used,
        had_failure,
        has_reasoning_keywords,
        is_greeting,
    }
}

/// Select the model name for a given tier, falling back to the primary model.
pub fn model_for_tier<'a>(
    tier: Tier,
    cheap_model: Option<&'a str>,
    mid_model: Option<&'a str>,
    top_model: Option<&'a str>,
    primary_model: &'a str,
) -> &'a str {
    match tier {
        Tier::Cheap => cheap_model.unwrap_or(primary_model),
        Tier::Mid => mid_model.unwrap_or(primary_model),
        Tier::Top => top_model.unwrap_or(primary_model),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_routes_to_cheap() {
        assert_eq!(classify("hello", 0, 0, false, Tier::Mid), Tier::Cheap);
        assert_eq!(classify("hi", 0, 0, false, Tier::Mid), Tier::Cheap);
        assert_eq!(classify("thanks", 0, 0, false, Tier::Mid), Tier::Cheap);
    }

    #[test]
    fn short_message_routes_to_cheap() {
        assert_eq!(
            classify("what is 2+2?", 0, 0, false, Tier::Mid),
            Tier::Cheap
        );
        assert_eq!(classify("list files", 0, 0, false, Tier::Mid), Tier::Cheap);
    }

    #[test]
    fn code_editing_routes_to_mid() {
        let msg = "Please fix the bug in src/main.rs where the parser fails";
        assert_eq!(classify(msg, 0, 0, false, Tier::Mid), Tier::Mid);
    }

    #[test]
    fn code_blocks_route_to_mid() {
        let msg = "Update this function:\n```rust\nfn foo() {}\n```\nto return a Result";
        assert_eq!(classify(msg, 0, 0, false, Tier::Mid), Tier::Mid);
    }

    #[test]
    fn many_code_blocks_route_to_top() {
        let msg = "Compare these implementations:\n```\nfn a() {}\n```\nvs\n```\nfn b() {}\n```\nvs\n```\nfn c() {}\n```\nWhich is best?";
        assert_eq!(classify(msg, 0, 0, false, Tier::Mid), Tier::Top);
    }

    #[test]
    fn reasoning_keywords_with_long_message_route_to_top() {
        let msg = "I need you to analyze the architecture of this system and compare the trade-offs between using an event-sourced approach versus a traditional CRUD pattern. Consider the implications for scalability, maintainability, and testing. Please also evaluate the performance characteristics.";
        assert_eq!(classify(msg, 0, 0, false, Tier::Mid), Tier::Top);
    }

    #[test]
    fn escalation_on_failure() {
        assert_eq!(classify("try again", 3, 2, true, Tier::Mid), Tier::Top);
    }

    #[test]
    fn many_tool_calls_routes_to_top() {
        assert_eq!(classify("continue", 6, 4, false, Tier::Mid), Tier::Top);
    }

    #[test]
    fn long_message_without_keywords_routes_to_mid() {
        let msg = "a ".repeat(120); // 240 chars, no keywords
        assert_eq!(classify(&msg, 0, 0, false, Tier::Mid), Tier::Mid);
    }

    #[test]
    fn greeting_with_file_path_routes_to_mid() {
        assert_eq!(
            classify("yes check src/main.rs", 0, 0, false, Tier::Mid),
            Tier::Mid
        );
        assert_eq!(
            classify(
                "ok fix the bug in crates/core/lib.rs",
                0,
                0,
                false,
                Tier::Mid
            ),
            Tier::Mid
        );
    }

    #[test]
    fn default_tier_is_used_for_ambiguous() {
        // A message that doesn't match any specific rule uses the configured default.
        // Must be >= 50 chars, < 200 chars, no code blocks, no file paths, no
        // reasoning keywords, and not a greeting to fall through to default.
        let msg = "I would like you to help me write a short poem about the sunset over the ocean";
        assert_eq!(classify(msg, 0, 0, false, Tier::Cheap), Tier::Cheap);
        assert_eq!(classify(msg, 0, 0, false, Tier::Top), Tier::Top);
    }

    #[test]
    fn model_for_tier_uses_configured_models() {
        assert_eq!(
            model_for_tier(
                Tier::Cheap,
                Some("haiku"),
                Some("sonnet"),
                Some("opus"),
                "default"
            ),
            "haiku"
        );
        assert_eq!(
            model_for_tier(
                Tier::Mid,
                Some("haiku"),
                Some("sonnet"),
                Some("opus"),
                "default"
            ),
            "sonnet"
        );
        assert_eq!(
            model_for_tier(
                Tier::Top,
                Some("haiku"),
                Some("sonnet"),
                Some("opus"),
                "default"
            ),
            "opus"
        );
    }

    #[test]
    fn model_for_tier_falls_back_to_primary() {
        assert_eq!(
            model_for_tier(Tier::Cheap, None, None, None, "fallback"),
            "fallback"
        );
        assert_eq!(
            model_for_tier(Tier::Top, None, None, None, "fallback"),
            "fallback"
        );
    }

    #[test]
    fn tier_round_trips() {
        assert_eq!(Tier::from_str_or_mid("cheap"), Tier::Cheap);
        assert_eq!(Tier::from_str_or_mid("mid"), Tier::Mid);
        assert_eq!(Tier::from_str_or_mid("top"), Tier::Top);
        assert_eq!(Tier::from_str_or_mid("unknown"), Tier::Mid);
    }
}
