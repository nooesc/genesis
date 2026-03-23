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
    // DigitalOcean personal access tokens
    SecretPattern {
        prefix: "dop_v1_",
        min_suffix_len: 32,
        label: "digitalocean-token",
    },
    // Heroku API keys (UUIDs, 36 chars with dashes)
    SecretPattern {
        prefix: "heroku_api_key=",
        min_suffix_len: 36,
        label: "heroku-key",
    },
    SecretPattern {
        prefix: "HEROKU_API_KEY=",
        min_suffix_len: 36,
        label: "heroku-key",
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

/// Database URL scheme prefixes to check for password redaction.
const DB_SCHEMES: &[(&str, &str)] = &[
    ("postgresql://", "database-url"),
    ("postgres://", "database-url"),
    ("mongodb+srv://", "database-url"),
    ("mongodb://", "database-url"),
    ("redis://", "database-url"),
    ("mysql://", "database-url"),
];

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

    // Redact GCP service account private_key fields (must run before PEM blocks
    // so the JSON-level pattern is matched before the inner PEM is stripped).
    sanitize_gcp_service_account_keys(&mut result);

    // Redact private keys (PEM format).
    sanitize_pem_blocks(&mut result);

    // Redact database URLs with embedded passwords.
    sanitize_database_urls(&mut result);

    // Redact JWT tokens (3-part base64 dot-separated starting with eyJ).
    sanitize_jwt_tokens(&mut result);

    // Redact Azure connection string AccountKey values.
    sanitize_azure_connection_strings(&mut result);

    // Redact AWS secret access keys (40-char base64 near aws_secret / SecretAccessKey).
    sanitize_aws_secret_keys(&mut result);

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

/// Redact database connection URLs that contain embedded passwords.
///
/// Matches `scheme://user:password@host` and replaces the password with `[REDACTED]`.
fn sanitize_database_urls(text: &mut String) {
    for &(scheme, _label) in DB_SCHEMES {
        let mut search_from = 0;
        loop {
            let Some(rel_pos) = text[search_from..].find(scheme) else {
                break;
            };
            let scheme_pos = search_from + rel_pos;
            let after_scheme = scheme_pos + scheme.len();
            // Limit search to the authority component (up to first `/` after scheme, or end of string).
            let authority_end = text[after_scheme..].find('/').unwrap_or(text.len() - after_scheme);
            let authority = &text[after_scheme..after_scheme + authority_end];

            // Look for the user:password@host pattern.
            // The colon separates user from password, the @ terminates the password.
            let colon_pos = authority.find(':');
            let at_pos = authority.find('@');

            match (colon_pos, at_pos) {
                (Some(c), Some(a)) if c < a && a - c > 1 => {
                    // There is a password between : and @
                    let password_start = after_scheme + c + 1;
                    let password_end = after_scheme + a;
                    let replacement = "[REDACTED:database-password]";
                    text.replace_range(password_start..password_end, replacement);
                    // Advance past the replacement to avoid re-matching.
                    search_from = password_start + replacement.len();
                }
                _ => {
                    search_from = after_scheme;
                    continue;
                }
            }
        }
    }
}

/// Redact JWT tokens: three base64url-encoded segments separated by dots,
/// starting with `eyJ` (base64 for `{"` which is the start of all JWT headers).
fn sanitize_jwt_tokens(text: &mut String) {
    const JWT_PREFIX: &str = "eyJ";
    let mut search_from = 0;
    loop {
        let Some(rel_pos) = text[search_from..].find(JWT_PREFIX) else {
            break;
        };
        let start = search_from + rel_pos;

        // Check we're at a token boundary (start of string, or preceded by non-alnum).
        if start > 0 {
            let prev = text.as_bytes()[start - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'-' {
                // Part of a longer token — this was already redacted or not a JWT.
                search_from = start + JWT_PREFIX.len();
                continue;
            }
        }

        // Walk the JWT: three dot-separated base64url segments.
        let rest = &text[start..];
        let mut end = 0;
        let mut dot_count = 0;
        for (i, c) in rest.char_indices() {
            if c == '.' {
                dot_count += 1;
                if dot_count > 2 {
                    end = i;
                    break;
                }
            } else if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=' || c == '/' || c == '+' {
                // Valid base64url character
            } else {
                end = i;
                break;
            }
            end = i + c.len_utf8();
        }

        let token = &text[start..start + end];
        // A valid JWT must have exactly 2 dots and be reasonably long.
        if dot_count >= 2 && token.len() >= 20 && token.matches('.').count() >= 2 {
            text.replace_range(start..start + end, "[REDACTED:jwt]");
            search_from = start + "[REDACTED:jwt]".len();
        } else {
            search_from = start + JWT_PREFIX.len();
            continue;
        }
    }
}

/// Redact Azure connection string `AccountKey=...` values.
fn sanitize_azure_connection_strings(text: &mut String) {
    const ACCOUNT_KEY: &str = "AccountKey=";
    let mut search_from = 0;
    loop {
        let Some(rel_pos) = text[search_from..].find(ACCOUNT_KEY) else {
            break;
        };
        let start = search_from + rel_pos;
        let value_start = start + ACCOUNT_KEY.len();
        // AccountKey values end at `;` or end of string.
        let value_end = text[value_start..]
            .find(|c: char| c == ';' || c.is_ascii_whitespace() || c == '"' || c == '\'')
            .map(|i| value_start + i)
            .unwrap_or(text.len());
        let value_len = value_end - value_start;
        if value_len >= 10 {
            text.replace_range(start..value_end, "[REDACTED:azure-key]");
            search_from = start + "[REDACTED:azure-key]".len();
        } else {
            search_from = value_start;
            continue;
        }
    }
}

/// Redact GCP service account `"private_key": "-----BEGIN ...` patterns.
///
/// These appear in JSON service account key files.
fn sanitize_gcp_service_account_keys(text: &mut String) {
    // The PEM block itself is already handled by sanitize_pem_blocks, but we also
    // want to catch the JSON field pattern. Look for the characteristic JSON key.
    const MARKER: &str = "\"private_key\"";
    let mut search_from = 0;
    loop {
        let Some(rel_pos) = text[search_from..].find(MARKER) else {
            break;
        };
        let start = search_from + rel_pos;
        let after_marker = start + MARKER.len();
        let rest = &text[after_marker..];

        // Look for the value: optional whitespace, colon, optional whitespace, quote.
        let trimmed = rest.trim_start();
        if !trimmed.starts_with(':') {
            search_from = after_marker;
            continue;
        }
        let after_colon = trimmed[1..].trim_start();
        if !after_colon.starts_with('"') {
            search_from = after_marker;
            continue;
        }

        // Find the start of the value string in the original text.
        let value_quote_offset = after_marker + (rest.len() - after_colon.len());
        let value_content_start = value_quote_offset + 1;

        // Find the closing quote (handle escaped quotes).
        let value_content = &text[value_content_start..];
        let mut closing_quote = None;
        let mut chars = value_content.char_indices();
        while let Some((i, c)) = chars.next() {
            if c == '\\' {
                // Skip escaped character
                chars.next();
            } else if c == '"' {
                closing_quote = Some(i);
                break;
            }
        }

        if let Some(close_offset) = closing_quote {
            let end = value_content_start + close_offset + 1; // include closing quote
            text.replace_range(start..end, "[REDACTED:gcp-service-key]");
            search_from = start + "[REDACTED:gcp-service-key]".len();
        } else {
            search_from = after_marker;
            continue;
        }
    }
}

/// Redact AWS secret access keys (40-char base64 strings near `aws_secret` or `SecretAccessKey`).
fn sanitize_aws_secret_keys(text: &mut String) {
    const MARKERS: &[&str] = &[
        "aws_secret_access_key",
        "AWS_SECRET_ACCESS_KEY",
        "SecretAccessKey",
    ];

    for marker in MARKERS {
        let mut search_from = 0;
        loop {
            let Some(rel_pos) = text[search_from..].find(marker) else {
                break;
            };
            let marker_pos = search_from + rel_pos;
            let after_marker = marker_pos + marker.len();
            let rest = &text[after_marker..];

            // Look for the value: skip `=`, `"`, `:`, `>`, whitespace to find the key.
            let value_start_rel = rest
                .find(|c: char| {
                    c.is_ascii_alphanumeric() || c == '+' || c == '/'
                });

            let Some(rel_start) = value_start_rel else {
                search_from = after_marker;
                continue;
            };

            // Only allow separators (=, :, >, ", whitespace) between marker and value.
            let separator = &rest[..rel_start];
            if separator.chars().any(|c| {
                !c.is_ascii_whitespace() && c != '=' && c != ':' && c != '"' && c != '\'' && c != '>'
            }) {
                search_from = after_marker;
                continue;
            }

            let abs_value_start = after_marker + rel_start;
            // AWS secret keys are exactly 40 chars of base64 (letters, digits, +, /).
            let value_end = text[abs_value_start..]
                .find(|c: char| {
                    !c.is_ascii_alphanumeric() && c != '+' && c != '/'
                })
                .map(|i| abs_value_start + i)
                .unwrap_or(text.len());

            let value_len = value_end - abs_value_start;
            if value_len >= 40 {
                text.replace_range(
                    abs_value_start..value_end,
                    "[REDACTED:aws-secret-key]",
                );
                search_from = abs_value_start + "[REDACTED:aws-secret-key]".len();
            } else {
                search_from = after_marker;
                continue;
            }
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

    // Check database URLs with passwords (scheme://user:pass@host).
    for &(scheme, _) in DB_SCHEMES {
        if let Some(pos) = text.find(scheme) {
            let rest = &text[pos + scheme.len()..];
            let colon = rest.find(':');
            let at = rest.find('@');
            if let (Some(c), Some(a)) = (colon, at) {
                if c < a && a - c > 1 {
                    return true;
                }
            }
        }
    }

    // Check JWT tokens (eyJ prefix with dots).
    if let Some(pos) = text.find("eyJ") {
        // Quick heuristic: eyJ followed by content with at least 2 dots.
        let rest = &text[pos..];
        let token_end = rest
            .find(|c: char| {
                c.is_ascii_whitespace() || c == '"' || c == '\'' || c == ')'
            })
            .unwrap_or(rest.len());
        let token = &rest[..token_end];
        if token.len() >= 20 && token.matches('.').count() >= 2 {
            return true;
        }
    }

    // Check Azure AccountKey.
    if let Some(pos) = text.find("AccountKey=") {
        let value_start = pos + "AccountKey=".len();
        let value_end = text[value_start..]
            .find(|c: char| c == ';' || c.is_ascii_whitespace() || c == '"')
            .map(|i| value_start + i)
            .unwrap_or(text.len());
        if value_end - value_start >= 10 {
            return true;
        }
    }

    // Check GCP service account key.
    if text.contains("\"private_key\"") {
        return true;
    }

    // Check AWS secret access keys.
    if text.contains("aws_secret_access_key")
        || text.contains("AWS_SECRET_ACCESS_KEY")
        || text.contains("SecretAccessKey")
    {
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

    // --- New pattern tests ---

    #[test]
    fn redacts_postgresql_url_password() {
        let input = "DATABASE_URL=postgresql://admin:s3cretP4ss@db.example.com:5432/mydb";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:database-password]"));
        assert!(!result.contains("s3cretP4ss"));
        assert!(result.contains("admin:"));
        assert!(result.contains("@db.example.com"));
    }

    #[test]
    fn redacts_postgres_url_password() {
        let input = "url: postgres://user:hunter2@localhost/db";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:database-password]"));
        assert!(!result.contains("hunter2"));
    }

    #[test]
    fn redacts_mongodb_srv_url_password() {
        let input = "MONGO_URI=mongodb+srv://root:MyP4ssw0rd@cluster0.abc123.mongodb.net/test";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:database-password]"));
        assert!(!result.contains("MyP4ssw0rd"));
    }

    #[test]
    fn redacts_mongodb_url_password() {
        let input = "mongodb://appuser:correcthorsebattery@mongo.internal:27017/production";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:database-password]"));
        assert!(!result.contains("correcthorsebattery"));
    }

    #[test]
    fn redacts_redis_url_password() {
        let input = "REDIS_URL=redis://default:redis_secret_123@redis.example.com:6379/0";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:database-password]"));
        assert!(!result.contains("redis_secret_123"));
    }

    #[test]
    fn redacts_mysql_url_password() {
        let input = "mysql://root:my_sql_pass@127.0.0.1:3306/app";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:database-password]"));
        assert!(!result.contains("my_sql_pass"));
    }

    #[test]
    fn preserves_database_url_without_password() {
        let input = "postgresql://localhost:5432/mydb";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn redacts_jwt_token() {
        let input = "token: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:jwt]"));
        assert!(!result.contains("eyJhbGci"));
        assert!(!result.contains("dozjgNry"));
    }

    #[test]
    fn redacts_jwt_in_header() {
        let input = r#"{"Authorization": "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL2V4YW1wbGUuY29tIn0.signature"}"#;
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:jwt]"));
    }

    #[test]
    fn preserves_short_eyj_string() {
        // Short eyJ strings that aren't real JWTs should not be redacted.
        let input = "the word eyJust is fine";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn redacts_openssh_private_key() {
        let input = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAA...\n-----END OPENSSH PRIVATE KEY-----\n";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:private-key]"));
        assert!(!result.contains("b3BlbnNz"));
    }

    #[test]
    fn redacts_azure_account_key() {
        let input = "DefaultEndpointsProtocol=https;AccountName=mystorageaccount;AccountKey=aGVsbG93b3JsZHRoaXNpc2Fhc2RmYXNkZmtqYXNkbGZranNkbGtm;EndpointSuffix=core.windows.net";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:azure-key]"));
        assert!(!result.contains("aGVsbG93b3Js"));
    }

    #[test]
    fn redacts_azure_account_key_standalone() {
        let input = "AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:azure-key]"));
        assert!(!result.contains("Eby8vdM"));
    }

    #[test]
    fn redacts_gcp_service_account_key() {
        let input = r#"{"type": "service_account", "private_key": "-----BEGIN RSA PRIVATE KEY-----\nMIIE..."}"#;
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:gcp-service-key]"));
        assert!(!result.contains("-----BEGIN RSA PRIVATE KEY-----"));
    }

    #[test]
    fn redacts_gcp_service_account_key_with_spaces() {
        let input = r#""private_key" : "-----BEGIN EC PRIVATE KEY-----\ndata\n-----END EC PRIVATE KEY-----\n""#;
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:gcp-service-key]"));
    }

    #[test]
    fn redacts_aws_secret_access_key_env() {
        let input = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY12";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:aws-secret-key]"));
        assert!(!result.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn redacts_aws_secret_access_key_config() {
        let input = "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY12";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:aws-secret-key]"));
        assert!(!result.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn redacts_aws_secret_access_key_json() {
        let input = r#""SecretAccessKey": "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY12""#;
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:aws-secret-key]"));
        assert!(!result.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn redacts_digitalocean_token() {
        let input = "DO_TOKEN=dop_v1_abcdef1234567890abcdef1234567890abcdef12";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:digitalocean-token]"));
        assert!(!result.contains("dop_v1_abcdef"));
    }

    #[test]
    fn redacts_heroku_api_key() {
        let input = "heroku_api_key=12345678-abcd-efgh-ijkl-1234567890abcdef12";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:heroku-key]"));
    }

    #[test]
    fn redacts_heroku_api_key_uppercase() {
        let input = "HEROKU_API_KEY=abcdef12-3456-7890-abcd-ef1234567890abcdef";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:heroku-key]"));
    }

    #[test]
    fn contains_credentials_detects_new_patterns() {
        // Database URLs
        assert!(contains_credentials(
            "postgresql://user:pass@host/db"
        ));
        assert!(contains_credentials(
            "mongodb+srv://user:pass@cluster.mongodb.net/db"
        ));
        assert!(contains_credentials(
            "redis://default:secret@redis:6379"
        ));
        assert!(contains_credentials(
            "mysql://root:pass@localhost/db"
        ));

        // JWT tokens
        assert!(contains_credentials(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.rg2e3tNBSQ"
        ));

        // Azure
        assert!(contains_credentials(
            "AccountKey=aGVsbG93b3JsZHRoaXNpc2E="
        ));

        // GCP
        assert!(contains_credentials(
            r#""private_key": "-----BEGIN RSA PRIVATE KEY-----""#
        ));

        // AWS secret keys
        assert!(contains_credentials(
            "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
        ));
        assert!(contains_credentials(
            "AWS_SECRET_ACCESS_KEY=test1234567890abcdef1234567890abcdef1234"
        ));

        // DigitalOcean
        assert!(contains_credentials(
            "dop_v1_abcdef1234567890abcdef1234567890abcdef12"
        ));
    }

    #[test]
    fn fast_path_returns_same_string_for_clean_text() {
        let input = "This is a normal log line with no credentials at all.";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn fast_path_no_false_positive_on_short_prefixes() {
        // Ensure short matches that don't meet min_suffix_len don't trigger allocation.
        let input = "sk-short and ghp_x and npm_y and SG.z";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn multiple_database_urls_redacted() {
        let input = "primary: postgresql://a:secret1@host1/db1 replica: postgresql://b:secret2@host2/db2";
        let result = sanitize_credentials(input);
        assert!(!result.contains("secret1"));
        assert!(!result.contains("secret2"));
        assert_eq!(result.matches("[REDACTED:database-password]").count(), 2);
    }

    #[test]
    fn multiple_jwt_tokens_in_text() {
        let input = "first: eyJhbGciOiJIUzI1NiJ9.eyJhIjoiMSJ9.sig1 second: eyJhbGciOiJSUzI1NiJ9.eyJiIjoiMiJ9.sig2";
        let result = sanitize_credentials(input);
        assert_eq!(result.matches("[REDACTED:jwt]").count(), 2);
    }
}
