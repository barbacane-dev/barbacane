//! OTLP export for traces and metrics.
//!
//! Implements fire-and-forget export to OpenTelemetry Collector.

use crate::{OtlpProtocol, TelemetryConfig, TelemetryError};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime::Tokio,
    trace::{Sampler, TracerProvider},
    Resource,
};

/// Initialize OTLP trace exporter.
///
/// Sets up async batch export to an OpenTelemetry Collector.
/// This replaces the basic tracer if OTLP endpoint is configured.
pub fn init_otlp_tracer(config: &TelemetryConfig) -> Result<(), TelemetryError> {
    let endpoint = match &config.otlp_endpoint {
        Some(ep) => ep,
        None => return Ok(()), // No OTLP endpoint, skip
    };

    // Build the exporter based on protocol
    let exporter = match config.otlp_protocol {
        OtlpProtocol::Grpc => {
            opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()
                .map_err(|e| TelemetryError::OtlpInit(format!("gRPC exporter: {}", e)))?
        }
        OtlpProtocol::Http => {
            opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .build()
                .map_err(|e| TelemetryError::OtlpInit(format!("HTTP exporter: {}", e)))?
        }
    };

    // Configure sampler
    let sampler = if config.trace_sampling >= 1.0 {
        Sampler::AlwaysOn
    } else if config.trace_sampling <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.trace_sampling)
    };

    // Build resource with service name
    let resource = Resource::new(vec![
        KeyValue::new("service.name", config.service_name.clone()),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ]);

    // Build the tracer provider with the updated API
    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, Tokio)
        .with_sampler(sampler)
        .with_resource(resource)
        .build();

    // Set as global provider
    global::set_tracer_provider(provider);

    // Set up W3C Trace Context propagator
    global::set_text_map_propagator(TraceContextPropagator::new());

    Ok(())
}

/// Shutdown OTLP exporter gracefully.
///
/// Flushes any remaining spans before shutdown.
pub fn shutdown_otlp() {
    global::shutdown_tracer_provider();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_without_endpoint() {
        let config = TelemetryConfig::default();
        // Should succeed but do nothing
        let result = init_otlp_tracer(&config);
        assert!(result.is_ok());
    }

    // Note: Testing with actual endpoints requires a running collector
    // and is better done in integration tests.
}
