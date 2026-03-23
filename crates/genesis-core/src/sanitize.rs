//! Credential sanitization for agent output.
//!
//! Strips API keys, tokens, and other secrets from text before it enters the
//! conversation context. This prevents accidental credential leakage when error
//! messages or tool results contain sensitive values.

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
    // Heroku API keys (UUID-like, 36 chars)
    SecretPattern {
        prefix: "heroku_api_key=",
        min_suffix_len: 30,
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

/// Database URL schemes to scan for embedded passwords.
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

    // Process prefix-based patterns using search_from to avoid skipping later
    // occurrences when an earlier match is too short.
    for pat in PATTERNS {
        let mut search_from = 0;
        loop {
            let Some(rel) = result[search_from..].find(pat.prefix) else {
                break;
            };
            let start = search_from + rel;

            // Find the end of the token: contiguous alphanumeric, dash, underscore, dot chars.
            let after_prefix = start + pat.prefix.len();
            let suffix_end = result[after_prefix..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
                .map(|i| after_prefix + i)
                .unwrap_or(result.len());

            let suffix_len = suffix_end - after_prefix;

            if suffix_len >= pat.min_suffix_len {
                let replacement = format!("[REDACTED:{}]", pat.label);
                let replacement_len = replacement.len();
                result.replace_range(start..suffix_end, &replacement);
                search_from = start + replacement_len;
            } else {
                // Not a real credential — advance past prefix to continue scanning.
                search_from = after_prefix;
            }
        }
    }

    // Redact Bearer tokens in Authorization headers.
    sanitize_bearer_tokens(&mut result);

    // Redact GCP service account private keys (must run before generic PEM
    // redaction so we can detect the JSON `"private_key"` context).
    sanitize_gcp_service_account_keys(&mut result);

    // Redact PEM-encoded private key blocks.
    sanitize_pem_blocks(&mut result);

    // Redact OpenSSH private keys.
    sanitize_openssh_keys(&mut result);

    // Redact JWT tokens (eyJ... three base64 parts separated by dots).
    sanitize_jwt_tokens(&mut result);

    // Redact database URLs with embedded passwords.
    sanitize_database_urls(&mut result);

    // Redact Azure connection strings.
    sanitize_azure_connection_strings(&mut result);

    // Redact AWS secret access keys (contextual, near markers).
    sanitize_aws_secret_keys(&mut result);

    // Redact Heroku API keys in env-style format (HEROKU_API_KEY=...).
    sanitize_heroku_env_keys(&mut result);

    result
}

/// Redact `Bearer <token>` patterns (common in HTTP headers and error messages).
fn sanitize_bearer_tokens(text: &mut String) {
    const BEARER: &str = "Bearer ";
    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(BEARER) else {
            break;
        };
        let start = search_from + rel;
        let token_start = start + BEARER.len();
        let token_end = text[token_start..]
            .find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'' || c == ')')
            .map(|i| token_start + i)
            .unwrap_or(text.len());

        let token_len = token_end - token_start;
        if token_len >= MIN_BEARER_TOKEN_LEN {
            let redaction = "[REDACTED:bearer-token]";
            text.replace_range(start..token_end, redaction);
            search_from = start + redaction.len();
        } else {
            search_from = token_start;
        }
    }
}

/// Redact PEM-encoded private key blocks.
fn sanitize_pem_blocks(text: &mut String) {
    const BEGIN: &str = "-----BEGIN ";
    const END_PREFIX: &str = "-----END ";
    const PRIVATE: &str = "PRIVATE KEY";

    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(BEGIN) else {
            break;
        };
        let begin_pos = search_from + rel;

        // Check if this is a private key block.
        let header_rest = &text[begin_pos + BEGIN.len()..];
        if !header_rest.starts_with("RSA PRIVATE KEY")
            && !header_rest.starts_with("EC PRIVATE KEY")
            && !header_rest.starts_with("DSA PRIVATE KEY")
            && !header_rest.starts_with(PRIVATE)
        {
            // Not a private key — advance past this BEGIN marker.
            search_from = begin_pos + BEGIN.len();
            continue;
        }

        // Find the end marker.
        let inner_search = begin_pos + BEGIN.len();
        if let Some(end_rel) = text[inner_search..].find(END_PREFIX) {
            let end_pos = inner_search + end_rel;
            // Find the closing `-----` of the END line.
            let end_line_end = text[end_pos..]
                .find("-----\n")
                .or_else(|| text[end_pos..].find("-----\r\n"))
                .map(|i| end_pos + i + 6) // len of "-----\n"
                .unwrap_or(text.len());

            let redaction = "[REDACTED:private-key]";
            text.replace_range(begin_pos..end_line_end, redaction);
            search_from = begin_pos + redaction.len();
        } else {
            search_from = begin_pos + BEGIN.len();
        }
    }
}

/// Redact OpenSSH private key blocks (`-----BEGIN OPENSSH PRIVATE KEY-----`).
fn sanitize_openssh_keys(text: &mut String) {
    const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    const END: &str = "-----END OPENSSH PRIVATE KEY-----";

    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(BEGIN) else {
            break;
        };
        let begin_pos = search_from + rel;

        if let Some(end_rel) = text[begin_pos..].find(END) {
            let end_pos = begin_pos + end_rel + END.len();
            // Consume trailing newline if present.
            let end_pos = if text[end_pos..].starts_with('\n') {
                end_pos + 1
            } else if text[end_pos..].starts_with("\r\n") {
                end_pos + 2
            } else {
                end_pos
            };
            let redaction = "[REDACTED:openssh-private-key]";
            text.replace_range(begin_pos..end_pos, redaction);
            search_from = begin_pos + redaction.len();
        } else {
            search_from = begin_pos + BEGIN.len();
        }
    }
}

/// Redact JWT tokens: three base64url segments separated by dots (eyJ...).
fn sanitize_jwt_tokens(text: &mut String) {
    const JWT_PREFIX: &str = "eyJ";

    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(JWT_PREFIX) else {
            break;
        };
        let start = search_from + rel;

        // A JWT should not be preceded by an alphanumeric character (avoids matching
        // inside words or base64 blobs that happen to contain "eyJ").
        if start > 0 {
            let prev = text.as_bytes()[start - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'-' {
                search_from = start + JWT_PREFIX.len();
                continue;
            }
        }

        // Scan the token: base64url chars plus dots.
        let token_end = text[start..]
            .find(|c: char| {
                !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.' && c != '='
            })
            .map(|i| start + i)
            .unwrap_or(text.len());

        let token = &text[start..token_end];

        // Validate JWT structure: exactly 3 dot-separated parts, each non-empty.
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() == 3 && parts.iter().all(|p| p.len() >= 2) && token.len() >= 20 {
            let redaction = "[REDACTED:jwt]";
            text.replace_range(start..token_end, redaction);
            search_from = start + redaction.len();
        } else {
            search_from = start + JWT_PREFIX.len();
        }
    }
}

/// Redact passwords embedded in database connection URLs.
///
/// Only searches the authority component (before the first `/` after `://`)
/// to avoid false-positive matches in query parameters or path segments.
fn sanitize_database_urls(text: &mut String) {
    for &(scheme, _label) in DB_SCHEMES {
        let mut search_from = 0;
        loop {
            let Some(rel) = text[search_from..].find(scheme) else {
                break;
            };
            let scheme_start = search_from + rel;
            let after_scheme = scheme_start + scheme.len();

            // Find the authority boundary: first `/` after `://` (or end of string).
            let authority_end = text[after_scheme..]
                .find('/')
                .map(|i| after_scheme + i)
                .unwrap_or(text.len());

            let authority = &text[after_scheme..authority_end];

            // Look for user:password@host pattern in the authority.
            if let Some(at_pos) = authority.find('@') {
                let userinfo = &authority[..at_pos];
                if let Some(colon_pos) = userinfo.find(':') {
                    let password = &userinfo[colon_pos + 1..];
                    if !password.is_empty() {
                        // Replace only the password portion.
                        let password_start = after_scheme + colon_pos + 1;
                        let password_end = after_scheme + at_pos;
                        let redaction = "[REDACTED:db-password]";
                        text.replace_range(password_start..password_end, redaction);
                        search_from = password_start + redaction.len();
                        continue;
                    }
                }
            }

            search_from = after_scheme;
        }
    }
}

/// Redact Azure connection string keys (`AccountKey=...`).
fn sanitize_azure_connection_strings(text: &mut String) {
    const MARKER: &str = "AccountKey=";

    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(MARKER) else {
            break;
        };
        let start = search_from + rel;
        let key_start = start + MARKER.len();

        // Azure keys are base64 and end at `;` or whitespace or end-of-string.
        let key_end = text[key_start..]
            .find(|c: char| c == ';' || c.is_ascii_whitespace() || c == '"' || c == '\'')
            .map(|i| key_start + i)
            .unwrap_or(text.len());

        let key_len = key_end - key_start;
        if key_len >= 10 {
            let redaction = "AccountKey=[REDACTED:azure-key]";
            text.replace_range(start..key_end, redaction);
            search_from = start + redaction.len();
        } else {
            search_from = key_start;
        }
    }
}

/// Redact GCP service account private keys in JSON.
///
/// Matches `"private_key": "-----BEGIN...-----END..."` or similar patterns
/// found in service account JSON key files.
fn sanitize_gcp_service_account_keys(text: &mut String) {
    // Look for the JSON key pattern.
    const MARKER: &str = "\"private_key\"";

    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(MARKER) else {
            break;
        };
        let marker_start = search_from + rel;
        let after_marker = marker_start + MARKER.len();

        // Skip optional whitespace and colon.
        let rest = &text[after_marker..];
        let trimmed = rest.trim_start();
        let offset = rest.len() - trimmed.len();

        if !trimmed.starts_with(':') {
            search_from = after_marker;
            continue;
        }
        let after_colon = &trimmed[1..];
        let after_colon_trimmed = after_colon.trim_start();
        let colon_offset = after_colon.len() - after_colon_trimmed.len();

        // Expect a quote-delimited value.
        if !after_colon_trimmed.starts_with('"') {
            search_from = after_marker;
            continue;
        }

        // Find the value start and end.
        let value_content_start =
            after_marker + offset + 1 /* : */ + colon_offset + 1 /* opening " */;

        // Find closing quote (handling escaped quotes).
        let mut pos = value_content_start;
        loop {
            if pos >= text.len() {
                break;
            }
            if text.as_bytes()[pos] == b'\\' {
                pos += 2; // skip escaped char
                continue;
            }
            if text.as_bytes()[pos] == b'"' {
                break;
            }
            pos += 1;
        }
        let value_end = pos; // points at closing quote

        if value_end > value_content_start {
            let value = &text[value_content_start..value_end];
            // Check if this looks like a PEM key.
            if value.contains("BEGIN") && value.contains("PRIVATE") {
                let replace_start = value_content_start;
                let replace_end = value_end;
                let redaction = "[REDACTED:gcp-private-key]";
                text.replace_range(replace_start..replace_end, redaction);
                search_from = replace_start + redaction.len();
                continue;
            }
        }

        search_from = after_marker;
    }
}

/// Redact AWS secret access keys found near contextual markers.
///
/// Looks for patterns like:
///   aws_secret_access_key = XXXX...
///   AWS_SECRET_ACCESS_KEY=XXXX...
///   <SecretAccessKey>XXXX...</SecretAccessKey>
fn sanitize_aws_secret_keys(text: &mut String) {
    // Env/config style: key = value or key=value (case insensitive search).
    for marker in &[
        "aws_secret_access_key",
        "AWS_SECRET_ACCESS_KEY",
        "aws_secret_key",
        "AWS_SECRET_KEY",
    ] {
        let mut search_from = 0;
        loop {
            let Some(rel) = text[search_from..].find(marker) else {
                break;
            };
            let start = search_from + rel;
            let after_marker = start + marker.len();

            // Skip optional whitespace and `=`.
            let rest = &text[after_marker..];
            let trimmed = rest.trim_start();
            let ws_len = rest.len() - trimmed.len();

            if !trimmed.starts_with('=') {
                search_from = after_marker;
                continue;
            }

            let after_eq = &trimmed[1..];
            let after_eq_trimmed = after_eq.trim_start();
            let eq_ws_len = after_eq.len() - after_eq_trimmed.len();

            let value_start = after_marker + ws_len + 1 + eq_ws_len;

            // Strip optional quotes.
            let (actual_start, quote_char) =
                if after_eq_trimmed.starts_with('"') || after_eq_trimmed.starts_with('\'') {
                    (value_start + 1, Some(after_eq_trimmed.as_bytes()[0]))
                } else {
                    (value_start, None)
                };

            // Find end of value.
            let value_end = if let Some(qc) = quote_char {
                text[actual_start..]
                    .find(|c: char| c as u8 == qc)
                    .map(|i| actual_start + i)
                    .unwrap_or(text.len())
            } else {
                text[actual_start..]
                    .find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'' || c == ';')
                    .map(|i| actual_start + i)
                    .unwrap_or(text.len())
            };

            let key_len = value_end - actual_start;
            if key_len >= 16 {
                let redaction = "[REDACTED:aws-secret-key]";
                text.replace_range(actual_start..value_end, redaction);
                search_from = actual_start + redaction.len();
            } else {
                search_from = after_marker;
            }
        }
    }

    // XML style: <SecretAccessKey>VALUE</SecretAccessKey>
    const XML_OPEN: &str = "<SecretAccessKey>";
    const XML_CLOSE: &str = "</SecretAccessKey>";
    let mut search_from = 0;
    loop {
        let Some(rel) = text[search_from..].find(XML_OPEN) else {
            break;
        };
        let tag_start = search_from + rel;
        let value_start = tag_start + XML_OPEN.len();

        if let Some(close_rel) = text[value_start..].find(XML_CLOSE) {
            let value_end = value_start + close_rel;
            if value_end > value_start {
                let redaction = "[REDACTED:aws-secret-key]";
                text.replace_range(value_start..value_end, redaction);
                search_from = value_start + redaction.len();
            } else {
                search_from = value_start;
            }
        } else {
            search_from = value_start;
        }
    }

    // Also handle XML `>` separator: SecretAccessKey>VALUE<
    // This catches malformed or alternate XML-like formats.
    const ALT_OPEN: &str = "SecretAccessKey>";
    let mut search_from_alt = 0;
    loop {
        let Some(rel) = text[search_from_alt..].find(ALT_OPEN) else {
            break;
        };
        let found_pos = search_from_alt + rel;

        // Make sure we haven't already redacted this as the full XML tag above.
        // Check if there's a `<` immediately before.
        if found_pos > 0 && text.as_bytes()[found_pos - 1] == b'<' {
            // This is `<SecretAccessKey>` — already handled above.
            search_from_alt = found_pos + ALT_OPEN.len();
            continue;
        }

        let value_start = found_pos + ALT_OPEN.len();
        // Value ends at `<` or whitespace.
        let value_end = text[value_start..]
            .find(|c: char| c == '<' || c.is_ascii_whitespace())
            .map(|i| value_start + i)
            .unwrap_or(text.len());

        let key_len = value_end - value_start;
        if key_len >= 16 {
            let redaction = "[REDACTED:aws-secret-key]";
            text.replace_range(value_start..value_end, redaction);
            search_from_alt = value_start + redaction.len();
        } else {
            search_from_alt = value_start;
        }
    }
}

/// Redact Heroku API keys in environment variable format (HEROKU_API_KEY=...).
fn sanitize_heroku_env_keys(text: &mut String) {
    for marker in &["HEROKU_API_KEY=", "HEROKU_API_TOKEN="] {
        let mut search_from = 0;
        loop {
            let Some(rel) = text[search_from..].find(marker) else {
                break;
            };
            let start = search_from + rel;
            let value_start = start + marker.len();

            // Strip optional quotes.
            let (actual_start, quote_char) =
                if text[value_start..].starts_with('"') || text[value_start..].starts_with('\'') {
                    (
                        value_start + 1,
                        Some(text.as_bytes().get(value_start).copied().unwrap_or(0)),
                    )
                } else {
                    (value_start, None)
                };

            let value_end = if let Some(qc) = quote_char {
                text[actual_start..]
                    .find(|c: char| c as u8 == qc)
                    .map(|i| actual_start + i)
                    .unwrap_or(text.len())
            } else {
                text[actual_start..]
                    .find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'')
                    .map(|i| actual_start + i)
                    .unwrap_or(text.len())
            };

            let key_len = value_end - actual_start;
            if key_len >= 10 {
                let redaction = "[REDACTED:heroku-key]";
                text.replace_range(actual_start..value_end, redaction);
                search_from = actual_start + redaction.len();
            } else {
                search_from = value_start;
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
        let mut search_from = 0;
        loop {
            let Some(rel) = text[search_from..].find(pat.prefix) else {
                break;
            };
            let pos = search_from + rel;
            let after = pos + pat.prefix.len();
            let suffix_end = text[after..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.')
                .map(|i| after + i)
                .unwrap_or(text.len());
            if suffix_end - after >= pat.min_suffix_len {
                return true;
            }
            // Advance past this prefix to check for later occurrences.
            search_from = after;
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

    // Check OpenSSH private keys.
    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        return true;
    }

    // Check JWT tokens (eyJ prefix with dot-separated structure).
    if text.contains("eyJ") {
        return true;
    }

    // Check database URLs with passwords.
    for &(scheme, _) in DB_SCHEMES {
        if text.contains(scheme) {
            // Quick check for user:pass@host pattern.
            if let Some(pos) = text.find(scheme) {
                let after = pos + scheme.len();
                let authority_end = text[after..]
                    .find('/')
                    .map(|i| after + i)
                    .unwrap_or(text.len());
                let authority = &text[after..authority_end];
                if authority.contains('@') && authority.contains(':') {
                    return true;
                }
            }
        }
    }

    // Check Azure connection strings.
    if text.contains("AccountKey=") {
        return true;
    }

    // Check GCP service account keys.
    if text.contains("\"private_key\"") && text.contains("BEGIN") {
        return true;
    }

    // Check AWS secret keys (contextual markers).
    for marker in &[
        "aws_secret_access_key",
        "AWS_SECRET_ACCESS_KEY",
        "aws_secret_key",
        "AWS_SECRET_KEY",
        "<SecretAccessKey>",
        "SecretAccessKey>",
    ] {
        if text.contains(marker) {
            return true;
        }
    }

    // Check Heroku env keys.
    if text.contains("HEROKU_API_KEY=") || text.contains("HEROKU_API_TOKEN=") {
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
        assert!(
            result.contains("[REDACTED:bearer-token]") || result.contains("[REDACTED:jwt]"),
            "bearer or jwt should be redacted"
        );
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

    // ── New pattern tests ──────────────────────────────────────────────

    #[test]
    fn redacts_postgresql_url_password() {
        let input = "DATABASE_URL=postgresql://admin:s3cret_p4ss@db.example.com:5432/mydb";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:db-password]"),
            "postgresql password should be redacted, got: {result}"
        );
        assert!(!result.contains("s3cret_p4ss"), "password should be gone");
        assert!(result.contains("admin:"), "username should remain");
        assert!(
            result.contains("@db.example.com"),
            "host should remain, got: {result}"
        );
    }

    #[test]
    fn redacts_postgres_url_password() {
        let input = "postgres://user:password123@localhost/db";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:db-password]"));
        assert!(!result.contains("password123"));
    }

    #[test]
    fn redacts_mongodb_srv_url_password() {
        let input =
            "MONGO_URI=mongodb+srv://appuser:hunter2@cluster0.abc123.mongodb.net/prod?retryWrites=true";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:db-password]"),
            "mongodb+srv password should be redacted, got: {result}"
        );
        assert!(!result.contains("hunter2"));
    }

    #[test]
    fn redacts_mongodb_url_password() {
        let input = "mongodb://root:mongopass@mongo:27017/admin";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:db-password]"));
        assert!(!result.contains("mongopass"));
    }

    #[test]
    fn redacts_redis_url_password() {
        let input = "REDIS_URL=redis://default:redis_secret@cache.example.com:6379/0";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:db-password]"),
            "redis password should be redacted, got: {result}"
        );
        assert!(!result.contains("redis_secret"));
    }

    #[test]
    fn redacts_mysql_url_password() {
        let input = "mysql://root:my_sql_pass@127.0.0.1:3306/app";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:db-password]"),
            "mysql password should be redacted, got: {result}"
        );
        assert!(!result.contains("my_sql_pass"));
    }

    #[test]
    fn preserves_db_url_without_password() {
        let input = "postgresql://localhost:5432/mydb";
        let result = sanitize_credentials(input);
        assert_eq!(result, input, "URL without password should be unchanged");
    }

    #[test]
    fn db_url_password_scoped_to_authority() {
        // The password search should not extend past the authority component.
        let input = "postgresql://user:pass@host/db?option=val:ue@thing";
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:db-password]"));
        assert!(
            result.contains("option=val:ue@thing"),
            "query string should not be modified, got: {result}"
        );
    }

    #[test]
    fn redacts_jwt_token() {
        let input = "token: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:jwt]"),
            "JWT should be redacted, got: {result}"
        );
        assert!(!result.contains("eyJhbGci"));
    }

    #[test]
    fn jwt_must_have_three_parts() {
        // Only two dot-separated parts — should NOT be redacted as JWT.
        let input = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0";
        let result = sanitize_credentials(input);
        assert!(
            !result.contains("[REDACTED:jwt]"),
            "two-part token should not be redacted as JWT"
        );
    }

    #[test]
    fn jwt_not_matched_inside_word() {
        // "eyJ" preceded by alphanumeric should not trigger JWT redaction.
        let input = "dataeyJhbGciOiJIUzI1NiJ9.payload.signature";
        let result = sanitize_credentials(input);
        assert!(
            !result.contains("[REDACTED:jwt]"),
            "eyJ preceded by alpha should not be a JWT"
        );
    }

    #[test]
    fn redacts_openssh_private_key() {
        let input = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAA...\n-----END OPENSSH PRIVATE KEY-----\n";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:openssh-private-key]"),
            "OpenSSH key should be redacted, got: {result}"
        );
        assert!(!result.contains("b3Blbn"));
    }

    #[test]
    fn redacts_ec_private_key() {
        let input = "-----BEGIN EC PRIVATE KEY-----\nMHQCAQEE...\n-----END EC PRIVATE KEY-----\n";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:private-key]"),
            "EC private key should be redacted, got: {result}"
        );
    }

    #[test]
    fn redacts_azure_connection_string() {
        let input = "DefaultEndpointsProtocol=https;AccountName=myaccount;AccountKey=abcdefghijklmnopqrstuvwxyz1234567890ABCDEFGHIJKLMNOP==;EndpointSuffix=core.windows.net";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:azure-key]"),
            "Azure AccountKey should be redacted, got: {result}"
        );
        assert!(!result.contains("abcdefghijklmnopqrstuvwxyz1234567890"));
        assert!(
            result.contains("AccountName=myaccount"),
            "account name should remain"
        );
    }

    #[test]
    fn redacts_gcp_service_account_key() {
        let input = r#"{"type": "service_account", "private_key": "-----BEGIN RSA PRIVATE KEY-----\nMIIEow...\n-----END RSA PRIVATE KEY-----\n"}"#;
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:gcp-private-key]"),
            "GCP private_key should be redacted, got: {result}"
        );
        assert!(!result.contains("MIIEow"));
    }

    #[test]
    fn redacts_aws_secret_access_key_env() {
        let input = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:aws-secret-key]"),
            "AWS secret key should be redacted, got: {result}"
        );
        assert!(!result.contains("wJalrXU"));
    }

    #[test]
    fn redacts_aws_secret_access_key_config() {
        let input = "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:aws-secret-key]"),
            "AWS secret key (config style) should be redacted, got: {result}"
        );
    }

    #[test]
    fn redacts_aws_secret_key_xml() {
        let input =
            "<Credentials><SecretAccessKey>wJalrXUtnFEMI/K7MDENG/bPxRfiCY</SecretAccessKey></Credentials>";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:aws-secret-key]"),
            "AWS secret key (XML) should be redacted, got: {result}"
        );
        assert!(!result.contains("wJalrXU"));
    }

    #[test]
    fn redacts_digitalocean_token() {
        let input = "DO_TOKEN=dop_v1_abcdef1234567890abcdef1234567890abcdef12";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:digitalocean-token]"),
            "DigitalOcean token should be redacted, got: {result}"
        );
        assert!(!result.contains("dop_v1_"));
    }

    #[test]
    fn redacts_heroku_api_key_prefix_pattern() {
        // Using the prefix-based pattern.
        let input = "config: heroku_api_key=12345678-abcd-efgh-ijkl-1234567890abcdef";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:heroku-key]"),
            "Heroku key (prefix pattern) should be redacted, got: {result}"
        );
    }

    #[test]
    fn redacts_heroku_env_key() {
        let input = "HEROKU_API_KEY=12345678-abcd-efgh-ijkl-1234567890ab";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:heroku-key]"),
            "HEROKU_API_KEY env should be redacted, got: {result}"
        );
    }

    #[test]
    fn search_from_skips_short_match_finds_later_real_token() {
        // First "sk-" is too short to be a real key, second is long enough.
        let input = "sk-short then sk-proj-ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:api-key]"),
            "second sk- should be redacted, got: {result}"
        );
        assert!(result.contains("sk-short"), "short sk- should remain");
    }

    #[test]
    fn multiple_bearer_tokens() {
        let input = "Bearer abcdefghij and Bearer xyzxyzxyzxyz";
        let result = sanitize_credentials(input);
        // Both should be redacted.
        let count = result.matches("[REDACTED:bearer-token]").count();
        assert_eq!(
            count, 2,
            "both bearer tokens should be redacted, got: {result}"
        );
    }

    #[test]
    fn multiple_pem_blocks() {
        let input = "-----BEGIN RSA PRIVATE KEY-----\nkey1\n-----END RSA PRIVATE KEY-----\nstuff\n-----BEGIN EC PRIVATE KEY-----\nkey2\n-----END EC PRIVATE KEY-----\n";
        let result = sanitize_credentials(input);
        let count = result.matches("[REDACTED:private-key]").count();
        assert_eq!(
            count, 2,
            "both PEM blocks should be redacted, got: {result}"
        );
    }

    #[test]
    fn contains_credentials_detects_new_patterns() {
        assert!(contains_credentials("postgresql://user:pass@host/db"));
        assert!(contains_credentials("AccountKey=longbase64key"));
        assert!(contains_credentials("AWS_SECRET_ACCESS_KEY=something"));
        assert!(contains_credentials("HEROKU_API_KEY=something"));
        assert!(contains_credentials("eyJhbGciOiJIUzI1NiJ9.payload.sig"));
        assert!(contains_credentials("-----BEGIN OPENSSH PRIVATE KEY-----"));
    }

    #[test]
    fn preserves_db_url_without_at_sign() {
        // No @ means no userinfo — should not redact.
        let input = "redis://localhost:6379/0";
        let result = sanitize_credentials(input);
        assert_eq!(result, input);
    }

    #[test]
    fn azure_key_short_value_not_redacted() {
        let input = "AccountKey=short";
        let result = sanitize_credentials(input);
        assert!(
            !result.contains("[REDACTED:azure-key]"),
            "short azure key should not be redacted"
        );
    }

    #[test]
    fn heroku_token_variant() {
        let input = "HEROKU_API_TOKEN=abcdefghij1234567890abcdefghij1234567890";
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:heroku-key]"),
            "HEROKU_API_TOKEN should be redacted, got: {result}"
        );
    }

    #[test]
    fn gcp_key_only_redacts_private_key_value() {
        let input = r#"{"type": "service_account", "private_key": "-----BEGIN PRIVATE KEY-----\ndata\n-----END PRIVATE KEY-----\n", "client_email": "test@proj.iam.gserviceaccount.com"}"#;
        let result = sanitize_credentials(input);
        assert!(result.contains("[REDACTED:gcp-private-key]"));
        assert!(
            result.contains("client_email"),
            "other fields should remain, got: {result}"
        );
    }

    #[test]
    fn aws_secret_key_with_quotes() {
        let input = r#"aws_secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY""#;
        let result = sanitize_credentials(input);
        assert!(
            result.contains("[REDACTED:aws-secret-key]"),
            "quoted AWS secret key should be redacted, got: {result}"
        );
    }

    #[test]
    fn multiple_db_urls_in_one_string() {
        let input = "primary=postgresql://u1:p1@h1/db1 replica=postgresql://u2:p2@h2/db2";
        let result = sanitize_credentials(input);
        let count = result.matches("[REDACTED:db-password]").count();
        assert_eq!(
            count, 2,
            "both db passwords should be redacted, got: {result}"
        );
        assert!(!result.contains("p1") || !result.contains(":p1@"));
        assert!(!result.contains("p2") || !result.contains(":p2@"));
    }
}
