use std::io::IsTerminal;

use clap::Parser;
use genesis_cli::{run, Cli, Command};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

#[cfg(feature = "telemetry")]
mod telemetry;
#[cfg(feature = "telemetry")]
use opentelemetry::trace::TracerProvider as _;

/// Resources that must live for the process lifetime.
struct _TracingGuard {
    #[cfg(feature = "telemetry")]
    _telemetry: Option<telemetry::TelemetryGuard>,
}

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

    // Load config early so telemetry can read its settings.
    let telemetry_config = genesis_config::load(None)
        .ok()
        .and_then(|lc| lc.config.telemetry);

    let _guard = init_tracing(tui_active, telemetry_config.as_ref());

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
}

/// Create an OpenTelemetryLayer from a provider, configured consistently.
#[cfg(feature = "telemetry")]
fn otel_layer<S>(
    provider: &opentelemetry_sdk::trace::SdkTracerProvider,
) -> tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let tracer = provider.tracer("genesis");
    tracing_opentelemetry::OpenTelemetryLayer::new(tracer)
        .with_location(true)
        .with_threads(false)
        .with_error_fields_to_exceptions(true)
}

fn init_tracing(
    tui_active: bool,
    #[allow(unused_variables)] telemetry_config: Option<&genesis_config::TelemetryConfig>,
) -> _TracingGuard {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let use_json = std::env::var("GENESIS_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    // Build the OTel provider (if feature-enabled and configured).
    #[cfg(feature = "telemetry")]
    let (tel_provider, otel_guard) = {
        let default_cfg = genesis_config::TelemetryConfig::default();
        let cfg = telemetry_config.unwrap_or(&default_cfg);
        match telemetry::init_telemetry(cfg) {
            Some((provider, guard)) => (Some(provider), Some(guard)),
            None => (None, None),
        }
    };

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
                let fmt_layer = tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact()
                    .with_ansi(false)
                    .with_writer(file);

                let registry = Registry::default().with(filter).with(fmt_layer);

                #[cfg(feature = "telemetry")]
                let registry = registry.with(tel_provider.as_ref().map(otel_layer));

                let _ = registry.try_init();

                return _TracingGuard {
                    #[cfg(feature = "telemetry")]
                    _telemetry: otel_guard,
                };
            }
        }
        // If file logging setup failed, suppress all output to avoid
        // corrupting the TUI.  Still attach OTel so spans are exported.
        let registry = Registry::default().with(EnvFilter::new("off"));

        #[cfg(feature = "telemetry")]
        let registry = registry.with(tel_provider.as_ref().map(otel_layer));

        let _ = registry.try_init();
    } else if use_json {
        let fmt_layer = tracing_subscriber::fmt::layer().json();
        let registry = Registry::default().with(filter).with(fmt_layer);

        #[cfg(feature = "telemetry")]
        let registry = registry.with(tel_provider.as_ref().map(otel_layer));

        let _ = registry.try_init();
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .compact();
        let registry = Registry::default().with(filter).with(fmt_layer);

        #[cfg(feature = "telemetry")]
        let registry = registry.with(tel_provider.as_ref().map(otel_layer));

        let _ = registry.try_init();
    }

    tracing::debug!("tracing initialized");
    _TracingGuard {
        #[cfg(feature = "telemetry")]
        _telemetry: otel_guard,
    }
}
