//! Structured logging with JSON output and trace correlation.
//!
//! Implements 12-factor app logging: structured JSON to stdout.

use crate::{LogFormat, TelemetryConfig, TelemetryError};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// Initialize the logging subsystem.
///
/// Sets up tracing-subscriber with either JSON or pretty format,
/// respecting the configured log level.
pub fn init_logging(config: &TelemetryConfig) -> Result<(), TelemetryError> {
    // Build the env filter from config or RUST_LOG
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    match config.log_format {
        LogFormat::Json => init_json_logging(filter),
        LogFormat::Pretty => init_pretty_logging(filter),
    }
}

/// Initialize JSON logging for production.
fn init_json_logging(filter: EnvFilter) -> Result<(), TelemetryError> {
    let json_layer = fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_file(false)
        .with_line_number(false)
        .flatten_event(true)
        .with_filter(filter);

    tracing_subscriber::registry()
        .with(json_layer)
        .try_init()
        .map_err(|e: tracing_subscriber::util::TryInitError| {
            TelemetryError::LoggingInit(e.to_string())
        })
}

/// Initialize pretty logging for development.
fn init_pretty_logging(filter: EnvFilter) -> Result<(), TelemetryError> {
    let pretty_layer = fmt::layer()
        .pretty()
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_filter(filter);

    tracing_subscriber::registry()
        .with(pretty_layer)
        .try_init()
        .map_err(|e: tracing_subscriber::util::TryInitError| {
            TelemetryError::LoggingInit(e.to_string())
        })
}

/// Standard log event names per ADR-0010.
pub mod events {
    /// Gateway is starting up.
    pub const STARTUP: &str = "startup";

    /// Gateway is shutting down.
    pub const SHUTDOWN: &str = "shutdown";

    /// Artifact has been loaded.
    pub const ARTIFACT_LOADED: &str = "artifact_loaded";

    /// Plugin has been initialized.
    pub const PLUGIN_INITIALIZED: &str = "plugin_initialized";

    /// Gateway is listening on a port.
    pub const LISTENING: &str = "listening";

    /// Request has been completed.
    pub const REQUEST_COMPLETED: &str = "request_completed";

    /// Request validation failed.
    pub const VALIDATION_FAILURE: &str = "validation_failure";

    /// Middleware short-circuited the request.
    pub const MIDDLEWARE_SHORT_CIRCUIT: &str = "middleware_short_circuit";

    /// Dispatch to upstream failed.
    pub const DISPATCH_ERROR: &str = "dispatch_error";

    /// WASM plugin trapped (panic/error).
    pub const WASM_TRAP: &str = "wasm_trap";

    /// Secret refresh failed.
    pub const SECRET_REFRESH_FAILED: &str = "secret_refresh_failed";

    /// OTLP export failed.
    pub const OTLP_EXPORT_FAILED: &str = "otlp_export_failed";
}

/// Helper macros for structured logging with standard fields.
///
/// These wrap the tracing macros to ensure consistent field naming.
#[macro_export]
macro_rules! log_startup {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::STARTUP,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_shutdown {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::SHUTDOWN,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_artifact_loaded {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::ARTIFACT_LOADED,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_plugin_initialized {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::PLUGIN_INITIALIZED,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_listening {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::LISTENING,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_request_completed {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::REQUEST_COMPLETED,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_validation_failure {
    ($($field:tt)*) => {
        tracing::warn!(
            event = $crate::logging::events::VALIDATION_FAILURE,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_middleware_short_circuit {
    ($($field:tt)*) => {
        tracing::info!(
            event = $crate::logging::events::MIDDLEWARE_SHORT_CIRCUIT,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_dispatch_error {
    ($($field:tt)*) => {
        tracing::error!(
            event = $crate::logging::events::DISPATCH_ERROR,
            $($field)*
        )
    };
}

#[macro_export]
macro_rules! log_wasm_trap {
    ($($field:tt)*) => {
        tracing::error!(
            event = $crate::logging::events::WASM_TRAP,
            $($field)*
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: We can't easily test logging initialization multiple times
    // in the same test process due to global subscriber state.
    // These tests verify the configuration logic.

    #[test]
    fn test_log_format_parse() {
        assert_eq!(LogFormat::parse("json"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("JSON"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("pretty"), Some(LogFormat::Pretty));
        assert_eq!(LogFormat::parse("PRETTY"), Some(LogFormat::Pretty));
        assert_eq!(LogFormat::parse("invalid"), None);
    }
}
