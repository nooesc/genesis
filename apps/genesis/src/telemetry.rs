//! OpenTelemetry tracing export — feature-gated behind `telemetry`.
//!
//! Call [`init_telemetry`] early in the process to get a tracer provider.
//! The caller creates an `OpenTelemetryLayer` from the provider's tracer and
//! composes it into the tracing subscriber.  The returned [`TelemetryGuard`]
//! must be held alive for the process lifetime; dropping it flushes pending
//! spans and shuts down the exporter.

use genesis_config::TelemetryConfig;
use opentelemetry::global;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    trace::{SdkTracerProvider, Sampler},
    Resource,
};

/// Holds the tracer provider so it can be shut down on drop.
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Err(e) = self.provider.shutdown() {
            eprintln!("OpenTelemetry shutdown error: {e}");
        }
    }
}

/// Build the OTel tracer provider.  Returns `None` if telemetry is disabled.
///
/// The caller should create an `OpenTelemetryLayer` from the provider's tracer:
/// ```ignore
/// let tracer = provider.tracer("genesis");
/// let layer = tracing_opentelemetry::OpenTelemetryLayer::new(tracer);
/// ```
pub fn init_telemetry(
    config: &TelemetryConfig,
) -> Option<(SdkTracerProvider, TelemetryGuard)> {
    if !config.enabled {
        return None;
    }

    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(&config.otlp_endpoint)
        .build()
        .ok()?;

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(Sampler::AlwaysOn)
        .with_batch_exporter(exporter)
        .build();

    global::set_tracer_provider(provider.clone());

    let guard = TelemetryGuard {
        provider: provider.clone(),
    };

    Some((provider, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_returns_none() {
        let config = TelemetryConfig::default();
        let result = init_telemetry(&config);
        assert!(result.is_none());
    }
}
