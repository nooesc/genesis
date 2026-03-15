//! Credential redaction for logs and error messages.
//!
//! Scans text for patterns that look like API keys, tokens, passwords,
//! and connection strings, replacing them with `[REDACTED]`.

use regex::Regex;
use std::sync::LazyLock;

/// A compiled redaction rule.
struct RedactRule {
    pattern: Regex,
    replacement: &'static str,
}

static RULES: LazyLock<Vec<RedactRule>> = LazyLock::new(|| {
    vec![
        // Bearer / token auth headers
        RedactRule {
            pattern: Regex::new(r"(?i)(bearer\s+)\S+").unwrap(),
            replacement: "${1}[REDACTED]",
        },
        // Authorization header values (e.g. "authorization: Basic dXNlcjpwYXNz")
        RedactRule {
            pattern: Regex::new(r"(?i)(authorization:\s*)\S+(\s+\S+)?").unwrap(),
            replacement: "${1}[REDACTED]",
        },
        // Generic API key patterns (sk-..., key-..., etc.)
        RedactRule {
            pattern: Regex::new(r"\b(sk-[a-zA-Z0-9]{20,})\b").unwrap(),
            replacement: "[REDACTED]",
        },
        RedactRule {
            pattern: Regex::new(r"\b(key-[a-zA-Z0-9]{20,})\b").unwrap(),
            replacement: "[REDACTED]",
        },
        // OpenAI API keys (sk-proj-...)
        RedactRule {
            pattern: Regex::new(r"\b(sk-proj-[a-zA-Z0-9_-]{20,})\b").unwrap(),
            replacement: "[REDACTED]",
        },
        // Anthropic API keys (sk-ant-...)
        RedactRule {
            pattern: Regex::new(r"\b(sk-ant-[a-zA-Z0-9_-]{20,})\b").unwrap(),
            replacement: "[REDACTED]",
        },
        // OpenRouter API keys (sk-or-...)
        RedactRule {
            pattern: Regex::new(r"\b(sk-or-[a-zA-Z0-9_-]{20,})\b").unwrap(),
            replacement: "[REDACTED]",
        },
        // Generic long hex/base64 tokens (40+ chars, likely secrets)
        RedactRule {
            pattern: Regex::new(r"\b([a-fA-F0-9]{40,})\b").unwrap(),
            replacement: "[REDACTED]",
        },
        // password= or passwd= or pwd= in query strings / config
        RedactRule {
            pattern: Regex::new(r"(?i)(password|passwd|pwd)\s*[=:]\s*\S+").unwrap(),
            replacement: "${1}=[REDACTED]",
        },
        // api_key= or apikey= in query strings
        RedactRule {
            pattern: Regex::new(r"(?i)(api[_-]?key)\s*[=:]\s*\S+").unwrap(),
            replacement: "${1}=[REDACTED]",
        },
        // secret= or token= in query strings
        RedactRule {
            pattern: Regex::new(r"(?i)(secret|token)\s*[=:]\s*\S+").unwrap(),
            replacement: "${1}=[REDACTED]",
        },
        // Database connection strings with credentials
        RedactRule {
            pattern: Regex::new(r"(?i)(postgres|mysql|mongodb|redis)://[^@\s]+@").unwrap(),
            replacement: "${1}://[REDACTED]@",
        },
    ]
});

/// Redact credentials and secrets from the given text.
pub fn redact(text: &str) -> String {
    // Fast path: skip regex replacements when no patterns match.
    if !contains_secret(text) {
        return text.to_owned();
    }
    let mut result = text.to_owned();
    for rule in RULES.iter() {
        result = rule.pattern.replace_all(&result, rule.replacement).into_owned();
    }
    result
}

/// Returns true if the text likely contains a secret that would be redacted.
pub fn contains_secret(text: &str) -> bool {
    RULES.iter().any(|rule| rule.pattern.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_tokens() {
        let input = "Authorization failed: Bearer sk-abc123456789012345678901234567890123";
        let output = redact(input);
        assert!(!output.contains("sk-abc123456789012345678901234567890123"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_openai_api_keys() {
        let input = "Using key sk-proj-abc123def456ghi789jkl012mno345";
        let output = redact(input);
        assert!(!output.contains("sk-proj-abc123def456ghi789jkl012mno345"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_anthropic_api_keys() {
        let input = "API key: sk-ant-api03-abcdef12345678901234567890123456";
        let output = redact(input);
        assert!(!output.contains("sk-ant-api03-abcdef12345678901234567890123456"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_password_fields() {
        let input = "config: password=SuperSecret123! host=localhost";
        let output = redact(input);
        assert!(!output.contains("SuperSecret123!"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_api_key_fields() {
        let input = "api_key=my-super-secret-key-value";
        let output = redact(input);
        assert!(!output.contains("my-super-secret-key-value"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_database_urls() {
        let input = "postgres://admin:s3cret@db.example.com:5432/mydb";
        let output = redact(input);
        assert!(!output.contains("admin:s3cret"));
        assert!(output.contains("[REDACTED]@"));
    }

    #[test]
    fn redacts_mongodb_urls() {
        let input = "mongodb://user:password123@cluster.mongodb.net/db";
        let output = redact(input);
        assert!(!output.contains("user:password123"));
        assert!(output.contains("[REDACTED]@"));
    }

    #[test]
    fn redacts_long_hex_tokens() {
        let token = "a".repeat(48);
        let input = format!("Token: {token}");
        let output = redact(&input);
        assert!(!output.contains(&token));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn preserves_normal_text() {
        let input = "Hello world, this is a normal log message with no secrets.";
        let output = redact(input);
        assert_eq!(output, input);
    }

    #[test]
    fn preserves_short_hex_strings() {
        let input = "commit abc123def is good";
        let output = redact(input);
        assert_eq!(output, input);
    }

    #[test]
    fn contains_secret_detects_keys() {
        assert!(contains_secret("key sk-proj-abc123def456ghi789jkl012mno345"));
        assert!(!contains_secret("Hello world"));
    }

    #[test]
    fn redacts_authorization_headers() {
        let input = "authorization: Basic dXNlcjpwYXNz";
        let output = redact(input);
        assert!(!output.contains("dXNlcjpwYXNz"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_token_fields() {
        let input = "token=ghp_1234567890abcdef1234567890abcdef12345678";
        let output = redact(input);
        assert!(!output.contains("ghp_1234567890abcdef1234567890abcdef12345678"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_secret_fields() {
        let input = "secret: my_webhook_secret_value_here";
        let output = redact(input);
        assert!(!output.contains("my_webhook_secret_value_here"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn handles_multiple_secrets_in_one_string() {
        let input = "password=abc123 api_key=xyz789 token=tok_value";
        let output = redact(input);
        assert!(!output.contains("abc123"));
        assert!(!output.contains("xyz789"));
        assert!(!output.contains("tok_value"));
    }
}
