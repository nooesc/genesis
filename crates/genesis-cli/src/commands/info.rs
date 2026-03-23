use std::path::PathBuf;

use genesis_config::load;
use genesis_storage::{ScheduleStore, SessionStore, SkillStore, UserModelStore};
use genesis_types::DeliveryPlatform;

use crate::CliError;

pub(crate) fn run_info(config_path: Option<PathBuf>, json: bool) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let db_path = &loaded.config.storage.database_path;

    let tool_count = genesis_core::default_tool_count();

    let session_count = SessionStore::new(db_path).count_sessions().unwrap_or(0);

    let skill_count = SkillStore::new(db_path)
        .list_all()
        .map(|s| s.len())
        .unwrap_or(0);

    let trait_count = UserModelStore::new(db_path)
        .list_all()
        .map(|t| t.len())
        .unwrap_or(0);

    let schedule_count = ScheduleStore::new(db_path)
        .list_all()
        .map(|s| s.len())
        .unwrap_or(0);

    let mcp_server_count = loaded.config.mcp_servers.len();
    let version = env!("CARGO_PKG_VERSION");

    if json {
        let info = serde_json::json!({
            "version": version,
            "profile": loaded.config.profile,
            "provider": {
                "backend": loaded.config.provider.backend,
                "model": loaded.config.provider.model,
            },
            "tools": tool_count,
            "mcp_servers": mcp_server_count,
            "sessions": session_count,
            "skills": skill_count,
            "user_traits": trait_count,
            "schedules": schedule_count,
            "config_path": loaded.paths.config_path.display().to_string(),
            "database_path": db_path.display().to_string(),
        });
        Ok(serde_json::to_string_pretty(&info)?)
    } else {
        Ok(format!(
            "genesis v{version}\n\
             profile:     {}\n\
             provider:    {} / {}\n\
             tools:       {tool_count}\n\
             mcp servers: {mcp_server_count}\n\
             sessions:    {session_count}\n\
             skills:      {skill_count}\n\
             user traits: {trait_count}\n\
             schedules:   {schedule_count}\n\
             config:      {}\n\
             database:    {}",
            loaded.config.profile,
            loaded.config.provider.backend,
            loaded.config.provider.model,
            loaded.paths.config_path.display(),
            db_path.display(),
        ))
    }
}

pub(crate) fn run_tools(config_path: Option<PathBuf>, json: bool) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    let ctx = genesis_core::build_execution_context_from_loaded(
        &loaded,
        "tools-list".to_owned(),
        DeliveryPlatform::Cli,
    );
    let runtime = genesis_core::build_default_tool_runtime(&ctx);
    let defs = runtime.definitions();

    if json {
        Ok(serde_json::to_string_pretty(&defs)?)
    } else {
        let mut lines = vec![format!("genesis tools ({} registered)", defs.len())];
        for def in &defs {
            lines.push(format!("  {:<20} {}", def.name, def.description));
        }
        Ok(lines.join("\n"))
    }
}
