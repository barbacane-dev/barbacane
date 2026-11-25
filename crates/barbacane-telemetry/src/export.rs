//! The tracer provider: sampling, and OTLP export when an endpoint is set.

use std::sync::OnceLock;

use crate::{OtlpProtocol, TelemetryConfig, TelemetryError};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime,
    trace::{span_processor_with_async_runtime, Sampler, SdkTracerProvider},
    Resource,
};

/// The installed provider, kept so [`shutdown`] can flush it.
static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();

/// The sampler for `rate`: a request whose `traceparent` says sampled or not
/// keeps that decision; a request that starts a trace is sampled at `rate`.
fn sampler(rate: f64) -> Sampler {
    let root = if rate >= 1.0 {
        Sampler::AlwaysOn
    } else if rate <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(rate)
    };
    Sampler::ParentBased(Box::new(root))
}

/// Build and install the tracer provider and the W3C Trace Context
/// propagator. Spans are exported over OTLP when `config` names an endpoint
/// and dropped otherwise; either way each request gets a trace and span id to
/// log and propagate.
///
/// The gRPC exporter runs on the Tokio runtime, so this is called from within
/// one; the HTTP exporter runs on a thread of its own.
pub fn init_tracer_provider(config: &TelemetryConfig) -> Result<SdkTracerProvider, TelemetryError> {
    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();
    let mut builder = SdkTracerProvider::builder()
        .with_sampler(sampler(config.trace_sampling))
        .with_resource(resource);

    if let Some(endpoint) = &config.otlp_endpoint {
        builder = match config.otlp_protocol {
            OtlpProtocol::Grpc => {
                let exporter = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()
                    .map_err(|e| TelemetryError::OtlpInit(format!("gRPC exporter: {e}")))?;
                let processor = span_processor_with_async_runtime::BatchSpanProcessor::builder(
                    exporter,
                    runtime::Tokio,
                )
                .build();
                builder.with_span_processor(processor)
            }
            OtlpProtocol::Http => {
                let exporter = opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_endpoint(http_traces_endpoint(endpoint))
                    .build()
                    .map_err(|e| TelemetryError::OtlpInit(format!("HTTP exporter: {e}")))?;
                builder.with_batch_exporter(exporter)
            }
        };
    }

    let provider = builder.build();
    global::set_tracer_provider(provider.clone());
    global::set_text_map_propagator(TraceContextPropagator::new());
    let _ = PROVIDER.set(provider.clone());
    Ok(provider)
}

/// The OTLP/HTTP traces URL for `endpoint`: the collector's base URL with
/// `/v1/traces` after it, unless it already ends so.
fn http_traces_endpoint(endpoint: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    if base.ends_with("/v1/traces") {
        base.to_string()
    } else {
        format!("{base}/v1/traces")
    }
}

/// Export the spans still buffered, and stop the provider.
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get() {
        let _ = provider.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_http_endpoint_gets_the_traces_path_once() {
        assert_eq!(
            http_traces_endpoint("http://collector:4318"),
            "http://collector:4318/v1/traces"
        );
        assert_eq!(
            http_traces_endpoint("http://collector:4318/"),
            "http://collector:4318/v1/traces"
        );
        assert_eq!(
            http_traces_endpoint("http://collector:4318/v1/traces"),
            "http://collector:4318/v1/traces"
        );
    }
}
