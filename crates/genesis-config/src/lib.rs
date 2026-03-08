use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const APP_DIR_NAME: &str = "genesis";
const DEFAULT_PROFILE: &str = "default";
const DEFAULT_PROVIDER_BACKEND: &str = "openai";
const DEFAULT_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_DATABASE_FILE: &str = "genesis.db";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisConfig {
    pub schema_version: u32,
    pub profile: String,
    pub provider: ProviderConfig,
    pub storage: StorageConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub backend: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub max_concurrency: usize,
    pub allow_destructive_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: GenesisConfig,
    pub paths: AppPaths,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FileConfig {
    schema_version: Option<u32>,
    profile: Option<String>,
    provider: Option<FileProviderConfig>,
    storage: Option<FileStorageConfig>,
    runtime: Option<FileRuntimeConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FileProviderConfig {
    backend: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FileStorageConfig {
    data_dir: Option<PathBuf>,
    database_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct FileRuntimeConfig {
    max_concurrency: Option<usize>,
    allow_destructive_tools: Option<bool>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine a configuration directory for genesis")]
    MissingConfigDirectory,
    #[error("could not determine a data directory for genesis")]
    MissingDataDirectory,
    #[error("failed to read config file at {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported config file extension at {0}; use .yaml, .yml, or .toml")]
    UnsupportedExtension(PathBuf),
    #[error("failed to parse yaml config at {path}: {source}")]
    ParseYaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("failed to parse toml config at {path}: {source}")]
    ParseToml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid value for {name}: {value}")]
    InvalidEnvValue { name: &'static str, value: String },
}

pub fn load(config_path_override: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    load_from_map(config_path_override, &env)
}

pub fn example_config(config_path_override: Option<&Path>) -> Result<GenesisConfig, ConfigError> {
    let paths = AppPaths::resolve(config_path_override)?;
    Ok(GenesisConfig {
        schema_version: 1,
        profile: DEFAULT_PROFILE.to_owned(),
        provider: ProviderConfig {
            backend: DEFAULT_PROVIDER_BACKEND.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            base_url: None,
            api_key_env: Some("OPENAI_API_KEY".to_owned()),
        },
        storage: StorageConfig {
            data_dir: paths.data_dir.clone(),
            database_path: paths.database_path,
        },
        runtime: RuntimeConfig {
            max_concurrency: 4,
            allow_destructive_tools: false,
        },
    })
}

pub fn render_example_yaml(config_path_override: Option<&Path>) -> Result<String, ConfigError> {
    let example = example_config(config_path_override)?;
    Ok(serde_yaml::to_string(&example).expect("example config should always serialize"))
}

pub fn load_from_map(
    config_path_override: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> Result<LoadedConfig, ConfigError> {
    let paths = AppPaths::resolve(config_path_override)?;
    let file_config = read_config_file(&paths.config_path)?;

    let data_dir = env
        .get("GENESIS_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| file_config.storage.as_ref().and_then(|storage| storage.data_dir.clone()))
        .unwrap_or_else(|| paths.data_dir.clone());

    let database_path = env
        .get("GENESIS_DATABASE_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            file_config
                .storage
                .as_ref()
                .and_then(|storage| storage.database_path.clone())
        })
        .unwrap_or_else(|| data_dir.join(DEFAULT_DATABASE_FILE));

    let profile = env
        .get("GENESIS_PROFILE")
        .cloned()
        .or_else(|| file_config.profile.clone())
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned());

    let provider = ProviderConfig {
        backend: env
            .get("GENESIS_PROVIDER_BACKEND")
            .cloned()
            .or_else(|| {
                file_config
                    .provider
                    .as_ref()
                    .and_then(|provider| provider.backend.clone())
            })
            .unwrap_or_else(|| DEFAULT_PROVIDER_BACKEND.to_owned()),
        model: env
            .get("GENESIS_MODEL")
            .cloned()
            .or_else(|| {
                file_config
                    .provider
                    .as_ref()
                    .and_then(|provider| provider.model.clone())
            })
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        base_url: env.get("GENESIS_PROVIDER_BASE_URL").cloned().or_else(|| {
            file_config
                .provider
                .as_ref()
                .and_then(|provider| provider.base_url.clone())
        }),
        api_key_env: env.get("GENESIS_PROVIDER_API_KEY_ENV").cloned().or_else(|| {
            file_config
                .provider
                .as_ref()
                .and_then(|provider| provider.api_key_env.clone())
        }),
    };

    let runtime = RuntimeConfig {
        max_concurrency: parse_env(
            env,
            "GENESIS_MAX_CONCURRENCY",
            file_config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.max_concurrency)
                .unwrap_or(4),
        )?,
        allow_destructive_tools: parse_env(
            env,
            "GENESIS_ALLOW_DESTRUCTIVE_TOOLS",
            file_config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.allow_destructive_tools)
                .unwrap_or(false),
        )?,
    };

    Ok(LoadedConfig {
        config: GenesisConfig {
            schema_version: file_config.schema_version.unwrap_or(1),
            profile,
            provider,
            storage: StorageConfig {
                data_dir: data_dir.clone(),
                database_path: database_path.clone(),
            },
            runtime,
        },
        paths: AppPaths {
            config_path: paths.config_path,
            data_dir,
            database_path,
        },
    })
}

impl AppPaths {
    pub fn resolve(config_path_override: Option<&Path>) -> Result<Self, ConfigError> {
        let config_path = config_path_override
            .map(Path::to_path_buf)
            .unwrap_or(default_config_path()?);
        let data_dir = default_data_dir()?;

        Ok(Self {
            config_path,
            database_path: data_dir.join(DEFAULT_DATABASE_FILE),
            data_dir,
        })
    }
}

fn default_config_path() -> Result<PathBuf, ConfigError> {
    let base = dirs::config_dir().ok_or(ConfigError::MissingConfigDirectory)?;
    Ok(base.join(APP_DIR_NAME).join("config.yaml"))
}

fn default_data_dir() -> Result<PathBuf, ConfigError> {
    let base = dirs::data_dir().ok_or(ConfigError::MissingDataDirectory)?;
    Ok(base.join(APP_DIR_NAME))
}

fn read_config_file(path: &Path) -> Result<FileConfig, ConfigError> {
    if !path.exists() {
        return Ok(FileConfig::default());
    }

    let raw = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;

    match path.extension().and_then(|extension| extension.to_str()) {
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw).map_err(|source| {
            ConfigError::ParseYaml {
                path: path.to_path_buf(),
                source,
            }
        }),
        Some("toml") => toml::from_str(&raw).map_err(|source| ConfigError::ParseToml {
            path: path.to_path_buf(),
            source,
        }),
        _ => Err(ConfigError::UnsupportedExtension(path.to_path_buf())),
    }
}

fn parse_env<T>(
    env: &BTreeMap<String, String>,
    name: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    match env.get(name) {
        Some(value) => value.parse::<T>().map_err(|_| ConfigError::InvalidEnvValue {
            name,
            value: value.clone(),
        }),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::load_from_map;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn defaults_to_rust_native_paths_when_no_file_exists() {
        let config = load_from_map(None, &BTreeMap::new()).expect("config should load");

        assert_eq!(config.config.profile, "default");
        assert_eq!(config.config.provider.backend, "openai");
        assert_eq!(config.config.provider.model, "gpt-4.1-mini");
        assert!(config.paths.config_path.ends_with("genesis/config.yaml"));
        assert!(config.paths.database_path.ends_with("genesis/genesis.db"));
    }

    #[test]
    fn merges_yaml_config_with_env_overrides() {
        let dir = tempdir().expect("tempdir should exist");
        let config_path = dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
schema_version: 3
profile: operator
provider:
  backend: openrouter
  model: moonshotai/kimi-k2
runtime:
  max_concurrency: 9
"#,
        )
        .expect("config file should be written");

        let env = BTreeMap::from([
            ("GENESIS_MODEL".to_owned(), "gpt-5".to_owned()),
            (
                "GENESIS_DATABASE_PATH".to_owned(),
                dir.path().join("custom.db").display().to_string(),
            ),
        ]);

        let loaded =
            load_from_map(Some(&config_path), &env).expect("config should merge file and env");

        assert_eq!(loaded.config.schema_version, 3);
        assert_eq!(loaded.config.profile, "operator");
        assert_eq!(loaded.config.provider.backend, "openrouter");
        assert_eq!(loaded.config.provider.model, "gpt-5");
        assert!(loaded.config.storage.database_path.ends_with("custom.db"));
        assert_eq!(loaded.config.runtime.max_concurrency, 9);
    }

    #[test]
    fn renders_example_yaml_with_expected_defaults() {
        let rendered = super::render_example_yaml(None).expect("yaml should render");

        assert!(rendered.contains("schema_version: 1"));
        assert!(rendered.contains("profile: default"));
        assert!(rendered.contains("backend: openai"));
        assert!(rendered.contains("model: gpt-4.1-mini"));
    }
}
