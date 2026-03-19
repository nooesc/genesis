use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provider::CODEX_PROVIDER_ID;
use crate::AuthError;

const AUTH_STORE_VERSION: u32 = 1;
const AUTH_FILE_NAME: &str = "auth.json";

/// How the credential was obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialSource {
    DeviceCode,
    CodexMigration,
    #[cfg(test)]
    Test,
}

/// Authentication mode for stored credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    Chatgpt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStore {
    pub version: u32,
    pub active_provider: Option<String>,
    pub updated_at: String,
    pub providers: HashMap<String, ProviderState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProviderState {
    #[serde(rename = "codex")]
    Codex(CodexState),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexState {
    pub tokens: CodexTokens,
    pub last_refresh: Option<String>,
    pub auth_mode: AuthMode,
    pub source: CredentialSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokens {
    pub access_token: String,
    pub refresh_token: String,
}

impl Default for AuthStore {
    fn default() -> Self {
        Self {
            version: AUTH_STORE_VERSION,
            active_provider: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
            providers: HashMap::new(),
        }
    }
}

/// Return the default auth store directory (platform data dir + "genesis").
pub fn default_auth_dir() -> Result<PathBuf, AuthError> {
    let base = dirs::data_dir().ok_or_else(|| {
        AuthError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not determine data directory",
        ))
    })?;
    Ok(base.join("genesis"))
}

/// Return the default auth store file path.
pub fn default_auth_path() -> Result<PathBuf, AuthError> {
    Ok(default_auth_dir()?.join(AUTH_FILE_NAME))
}

/// Read the auth store from disk. Returns default if file doesn't exist.
pub fn read_store(path: &Path) -> Result<AuthStore, AuthError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(serde_json::from_str(&contents)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AuthStore::default()),
        Err(e) => Err(AuthError::Io(e)),
    }
}

/// Write the auth store to disk with restricted permissions (0600 on Unix).
///
/// On Unix, the file is created with mode 0600 from the start to avoid
/// a window where tokens are world-readable at the default umask.
pub fn write_store(path: &Path, store: &AuthStore) -> Result<(), AuthError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(store)?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(json.as_bytes())?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, &json)?;
    }

    Ok(())
}

/// Save Codex tokens into the auth store, setting it as the active provider.
pub fn save_codex_tokens(
    path: &Path,
    tokens: CodexTokens,
    source: CredentialSource,
) -> Result<(), AuthError> {
    let mut store = read_store(path)?;
    store.active_provider = Some(CODEX_PROVIDER_ID.to_owned());
    let now = chrono::Utc::now().to_rfc3339();
    store.providers.insert(
        CODEX_PROVIDER_ID.to_owned(),
        ProviderState::Codex(CodexState {
            tokens,
            last_refresh: Some(now.clone()),
            auth_mode: AuthMode::Chatgpt,
            source,
        }),
    );
    store.updated_at = now;
    write_store(path, &store)
}

/// Remove the active provider's credentials from the store.
pub fn clear_active_provider(path: &Path) -> Result<Option<String>, AuthError> {
    let mut store = read_store(path)?;
    let removed = store.active_provider.take();
    if let Some(ref provider_id) = removed {
        store.providers.remove(provider_id);
        store.updated_at = chrono::Utc::now().to_rfc3339();
        write_store(path, &store)?;
    }
    Ok(removed)
}

/// Get the Codex state from the store, if it exists.
pub fn get_codex_state(store: &AuthStore) -> Option<&CodexState> {
    match store.providers.get(CODEX_PROVIDER_ID) {
        Some(ProviderState::Codex(state)) => Some(state),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_store_returns_default_when_file_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = read_store(&path).unwrap();
        assert_eq!(store.version, AUTH_STORE_VERSION);
        assert!(store.active_provider.is_none());
        assert!(store.providers.is_empty());
    }

    #[test]
    fn write_and_read_store_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut store = AuthStore {
            active_provider: Some(CODEX_PROVIDER_ID.to_owned()),
            ..Default::default()
        };
        store.providers.insert(
            CODEX_PROVIDER_ID.to_owned(),
            ProviderState::Codex(CodexState {
                tokens: CodexTokens {
                    access_token: "access-123".to_owned(),
                    refresh_token: "refresh-456".to_owned(),
                },
                last_refresh: Some("2026-01-01T00:00:00Z".to_owned()),
                auth_mode: AuthMode::Chatgpt,
                source: CredentialSource::DeviceCode,
            }),
        );
        write_store(&path, &store).unwrap();
        let loaded = read_store(&path).unwrap();
        assert_eq!(loaded.active_provider, Some(CODEX_PROVIDER_ID.to_owned()));
        let codex = get_codex_state(&loaded).unwrap();
        assert_eq!(codex.tokens.access_token, "access-123");
        assert_eq!(codex.source, CredentialSource::DeviceCode);
    }

    #[test]
    fn save_codex_tokens_creates_store_and_sets_active() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save_codex_tokens(
            &path,
            CodexTokens {
                access_token: "at".to_owned(),
                refresh_token: "rt".to_owned(),
            },
            CredentialSource::Test,
        )
        .unwrap();
        let store = read_store(&path).unwrap();
        assert_eq!(store.active_provider, Some(CODEX_PROVIDER_ID.to_owned()));
        let codex = get_codex_state(&store).unwrap();
        assert_eq!(codex.tokens.access_token, "at");
        assert_eq!(codex.auth_mode, AuthMode::Chatgpt);
    }

    #[test]
    fn clear_active_provider_removes_credentials() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save_codex_tokens(
            &path,
            CodexTokens {
                access_token: "at".to_owned(),
                refresh_token: "rt".to_owned(),
            },
            CredentialSource::Test,
        )
        .unwrap();
        let removed = clear_active_provider(&path).unwrap();
        assert_eq!(removed, Some(CODEX_PROVIDER_ID.to_owned()));
        let store = read_store(&path).unwrap();
        assert!(store.active_provider.is_none());
        assert!(store.providers.is_empty());
    }

    #[test]
    fn clear_active_provider_returns_none_when_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let removed = clear_active_provider(&path).unwrap();
        assert!(removed.is_none());
    }

    #[test]
    fn get_codex_state_returns_none_when_not_present() {
        let store = AuthStore::default();
        assert!(get_codex_state(&store).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn write_store_sets_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = AuthStore::default();
        write_store(&path, &store).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn credential_source_serializes_as_kebab_case() {
        let json = serde_json::to_string(&CredentialSource::DeviceCode).unwrap();
        assert_eq!(json, "\"device-code\"");
        let json = serde_json::to_string(&CredentialSource::CodexMigration).unwrap();
        assert_eq!(json, "\"codex-migration\"");
    }

    #[test]
    fn auth_mode_serializes_as_kebab_case() {
        let json = serde_json::to_string(&AuthMode::Chatgpt).unwrap();
        assert_eq!(json, "\"chatgpt\"");
        let parsed: AuthMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, AuthMode::Chatgpt);
    }

    #[test]
    fn legacy_auth_json_deserializes_with_enums() {
        // Verify that auth.json written by older versions (with string fields)
        // deserializes correctly into the new enum types.
        let legacy_json = r#"{
            "version": 1,
            "active_provider": "openai-codex",
            "updated_at": "2026-01-01T00:00:00Z",
            "providers": {
                "openai-codex": {
                    "type": "codex",
                    "tokens": {
                        "access_token": "at",
                        "refresh_token": "rt"
                    },
                    "last_refresh": "2026-01-01T00:00:00Z",
                    "auth_mode": "chatgpt",
                    "source": "device-code"
                }
            }
        }"#;
        let store: AuthStore =
            serde_json::from_str(legacy_json).expect("should parse legacy format");
        let codex = get_codex_state(&store).unwrap();
        assert_eq!(codex.auth_mode, AuthMode::Chatgpt);
        assert_eq!(codex.source, CredentialSource::DeviceCode);
        assert_eq!(codex.tokens.access_token, "at");
    }
}
