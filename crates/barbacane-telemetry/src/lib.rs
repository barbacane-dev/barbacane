//! Observability infrastructure for Barbacane API Gateway.
//!
//! This crate provides:
//! - Structured JSON logging with trace correlation
//! - Prometheus metrics registry and exposition
//! - Distributed tracing with W3C Trace Context
//! - OTLP export to OpenTelemetry Collector
//!
//! # Usage
//!
//! ```ignore
//! use barbacane_telemetry::{TelemetryConfig, Telemetry};
//!
//! let config = TelemetryConfig::new()
//!     .with_log_level("info")
//!     .with_otlp_endpoint("http://localhost:4317");
//!
//! let telemetry = Telemetry::init(config)?;
//! ```

pub mod config;
pub mod export;
pub mod logging;
pub mod metrics;
pub mod prometheus;
pub mod tracing;

pub use config::{LogFormat, ObservabilityConfig, OtlpProtocol, TelemetryConfig};
pub use logging::events;
pub use metrics::MetricsRegistry;
pub use prometheus::PROMETHEUS_CONTENT_TYPE;
pub use tracing::{attributes, spans, TracingContext};

use std::sync::Arc;
use thiserror::Error;

/// Telemetry errors.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// Failed to initialize logging.
    #[error("failed to initialize logging: {0}")]
    LoggingInit(String),

    /// Failed to initialize tracing.
    #[error("failed to initialize tracing: {0}")]
    TracingInit(String),

    /// Failed to initialize OTLP exporter.
    #[error("failed to initialize OTLP exporter: {0}")]
    OtlpInit(String),
}

/// Main telemetry handle.
///
/// Holds references to the metrics registry and provides methods for
/// trace context propagation.
pub struct Telemetry {
    config: TelemetryConfig,
    metrics: Arc<MetricsRegistry>,
}

impl Telemetry {
    /// Initialize telemetry with the given configuration.
    ///
    /// This sets up:
    /// - Structured logging (JSON or pretty format)
    /// - Metrics registry
    /// - OTLP exporter (if endpoint configured)
    pub fn init(config: TelemetryConfig) -> Result<Self, TelemetryError> {
        // Initialize structured logging
        logging::init_logging(&config)?;

        // Initialize metrics registry
        let metrics = Arc::new(MetricsRegistry::new());

        // Initialize tracer - OTLP if endpoint configured, basic otherwise
        if config.otlp_endpoint.is_some() {
            export::init_otlp_tracer(&config)?;
        } else {
            tracing::init_basic_tracer(&config.service_name, config.trace_sampling);
        }

        Ok(Self { config, metrics })
    }

    /// Initialize telemetry without setting up logging.
    ///
    /// Use this when logging is already initialized (e.g., in tests).
    pub fn init_without_logging(config: TelemetryConfig) -> Result<Self, TelemetryError> {
        // Initialize metrics registry
        let metrics = Arc::new(MetricsRegistry::new());

        // Initialize tracer - OTLP if endpoint configured, basic otherwise
        if config.otlp_endpoint.is_some() {
            export::init_otlp_tracer(&config)?;
        } else {
            tracing::init_basic_tracer(&config.service_name, config.trace_sampling);
        }

        Ok(Self { config, metrics })
    }

    /// Shutdown telemetry gracefully.
    ///
    /// Flushes any remaining spans before shutdown.
    pub fn shutdown(&self) {
        if self.config.otlp_endpoint.is_some() {
            export::shutdown_otlp();
        } else {
            tracing::shutdown_tracer();
        }
    }

    /// Get the telemetry configuration.
    pub fn config(&self) -> &TelemetryConfig {
        &self.config
    }

    /// Get the metrics registry.
    pub fn metrics(&self) -> &Arc<MetricsRegistry> {
        &self.metrics
    }

    /// Get a cloned Arc reference to the metrics registry.
    pub fn metrics_clone(&self) -> Arc<MetricsRegistry> {
        Arc::clone(&self.metrics)
    }

    /// Render metrics in Prometheus text format.
    pub fn render_prometheus(&self) -> String {
        prometheus::render_metrics(&self.metrics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = TelemetryConfig::default();
        assert_eq!(config.service_name, "barbacane");
        assert_eq!(config.log_level, "info");
        assert_eq!(config.log_format, LogFormat::Json);
        assert!(config.otlp_endpoint.is_none());
        assert_eq!(config.trace_sampling, 1.0);
    }

    #[test]
    fn test_config_builder() {
        let config = TelemetryConfig::new()
            .with_service_name("test-service")
            .with_log_level("debug")
            .with_log_format(LogFormat::Pretty)
            .with_otlp_endpoint("http://localhost:4317")
            .with_trace_sampling(0.5);

        assert_eq!(config.service_name, "test-service");
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.log_format, LogFormat::Pretty);
        assert_eq!(
            config.otlp_endpoint,
            Some("http://localhost:4317".to_string())
        );
        assert_eq!(config.trace_sampling, 0.5);
    }

    #[test]
    fn test_trace_sampling_clamped() {
        let config = TelemetryConfig::new().with_trace_sampling(1.5);
        assert_eq!(config.trace_sampling, 1.0);

        let config = TelemetryConfig::new().with_trace_sampling(-0.5);
        assert_eq!(config.trace_sampling, 0.0);
    }

    #[test]
    fn test_telemetry_init_without_logging() {
        let config = TelemetryConfig::default();
        let result = Telemetry::init_without_logging(config);
        assert!(result.is_ok());
    }
}
