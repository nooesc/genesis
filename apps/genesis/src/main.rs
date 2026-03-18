use std::io::IsTerminal;

use clap::Parser;
use genesis_cli::{run, Cli, Command};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Parse CLI first so we know whether TUI mode is active before
    // initializing tracing.  In TUI mode, logging MUST go to a file —
    // writing to stdout would corrupt the ratatui viewport.
    let cli = Cli::parse();

    let tui_active = matches!(
        &cli.command,
        Command::Chat { no_tui: false, .. }
    ) && std::io::stdout().is_terminal();

    // Load config to check for telemetry settings.
    let telemetry_config = genesis_config::load(None)
        .ok()
        .and_then(|loaded| loaded.config.telemetry);

    let _otel_guard = init_tracing(tui_active, telemetry_config.as_ref());

    match run(cli).await {
        Ok(output) => {
            if !output.is_empty() {
                println!("{output}");
            }
        }
        Err(error) => {
            eprintln!("\x1b[38;2;215;95;95m{error}\x1b[0m");
            std::process::exit(1);
        }
    }

    // Flush telemetry on exit.
    #[cfg(feature = "telemetry")]
    if let Some(guard) = _otel_guard {
        guard.shutdown();
    }
}

/// Guard that ensures the OTel tracer provider is shut down cleanly.
#[cfg(feature = "telemetry")]
struct OtelGuard {
    provider: opentelemetry_sdk::trace::SdkTracerProvider,
}

#[cfg(feature = "telemetry")]
impl OtelGuard {
    fn shutdown(self) {
        if let Err(e) = self.provider.shutdown() {
            eprintln!("OpenTelemetry shutdown error: {e}");
        }
    }
}

fn init_tracing(
    tui_active: bool,
    #[allow(unused_variables)] telemetry_config: Option<&genesis_config::TelemetryConfig>,
) -> Option<OtelGuard> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let use_json = std::env::var("GENESIS_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    if tui_active {
        // TUI mode: redirect all logging to ~/.genesis/logs/tui.log so it
        // doesn't corrupt the ratatui viewport.
        if let Some(log_path) = genesis_tui::tui_log_path() {
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .compact()
                    .with_ansi(false)
                    .with_writer(file)
                    .try_init();
                return None;
            }
        }
        // If file logging setup failed, suppress all output to avoid
        // corrupting the TUI.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("off"))
            .try_init();
        return None;
    }

    // Non-TUI mode: use registry-based subscriber so we can layer in OTel.
    #[cfg(feature = "telemetry")]
    {
        if let Some(config) = telemetry_config {
            if config.enabled {
                return init_with_otel(filter, use_json, config);
            }
        }
    }

    // Default: fmt-only subscriber.
    if use_json {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .compact()
            .try_init();
    }
    tracing::debug!("tracing initialized");
    None
}

/// Dummy type when telemetry feature is disabled.
#[cfg(not(feature = "telemetry"))]
struct OtelGuard;

/// Initialize tracing with an OpenTelemetry OTLP export layer.
#[cfg(feature = "telemetry")]
fn init_with_otel(
    filter: EnvFilter,
    use_json: bool,
    config: &genesis_config::TelemetryConfig,
) -> Option<OtelGuard> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::{SpanExporter, WithExportConfig};
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use opentelemetry_sdk::Resource;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Build the OTLP span exporter.
    let exporter = match SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&config.otlp_endpoint)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Failed to create OTLP exporter: {e}");
            // Fall back to fmt-only.
            if use_json {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .json()
                    .try_init();
            } else {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .compact()
                    .try_init();
            }
            return None;
        }
    };

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();

    // Create the tracing-opentelemetry layer.
    let otel_layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("genesis"));

    // Build the subscriber with both fmt and OTel layers.
    let registry = tracing_subscriber::registry().with(filter).with(otel_layer);
    if use_json {
        let fmt_layer = tracing_subscriber::fmt::layer().json();
        let _ = registry.with(fmt_layer).try_init();
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .compact();
        let _ = registry.with(fmt_layer).try_init();
    }

    tracing::info!(
        endpoint = config.otlp_endpoint.as_str(),
        service = config.service_name.as_str(),
        "OpenTelemetry tracing initialized"
    );

    Some(OtelGuard { provider })
}
