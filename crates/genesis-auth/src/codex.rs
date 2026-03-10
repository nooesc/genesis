use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio::sync::RwLock;

use crate::error::AuthError;
use crate::jwt;
use crate::provider::{
    CODEX_DEVICE_CODE_URL, CODEX_DEVICE_POLL_URL, CODEX_DEVICE_VERIFY_URL, CODEX_INFERENCE_URL,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, CODEX_PROVIDER_ID, CODEX_REDIRECT_URI,
};
use crate::store::{self, CodexTokens};

const DEVICE_CODE_POLL_INTERVAL_SECS: u64 = 5;
const DEVICE_CODE_TIMEOUT_MINS: u32 = 15;
const TOKEN_REFRESH_SKEW_SECS: i64 = 120;
const TOKEN_REFRESH_TIMEOUT_SECS: u64 = 15;
const CACHE_TTL_SECS: u64 = 60;

struct CachedEntry {
    creds: ResolvedCredentials,
    cached_at: std::time::Instant,
}

static CREDENTIALS_CACHE: OnceLock<RwLock<HashMap<PathBuf, CachedEntry>>> = OnceLock::new();

fn cache() -> &'static RwLock<HashMap<PathBuf, CachedEntry>> {
    CREDENTIALS_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Read the Codex base URL, allowing override via `GENESIS_CODEX_BASE_URL` env var.
fn codex_base_url() -> String {
    std::env::var("GENESIS_CODEX_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| CODEX_INFERENCE_URL.to_owned())
}

/// Credentials resolved from the auth store, ready for API calls.
#[derive(Debug, Clone)]
pub struct ResolvedCredentials {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub source: String,
}

/// Result from the device code request step.
#[derive(Debug)]
pub struct DeviceCodeResponse {
    pub user_code: String,
    pub device_auth_id: String,
    pub poll_interval: u64,
    pub verification_url: String,
}

/// Check if running in an SSH/remote session where browser can't be opened.
pub fn is_remote_session() -> bool {
    std::env::var("SSH_CLIENT").is_ok() || std::env::var("SSH_TTY").is_ok()
}

/// Try to import tokens from the Codex CLI auth store (~/.codex/auth.json).
/// Uses `CODEX_HOME` env var or defaults to `~/.codex`.
pub fn import_codex_cli_tokens() -> Option<CodexTokens> {
    let codex_home = std::env::var("CODEX_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".codex").to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    if codex_home.is_empty() {
        return None;
    }
    import_codex_cli_tokens_from(&PathBuf::from(&codex_home))
}

/// Try to import tokens from a specific Codex CLI directory.
pub fn import_codex_cli_tokens_from(codex_dir: &Path) -> Option<CodexTokens> {
    let auth_path = codex_dir.join("auth.json");
    let contents = std::fs::read_to_string(&auth_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let tokens = parsed.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?.to_owned();
    let refresh_token = tokens.get("refresh_token")?.as_str()?.to_owned();
    if access_token.is_empty() || refresh_token.is_empty() {
        return None;
    }
    Some(CodexTokens {
        access_token,
        refresh_token,
    })
}

/// Request a device code from OpenAI's auth endpoint.
pub async fn request_device_code(
    client: &reqwest::Client,
) -> Result<DeviceCodeResponse, AuthError> {
    let resp = client
        .post(CODEX_DEVICE_CODE_URL)
        .json(&serde_json::json!({"client_id": CODEX_OAUTH_CLIENT_ID}))
        .send()
        .await
        .map_err(|e| AuthError::DeviceCodeRequest {
            message: e.to_string(),
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::DeviceCodeRequest {
            message: format!("status {status}: {body}"),
        });
    }
    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AuthError::DeviceCodeRequest {
            message: e.to_string(),
        })?;
    let user_code = data["user_code"]
        .as_str()
        .ok_or_else(|| AuthError::DeviceCodeRequest {
            message: "missing user_code".to_owned(),
        })?
        .to_owned();
    let device_auth_id = data["device_auth_id"]
        .as_str()
        .ok_or_else(|| AuthError::DeviceCodeRequest {
            message: "missing device_auth_id".to_owned(),
        })?
        .to_owned();
    let poll_interval = data["interval"]
        .as_u64()
        .unwrap_or(DEVICE_CODE_POLL_INTERVAL_SECS)
        .max(3);
    Ok(DeviceCodeResponse {
        user_code,
        device_auth_id,
        poll_interval,
        verification_url: CODEX_DEVICE_VERIFY_URL.to_owned(),
    })
}

/// Poll OpenAI's device auth endpoint until the user completes sign-in.
pub async fn poll_for_authorization(
    client: &reqwest::Client,
    device_auth_id: &str,
    user_code: &str,
    poll_interval: u64,
) -> Result<(String, String), AuthError> {
    let max_wait = std::time::Duration::from_secs(DEVICE_CODE_TIMEOUT_MINS as u64 * 60);
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= max_wait {
            return Err(AuthError::LoginTimeout {
                minutes: DEVICE_CODE_TIMEOUT_MINS,
            });
        }
        tokio::time::sleep(std::time::Duration::from_secs(poll_interval)).await;
        let resp = client
            .post(CODEX_DEVICE_POLL_URL)
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await
            .map_err(|e| AuthError::DeviceCodeRequest {
                message: e.to_string(),
            })?;
        match resp.status().as_u16() {
            200 => {
                let data: serde_json::Value =
                    resp.json().await.map_err(|e| AuthError::DeviceCodeRequest {
                        message: e.to_string(),
                    })?;
                let authorization_code = data["authorization_code"]
                    .as_str()
                    .ok_or_else(|| AuthError::TokenExchange {
                        message: "missing authorization_code".to_owned(),
                    })?
                    .to_owned();
                let code_verifier = data["code_verifier"]
                    .as_str()
                    .ok_or_else(|| AuthError::TokenExchange {
                        message: "missing code_verifier".to_owned(),
                    })?
                    .to_owned();
                return Ok((authorization_code, code_verifier));
            }
            403 | 404 => continue,
            status => {
                return Err(AuthError::DeviceCodeRequest {
                    message: format!("poll returned status {status}"),
                });
            }
        }
    }
}

/// Exchange an authorization code for access and refresh tokens.
pub async fn exchange_code_for_tokens(
    client: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
) -> Result<CodexTokens, AuthError> {
    let resp = client
        .post(CODEX_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code),
            ("redirect_uri", CODEX_REDIRECT_URI),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| AuthError::TokenExchange {
            message: e.to_string(),
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::TokenExchange {
            message: format!("status {status}: {body}"),
        });
    }
    let data: serde_json::Value =
        resp.json()
            .await
            .map_err(|e| AuthError::TokenExchange {
                message: e.to_string(),
            })?;
    let access_token = data["access_token"]
        .as_str()
        .ok_or_else(|| AuthError::TokenExchange {
            message: "missing access_token".to_owned(),
        })?
        .to_owned();
    let refresh_token = data["refresh_token"]
        .as_str()
        .unwrap_or("")
        .to_owned();

    if refresh_token.is_empty() {
        tracing::warn!("Token exchange returned no refresh_token — token refresh will not work");
    }

    Ok(CodexTokens {
        access_token,
        refresh_token,
    })
}

/// Refresh the access token using the refresh token.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<CodexTokens, AuthError> {
    let resp = client
        .post(CODEX_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| AuthError::TokenRefresh {
            message: e.to_string(),
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::TokenRefresh {
            message: format!("status {status}: {body}"),
        });
    }
    let data: serde_json::Value =
        resp.json()
            .await
            .map_err(|e| AuthError::TokenRefresh {
                message: e.to_string(),
            })?;
    let access_token = data["access_token"]
        .as_str()
        .ok_or_else(|| AuthError::TokenRefresh {
            message: "missing access_token in refresh response".to_owned(),
        })?
        .to_owned();
    let new_refresh = data["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token)
        .to_owned();
    Ok(CodexTokens {
        access_token,
        refresh_token: new_refresh,
    })
}

/// Resolve Codex credentials from the auth store, refreshing if needed.
///
/// Uses an in-process cache with a 60-second TTL to avoid repeated disk reads.
/// The cache also checks that the token is not about to expire; if it is, the
/// cache is bypassed so the token can be refreshed.
pub async fn resolve_credentials(auth_store_path: &Path) -> Result<ResolvedCredentials, AuthError> {
    // Fast path: check cache under read lock.
    {
        let guard = cache().read().await;
        if let Some(entry) = guard.get(auth_store_path) {
            if cache_hit(entry) {
                return Ok(entry.creds.clone());
            }
        }
    }

    // Slow path: acquire write lock, double-check, then resolve.
    // Holding the write lock across the refresh prevents thundering herd
    // (multiple concurrent callers all triggering parallel token refreshes).
    let mut guard = cache().write().await;
    if let Some(entry) = guard.get(auth_store_path) {
        if cache_hit(entry) {
            return Ok(entry.creds.clone());
        }
    }

    let creds = resolve_credentials_inner(auth_store_path).await?;
    guard.insert(
        auth_store_path.to_path_buf(),
        CachedEntry {
            creds: creds.clone(),
            cached_at: std::time::Instant::now(),
        },
    );

    Ok(creds)
}

fn cache_hit(entry: &CachedEntry) -> bool {
    entry.cached_at.elapsed().as_secs() < CACHE_TTL_SECS
        && !jwt::is_expiring(&entry.creds.api_key, TOKEN_REFRESH_SKEW_SECS)
}

/// Inner implementation that reads from disk and optionally refreshes the token.
async fn resolve_credentials_inner(
    auth_store_path: &Path,
) -> Result<ResolvedCredentials, AuthError> {
    let store = store::read_store(auth_store_path)?;
    let codex = store::get_codex_state(&store).ok_or(AuthError::NotLoggedIn)?;
    let access_token = &codex.tokens.access_token;
    let base_url = codex_base_url();

    // TODO: Add fd-lock file locking around token refresh to prevent concurrent
    // processes from racing on the same auth store. See design doc for the
    // lock-read-recheck-refresh-write-unlock protocol.
    if jwt::is_expiring(access_token, TOKEN_REFRESH_SKEW_SECS) {
        tracing::debug!("Codex access token expiring, attempting refresh");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TOKEN_REFRESH_TIMEOUT_SECS))
            .build()
            .map_err(AuthError::Http)?;
        match refresh_access_token(&client, &codex.tokens.refresh_token).await {
            Ok(new_tokens) => {
                let api_key = new_tokens.access_token.clone();
                store::save_codex_tokens(auth_store_path, new_tokens, &codex.source)?;
                return Ok(ResolvedCredentials {
                    provider: CODEX_PROVIDER_ID.to_owned(),
                    base_url,
                    api_key,
                    source: "auth-store".to_owned(),
                });
            }
            Err(e) => {
                tracing::warn!("Token refresh failed, using existing token: {e}");
            }
        }
    }

    Ok(ResolvedCredentials {
        provider: CODEX_PROVIDER_ID.to_owned(),
        base_url,
        api_key: access_token.clone(),
        source: "auth-store".to_owned(),
    })
}

/// Run the full interactive device code login flow.
pub async fn login(auth_store_path: &Path) -> Result<ResolvedCredentials, AuthError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(AuthError::Http)?;

    let device = request_device_code(&client).await?;

    eprintln!();
    eprintln!("To continue, follow these steps:");
    eprintln!();
    eprintln!("  1. Open this URL in your browser:");
    eprintln!("     \x1b[94m{}\x1b[0m", device.verification_url);
    eprintln!();
    eprintln!("  2. Enter this code:");
    eprintln!("     \x1b[94m{}\x1b[0m", device.user_code);
    eprintln!();
    eprintln!("Waiting for sign-in... (press Ctrl+C to cancel)");

    if !is_remote_session() {
        if let Err(e) = open::that(&device.verification_url) {
            tracing::debug!("Could not open browser: {e}");
        }
    }

    let (authorization_code, code_verifier) = poll_for_authorization(
        &client,
        &device.device_auth_id,
        &device.user_code,
        device.poll_interval,
    )
    .await?;

    let tokens = exchange_code_for_tokens(&client, &authorization_code, &code_verifier).await?;
    let api_key = tokens.access_token.clone();
    store::save_codex_tokens(auth_store_path, tokens, "device-code")?;

    Ok(ResolvedCredentials {
        provider: CODEX_PROVIDER_ID.to_owned(),
        base_url: codex_base_url(),
        api_key,
        source: "device-code".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn is_remote_session_does_not_panic() {
        let _ = is_remote_session();
    }

    #[test]
    fn import_codex_cli_tokens_reads_valid_file() {
        let dir = tempdir().unwrap();
        let codex_dir = dir.path();
        let auth_json = serde_json::json!({
            "tokens": {
                "access_token": "codex-at",
                "refresh_token": "codex-rt"
            }
        });
        std::fs::write(
            codex_dir.join("auth.json"),
            serde_json::to_string(&auth_json).unwrap(),
        )
        .unwrap();

        let tokens = import_codex_cli_tokens_from(codex_dir).unwrap();
        assert_eq!(tokens.access_token, "codex-at");
        assert_eq!(tokens.refresh_token, "codex-rt");
    }

    #[test]
    fn import_codex_cli_tokens_returns_none_for_missing_file() {
        let result = import_codex_cli_tokens_from(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }

    #[test]
    fn import_codex_cli_tokens_returns_none_for_empty_tokens() {
        let dir = tempdir().unwrap();
        let codex_dir = dir.path();
        let auth_json = serde_json::json!({
            "tokens": { "access_token": "", "refresh_token": "rt" }
        });
        std::fs::write(
            codex_dir.join("auth.json"),
            serde_json::to_string(&auth_json).unwrap(),
        )
        .unwrap();
        let result = import_codex_cli_tokens_from(codex_dir);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_credentials_returns_not_logged_in_when_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let result = resolve_credentials(&path).await;
        assert!(matches!(result, Err(AuthError::NotLoggedIn)));
    }

    #[tokio::test]
    async fn resolve_credentials_returns_stored_token() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        // Create a token with far-future expiry
        let claims = serde_json::json!({"exp": 9999999999_u64});
        let header = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            r#"{"alg":"RS256"}"#,
        );
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            serde_json::to_string(&claims).unwrap(),
        );
        let fake_jwt = format!("{header}.{payload}.sig");
        store::save_codex_tokens(
            &path,
            CodexTokens {
                access_token: fake_jwt.clone(),
                refresh_token: "rt".to_owned(),
            },
            "test",
        )
        .unwrap();
        let creds = resolve_credentials(&path).await.unwrap();
        assert_eq!(creds.provider, CODEX_PROVIDER_ID);
        assert_eq!(creds.api_key, fake_jwt);
        assert_eq!(creds.base_url, CODEX_INFERENCE_URL);
    }

    #[tokio::test]
    async fn resolve_credentials_serves_from_cache_after_file_deleted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth_cache_test.json");

        // Create a token with far-future expiry
        let claims = serde_json::json!({"exp": 9999999999_u64});
        let header = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            r#"{"alg":"RS256"}"#,
        );
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            serde_json::to_string(&claims).unwrap(),
        );
        let fake_jwt = format!("{header}.{payload}.sig");

        store::save_codex_tokens(
            &path,
            CodexTokens {
                access_token: fake_jwt.clone(),
                refresh_token: "rt".to_owned(),
            },
            "test",
        )
        .unwrap();

        // First call: resolves from disk and populates cache
        let creds1 = resolve_credentials(&path).await.unwrap();
        assert_eq!(creds1.api_key, fake_jwt);

        // Delete the file
        std::fs::remove_file(&path).unwrap();

        // Second call: should succeed from cache even though file is gone
        let creds2 = resolve_credentials(&path).await.unwrap();
        assert_eq!(creds2.api_key, fake_jwt);
        assert_eq!(creds2.provider, CODEX_PROVIDER_ID);
        assert_eq!(creds2.source, "auth-store");
    }
}
