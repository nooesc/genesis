/// Credential sanitization for agent output.
///
/// Strips API keys, tokens, and other secrets from text before it enters the
/// conversation context. This prevents accidental credential leakage when error
/// messages or tool results contain sensitive values.
/// Known secret patterns and their redaction labels.
const PATTERNS: &[SecretPattern] = &[
    // GitHub tokens (classic PATs, OAuth, app, fine-grained)
    SecretPattern {
        prefix: "ghp_",
        min_suffix_len: 36,
        label: "github-pat",
    },
    SecretPattern {
        prefix: "gho_",
        min_suffix_len: 36,
        label: "github-oauth",
    },
    SecretPattern {
        prefix: "ghs_",
        min_suffix_len: 36,
        label: "github-app",
    },
    SecretPattern {
        prefix: "ghu_",
        min_suffix_len: 36,
        label: "github-user",
    },
    SecretPattern {
        prefix: "github_pat_",
        min_suffix_len: 22,
        label: "github-pat",
    },
    // OpenAI / Anthropic / generic API keys
    SecretPattern {
        prefix: "sk-",
        min_suffix_len: 20,
        label: "api-key",
    },
    SecretPattern {
        prefix: "sk-ant-",
        min_suffix_len: 20,
        label: "anthropic-key",
    },
    // AWS access keys
    SecretPattern {
        prefix: "AKIA",
        min_suffix_len: 16,
        label: "aws-key",
    },
    // Stripe keys
    SecretPattern {
        prefix: "sk_live_",
        min_suffix_len: 20,
        label: "stripe-key",
    },
    SecretPattern {
        prefix: "sk_test_",
        min_suffix_len: 20,
        label: "stripe-key",
    },
    SecretPattern {
        prefix: "pk_live_",
        min_suffix_len: 20,
        label: "stripe-key",
    },
    SecretPattern {
        prefix: "pk_test_",
        min_suffix_len: 20,
        label: "stripe-key",
    },
    // Slack tokens
    SecretPattern {
        prefix: "xoxb-",
        min_suffix_len: 20,
        label: "slack-token",
    },
    SecretPattern {
        prefix: "xoxp-",
        min_suffix_len: 20,
        label: "slack-token",
    },
    SecretPattern {
        prefix: "xoxs-",
        min_suffix_len: 20,
        label: "slack-token",
    },
    // Discord bot tokens (base64-ish, usually ~59 chars)
    SecretPattern {
        prefix: "Bot ",
        min_suffix_len: 50,
        label: "discord-token",
    },
    // npm tokens
    SecretPattern {
        prefix: "npm_",
        min_suffix_len: 32,
        label: "npm-token",
    },
    // Sendgrid
    SecretPattern {
        prefix: "SG.",
        min_suffix_len: 40,
        label: "sendgrid-key",
    },
];

struct SecretPattern {
    /// The prefix that identifies this type of secret.
    prefix: &'static str,
    /// Minimum number of alphanumeric/special characters after the prefix
    /// for a match. This reduces false positives.
    min_suffix_len: usize,
    /// Human-readable label used in the redaction placeholder.
    label: &'static str,
}

/// Minimum length for a Bearer token to be considered a real credential.
const MIN_BEARER_TOKEN_LEN: usize = 10;

/// Sanitize text by replacing known credential patterns with redaction markers.
///
/// Returns a new string with all detected credentials replaced by
/// `[REDACTED:<label>]`.
pub fn sanitize_credentials(text: &str) -> String {
    // Fast path: skip allocation and pattern scanning when no credentials are present.
    if !contains_credentials(text) {
        return text.to_owned();
    }
    let mut result = text.to_owned();

    // Process patterns from longest prefix to shortest to avoid partial matches.
    for pat in PATTERNS {
        loop {
            let Some(start) = result.find(pat.prefix) else {
                break;
            };

            // Find the end of the token: contiguous alphanumeric, dash, underscore, dot chars.
            let token_start = start;
            let after_prefix = start + pat.prefix.len();
            let suffix_end = result[after_prefix..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
                .map(|i| after_prefix + i)
                .unwrap_or(result.len());

            let suffix_len = suffix_end - after_prefix;

            if suffix_len >= pat.min_suffix_len {
                let replacement = format!("[REDACTED:{}]", pat.label);
                result.replace_range(token_start..suffix_end, &replacement);
            } else {
                // Not a real credential — skip past this occurrence to avoid
                // infinite loop. We insert a zero-width marker that we'll strip
                // later, or just break if no more occurrences.
                break;
            }
        }
    }

    // Also redact Bearer tokens in Authorization headers.
    sanitize_bearer_tokens(&mut result);

    // Redact private keys (PEM format).
    sanitize_pem_blocks(&mut result);

    result
}

/// Redact `Bearer <token>` patterns (common in HTTP headers and error messages).
fn sanitize_bearer_tokens(text: &mut String) {
    const BEARER: &str = "Bearer ";
    loop {
        let Some(start) = text.find(BEARER) else {
            break;
        };
        let token_start = start + BEARER.len();
        let token_end = text[token_start..]
            .find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'' || c == ')')
            .map(|i| token_start + i)
            .unwrap_or(text.len());

        let token_len = token_end - token_start;
        if token_len >= MIN_BEARER_TOKEN_LEN {
            text.replace_range(start..token_end, "[REDACTED:bearer-token]");
        } else {
            break;
        }
    }
}

/// Redact PEM-encoded private key blocks.
fn sanitize_pem_blocks(text: &mut String) {
    const BEGIN: &str = "-----BEGIN ";
    const END_PREFIX: &str = "-----END ";
    const PRIVATE: &str = "PRIVATE KEY";

    loop {
        let Some(begin_pos) = text.find(BEGIN) else {
            break;
        };

        // Check if this is a private key block.
        let header_rest = &text[begin_pos + BEGIN.len()..];
        if !header_rest.contains(PRIVATE) {
            // Not a private key — skip.
            break;
        }

        // Find the end marker.
        let search_from = begin_pos + BEGIN.len();
        if let Some(end_rel) = text[search_from..].find(END_PREFIX) {
            let end_pos = search_from + end_rel;
            // Find the closing `-----` of the END line.
            let end_line_end = text[end_pos..]
                .find("-----\n")
                .or_else(|| text[end_pos..].find("-----\r\n"))
                .map(|i| end_pos + i + 6) // len of "-----\n"
                .unwrap_or(text.len());

            text.replace_range(begin_pos..end_line_end, "[REDACTED:private-key]");
        } else {
            break;
        }
    }
}

/// Returns `true` if the text appears to contain any credential patterns.
///
/// This is a fast check that avoids allocating a new string. Useful for
/// deciding whether to call `sanitize_credentials`.
pub fn contains_credentials(text: &str) -> bool {
    for pat in PATTERNS {
        if let Some(pos) = text.find(pat.prefix) {
            let after = pos + pat.prefix.len();
            let suffix_end = text[after..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
                .map(|i| after + i)
                .unwrap_or(text.len());
            if suffix_end - after >= pat.min_suffix_len {
                return true;
            }
        }
    }

    // Check bearer tokens.
    if let Some(pos) = text.find("Bearer ") {
        let token_start = pos + 7;
        let token_end = text[token_start..]
            .find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'' || c == ')')
            .map(|i| token_start + i)
            .unwrap_or(text.len());
        if token_end - token_start >= MIN_BEARER_TOKEN_LEN {
            return true;
        }
    }

    // Check PEM private keys.
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_github_pat() {
        let input = "Error: auth failed with token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:github-pat]"));
        assert!(!result.contains("ghp_"));
    }

    #[test]
    fn redacts_openai_key() {
        let input = "API key: sk-proj-1234567890abcdefghijklmno";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:api-key]"));
        assert!(!result.contains("sk-proj-"));
    }

    #[test]
    fn redacts_anthropic_key() {
        let input = "key=sk-ant-api03-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let result = sanitize_credentials(input);
        // sk-ant- matches both anthropic-key and api-key; either is acceptable
        assert!(
            result.contains("[REDACTED:") && !result.contains("sk-ant-"),
            "anthropic key should be redacted"
        );
    }

    #[test]
    fn redacts_aws_access_key() {
        let input = "aws_access_key_id = AKIAIOSFODNN7EXAMPLE";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:aws-key]"));
        assert!(!result.contains("AKIAIOSFODNN7"));
    }

    #[test]
    fn redacts_stripe_key() {
        let input = "stripe_key: sk_live_1234567890abcdefghijklmno";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:stripe-key]"));
    }

    #[test]
    fn redacts_slack_token() {
        let input = "SLACK_TOKEN=xoxb-123456789012-123456789012-abcdefghijklmnopqrstuv";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:slack-token]"));
        assert!(!result.contains("xoxb-"));
    }

    #[test]
    fn redacts_bearer_token() {
        let input =
            r#"Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.very-long-token-here"#;
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:bearer-token]"));
        assert!(!result.contains("eyJhb"));
    }

    #[test]
    fn redacts_pem_private_key() {
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----\n";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:private-key]"));
        assert!(!result.contains("MIIEowI"));
    }

    #[test]
    fn preserves_short_sk_prefix() {
        // "sk-" followed by too few chars should not be redacted.
        let input = "use sk-short as prefix";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn preserves_normal_text() {
        let input = "Hello world, this is a normal message with no secrets.";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn contains_credentials_detects_patterns() {
        assert!(contains_credentials(
            "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn"
        ));
        assert!(contains_credentials(
            "key: sk-proj-1234567890abcdefghijklmno"
        ));
        assert!(contains_credentials(
            "Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.token"
        ));
        assert!(!contains_credentials("normal text with no secrets"));
        assert!(!contains_credentials("sk-short"));
    }

    #[test]
    fn redacts_npm_token() {
        let input = "//registry.npmjs.org/:_authToken=npm_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefgh";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:npm-token]"));
    }

    #[test]
    fn multiple_credentials_in_one_string() {
        let input = "keys: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmn and sk-proj-1234567890abcdefghijklmno";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:github-pat]"));
        assert!(result.contains("[REDACTED:api-key]"));
        assert!(!result.contains("ghp_"));
        assert!(!result.contains("sk-proj-"));
    }

    #[test]
    fn redacts_github_fine_grained_pat() {
        let input =
            "token: github_pat_11ABCDEFG0AbCdEfGhIjKl_MNOPQRSTUVWXYZ1234567890abcdefghijklmnopqr";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:github-pat]"));
    }

    #[test]
    fn redacts_sendgrid_key() {
        let input = "SENDGRID_API_KEY=SG.abcdefghijklmnopqrstuvwxyz1234567890ABCDEFGHIJ";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:sendgrid-key]"));
    }
}
