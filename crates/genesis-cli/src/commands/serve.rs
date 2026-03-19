use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, Timelike};
use genesis_config::{load, LoadedConfig};
use genesis_core::execution::delivery_platform_from_str;
use genesis_core::execution::PluginRuntimeOverrides;
use genesis_core::execution::SessionTurnInput;
use genesis_core::scheduler::{check_due_schedules, CronTime};
use genesis_gateway::{build_router, AppState};
use genesis_storage::{bootstrap, ScheduleStore};

use crate::chat::build_session_service;
use crate::{mcp_startup_strict, parse_trusted_proxies, resolve_api_key_required, CliError};

pub(crate) async fn run_schedule_daemon(
    loaded: &LoadedConfig,
    runtime_overrides: PluginRuntimeOverrides,
) -> Result<String, CliError> {
    println!(
        "starting genesis scheduler daemon for provider {} / {}",
        loaded.config.provider.backend, loaded.config.provider.model
    );
    let strict_startup = mcp_startup_strict(loaded)?;
    let service = build_session_service(loaded, strict_startup, false, runtime_overrides).await?;

    loop {
        let store = ScheduleStore::new(&loaded.config.storage.database_path);
        let schedules = store.list_enabled()?;
        let due = check_due_schedules(&schedules, &cron_time_from_datetime(Local::now()));

        for schedule in due {
            let session_id = default_schedule_session_id();
            let outcome = service
                .run_turn(SessionTurnInput {
                    session_id: &session_id,
                    session_platform: &schedule.destination,
                    delivery_platform: delivery_platform_from_str(&schedule.destination),
                    prompt: &schedule.prompt,
                    title: Some(schedule.id.as_str()),
                    images: Vec::new(),
                })
                .await?;

            println!(
                "fired {} -> session {} [{}]: {}",
                schedule.id, outcome.session_id, schedule.destination, outcome.result.response
            );
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

pub(crate) async fn run_serve(
    config_path: Option<PathBuf>,
    host: &str,
    port: u16,
    runtime_overrides: PluginRuntimeOverrides,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;

    let strict_startup = mcp_startup_strict(&loaded)?;
    let service = build_session_service(&loaded, strict_startup, false, runtime_overrides).await?;
    let mcp = service.mcp_manager();

    let api_key = std::env::var("GENESIS_API_KEY").ok();
    let api_key_required = resolve_api_key_required(&loaded.config.profile)?;
    let trusted_proxies = parse_trusted_proxies()?;
    // Env var overrides config file setting
    let rate_limit_rpm = std::env::var("GENESIS_RATE_LIMIT_RPM")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .or_else(|| {
            loaded
                .config
                .gateway
                .as_ref()
                .and_then(|g| g.rate_limit_rpm)
        });
    let state = std::sync::Arc::new(AppState::new(
        loaded,
        api_key,
        api_key_required,
        mcp,
        rate_limit_rpm,
        trusted_proxies,
        runtime_overrides,
    ));
    let router = build_router(std::sync::Arc::clone(&state));

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(CliError::Io)?;

    // Start background scheduler
    let db_path = state.loaded.config.storage.database_path.clone();
    let sched_loaded = std::sync::Arc::clone(&state);
    let executor = std::sync::Arc::new(GatewayScheduleExecutor {
        loaded: sched_loaded,
    });
    let scheduler = genesis_core::scheduler::SchedulerRuntime::new(db_path, executor);
    let sched_cancel = scheduler.cancellation_handle();
    let sched_handle = tokio::spawn(scheduler.run());

    println!("genesis gateway listening on {addr}");
    let shutdown_state = std::sync::Arc::clone(&state);
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let uptime = shutdown_state.started_at.elapsed().as_secs();
            let requests = shutdown_state
                .requests_total
                .load(std::sync::atomic::Ordering::Relaxed);
            let errors = shutdown_state
                .errors_total
                .load(std::sync::atomic::Ordering::Relaxed);
            let input_tokens = shutdown_state
                .input_tokens_total
                .load(std::sync::atomic::Ordering::Relaxed);
            let output_tokens = shutdown_state
                .output_tokens_total
                .load(std::sync::atomic::Ordering::Relaxed);
            println!("\nshutting down gateway...");
            println!(
                "  uptime: {}s | requests: {} | errors: {} | tokens: {} in / {} out",
                uptime, requests, errors, input_tokens, output_tokens
            );

            // Prune expired cache entries on shutdown
            let cache = genesis_storage::ResponseCacheStore::new(
                &shutdown_state.loaded.config.storage.database_path,
            );
            if let Ok(pruned) = cache.prune_expired() {
                if pruned > 0 {
                    println!("  pruned {pruned} expired cache entries");
                }
            }
        })
        .await;

    // Stop the scheduler
    sched_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Err(error) = sched_handle.await {
        return Err(CliError::Other(format!(
            "scheduler task failed during shutdown: {error}"
        )));
    }

    serve_result.map_err(CliError::Io)?;
    Ok("server stopped".to_owned())
}

/// Schedule executor that runs prompts through SessionExecutionService.
pub(crate) struct GatewayScheduleExecutor {
    loaded: std::sync::Arc<genesis_gateway::AppState>,
}

impl genesis_core::scheduler::ScheduleExecutor for GatewayScheduleExecutor {
    fn execute(
        &self,
        schedule: genesis_core::scheduler::DueSchedule,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let service = self.loaded.session_service();
            let session_id = format!("schedule-{}", schedule.id);
            let title = format!("Schedule: {}", schedule.id);
            let platform =
                genesis_core::execution::delivery_platform_from_str(&schedule.destination);
            let input = genesis_core::execution::SessionTurnInput {
                session_id: &session_id,
                session_platform: &schedule.destination,
                delivery_platform: platform,
                prompt: &schedule.prompt,
                images: Vec::new(),
                title: Some(&title),
            };
            service
                .run_turn(input)
                .await
                .map(|_| ())
                .map_err(|e| format!("{e}"))
        })
    }
}

pub(crate) async fn run_nudge(
    config_path: Option<PathBuf>,
    runtime_overrides: PluginRuntimeOverrides,
) -> Result<String, CliError> {
    let loaded = load(config_path.as_deref())?;
    bootstrap(&loaded.config.storage.database_path)?;

    let session_id = format!(
        "nudge-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let prompt = genesis_core::nudge::build_nudge_prompt(&loaded);
    let mut service = genesis_core::execution::SessionExecutionService::new(&loaded);
    service.set_plugin_runtime_overrides(runtime_overrides);
    let response = service
        .run_turn(SessionTurnInput {
            session_id: &session_id,
            session_platform: "nudge",
            delivery_platform: genesis_types::DeliveryPlatform::Cli,
            prompt: &prompt,
            title: Some("Self-reflection nudge"),
            images: Vec::new(),
        })
        .await?
        .result
        .response;
    Ok(format!(
        "Nudge complete (session: {session_id}):\n\n{response}"
    ))
}

pub(crate) fn default_schedule_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("sched-{timestamp}")
}

pub(crate) fn default_schedule_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("sched-run-{timestamp}")
}

pub(crate) fn cron_time_from_datetime<Tz: chrono::TimeZone>(now: DateTime<Tz>) -> CronTime {
    CronTime {
        minute: now.minute(),
        hour: now.hour(),
        day_of_month: now.day(),
        month: now.month(),
        day_of_week: now.weekday().num_days_from_sunday(),
    }
}
