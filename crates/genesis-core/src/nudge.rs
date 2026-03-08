//! Self-nudge system for periodic agent reflection.
//!
//! The agent reviews its accumulated knowledge (memories, user model, recent
//! sessions) and consolidates learning — updating memories, refining user
//! model observations, and creating skills from repeated patterns.

use genesis_config::LoadedConfig;
use genesis_storage::{bootstrap, format_user_traits, SessionStore, UserModelStore};

use crate::execution::{SessionExecutionError, SessionExecutionService, SessionTurnInput};

/// Build a reflection prompt from the agent's current knowledge state.
///
/// This loads memories, user model traits, and recent sessions from storage,
/// then composes a prompt that asks the agent to reflect and consolidate.
pub fn build_nudge_prompt(loaded: &LoadedConfig) -> String {
    let db_path = &loaded.config.storage.database_path;
    let _ = bootstrap(db_path);

    let memories_section = load_memories_section(db_path);
    let user_model_section = load_user_model_section(db_path);
    let sessions_section = load_recent_sessions_section(db_path);

    let mut prompt = String::from(
        "Perform a self-reflection on your accumulated knowledge. Review the information below and take action:\n\n\
         1. **Consolidate memories**: Remove outdated or redundant memories. Store any new insights.\n\
         2. **Refine user model**: Update observations about the user based on patterns. Increase confidence where evidence is strong, or note new traits you've observed.\n\
         3. **Create skills**: If you see repeated task patterns in recent sessions, create reusable skills to handle them more efficiently next time.\n\
         4. **Prune low-value data**: Delete memories or observations that are no longer relevant.\n\n\
         Be concise. Only make changes that genuinely improve your knowledge. If everything looks good, say so briefly.\n"
    );

    if let Some(ref section) = memories_section {
        prompt.push_str("\n## Current Memories\n");
        prompt.push_str(&section);
        prompt.push('\n');
    }

    if let Some(ref section) = user_model_section {
        prompt.push_str("\n## User Model\n");
        prompt.push_str(&section);
        prompt.push('\n');
    }

    if let Some(ref section) = sessions_section {
        prompt.push_str("\n## Recent Sessions\n");
        prompt.push_str(&section);
        prompt.push('\n');
    }

    if memories_section.is_none() && user_model_section.is_none() && sessions_section.is_none() {
        prompt.push_str(
            "\nNo accumulated knowledge yet. This is normal for early sessions. \
             Focus on learning about the user and their needs as you interact.\n",
        );
    }

    prompt
}

/// Run a nudge turn — execute the reflection prompt through the agent loop.
pub async fn run_nudge(
    loaded: &LoadedConfig,
    session_id: &str,
) -> Result<String, SessionExecutionError> {
    let prompt = build_nudge_prompt(loaded);
    let service = SessionExecutionService::new(loaded);

    let outcome = service
        .run_turn(SessionTurnInput {
            session_id,
            session_platform: "nudge",
            delivery_platform: genesis_types::DeliveryPlatform::Cli,
            prompt: &prompt,
            title: Some("Self-reflection nudge"),
            images: Vec::new(),
        })
        .await?;

    Ok(outcome.result.response)
}

fn load_memories_section(db_path: &std::path::Path) -> Option<String> {
    let connection = rusqlite::Connection::open(db_path).ok()?;
    let mut stmt = connection
        .prepare(
            "SELECT kind, content, created_at FROM memories
             ORDER BY created_at DESC LIMIT 50",
        )
        .ok()?;

    let memories: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    if memories.is_empty() {
        return None;
    }

    let lines: Vec<String> = memories
        .iter()
        .map(|(kind, content, created_at)| format!("- [{}] {} ({})", kind, content, created_at))
        .collect();

    Some(lines.join("\n"))
}

fn load_user_model_section(db_path: &std::path::Path) -> Option<String> {
    let store = UserModelStore::new(db_path);
    let traits = store.list_all().ok()?;
    format_user_traits(&traits)
}

fn load_recent_sessions_section(db_path: &std::path::Path) -> Option<String> {
    let store = SessionStore::new(db_path);
    let sessions = store.list_recent_sessions(10).ok()?;
    if sessions.is_empty() {
        return None;
    }

    let lines: Vec<String> = sessions
        .iter()
        .map(|s| {
            let title = s.title.as_deref().unwrap_or("(untitled)");
            format!("- {} [{}] {} ({})", s.id, s.platform, title, s.updated_at)
        })
        .collect();

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_config::{
        AppPaths, GenesisConfig, LoadedConfig, ProviderConfig, RuntimeConfig, StorageConfig,
    };
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn test_loaded_config(data_dir: PathBuf, database_path: PathBuf) -> LoadedConfig {
        LoadedConfig {
            config: GenesisConfig {
                schema_version: 1,
                profile: "operator".to_owned(),
                provider: ProviderConfig {
                    backend: "openai".to_owned(),
                    model: "gpt-4.1-mini".to_owned(),
                    base_url: Some("http://localhost:8000/v1".to_owned()),
                    api_key_env: None,
                },
                tool_provider: None,
                mcp_servers: std::collections::HashMap::new(),
                storage: StorageConfig {
                    data_dir: data_dir.clone(),
                    database_path: database_path.clone(),
                },
                runtime: RuntimeConfig {
                    max_concurrency: 4,
                    allow_destructive_tools: false,
                    max_turns: 20,
                    max_context_messages: None,
                    budget_limit: None,
                    terminal: None,
                },
            },
            paths: AppPaths {
                config_path: PathBuf::from("/tmp/genesis/config.yaml"),
                data_dir,
                database_path,
            },
        }
    }

    #[test]
    fn nudge_prompt_includes_reflection_instructions() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let db_path = data_dir.join("genesis.db");
        let loaded = test_loaded_config(data_dir, db_path);

        let prompt = build_nudge_prompt(&loaded);
        assert!(prompt.contains("self-reflection"));
        assert!(prompt.contains("Consolidate memories"));
        assert!(prompt.contains("Refine user model"));
        assert!(prompt.contains("Create skills"));
    }

    #[test]
    fn nudge_prompt_includes_memories_when_present() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let db_path = data_dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        // Create session for FK constraint, then store a memory
        let session_store = SessionStore::new(&db_path);
        session_store.create_session("test-s", "cli", None).expect("session");

        let connection = rusqlite::Connection::open(&db_path).expect("open");
        connection
            .execute(
                "INSERT INTO memories (id, session_id, kind, content, created_at) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
                rusqlite::params!["mem-1", "test-s", "preference", "User prefers dark mode"],
            )
            .expect("insert");

        let loaded = test_loaded_config(data_dir, db_path);
        let prompt = build_nudge_prompt(&loaded);
        assert!(prompt.contains("Current Memories"));
        assert!(prompt.contains("dark mode"));
    }

    #[test]
    fn nudge_prompt_includes_user_model_when_present() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let db_path = data_dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let model_store = UserModelStore::new(&db_path);
        model_store
            .observe("likes_rust", "preference", "Prefers Rust", Some("s1"))
            .expect("observe");

        let loaded = test_loaded_config(data_dir, db_path);
        let prompt = build_nudge_prompt(&loaded);
        assert!(prompt.contains("User Model"));
        assert!(prompt.contains("likes_rust"));
    }

    #[test]
    fn nudge_prompt_includes_sessions_when_present() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let db_path = data_dir.join("genesis.db");
        bootstrap(&db_path).expect("bootstrap");

        let session_store = SessionStore::new(&db_path);
        session_store
            .create_session("s-1", "cli", Some("Debug session"))
            .expect("create");

        let loaded = test_loaded_config(data_dir, db_path);
        let prompt = build_nudge_prompt(&loaded);
        assert!(prompt.contains("Recent Sessions"));
        assert!(prompt.contains("Debug session"));
    }

    #[test]
    fn nudge_prompt_handles_empty_state() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let db_path = data_dir.join("genesis.db");
        let loaded = test_loaded_config(data_dir, db_path);

        let prompt = build_nudge_prompt(&loaded);
        assert!(prompt.contains("No accumulated knowledge yet"));
    }
}
