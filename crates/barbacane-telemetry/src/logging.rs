//! Structured logging with JSON output and trace correlation.
//!
//! Implements 12-factor app logging: structured JSON to stdout.

use crate::{LogFormat, TelemetryConfig, TelemetryError};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::Event;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::{
    filter::filter_fn, fmt, layer::SubscriberExt, registry::LookupSpan, util::SubscriberInitExt,
    EnvFilter, Layer,
};

/// Initialize the logging subsystem.
///
/// Sets up tracing-subscriber with either JSON or pretty format, respecting
/// the configured log level, and the layer that turns the gateway's spans
/// into OpenTelemetry spans of `provider`.
pub fn init_logging(
    config: &TelemetryConfig,
    provider: &SdkTracerProvider,
) -> Result<(), TelemetryError> {
    // Build the env filter from config or RUST_LOG
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    match config.log_format {
        LogFormat::Json => init_json_logging(filter, provider),
        LogFormat::Pretty => init_pretty_logging(filter, provider),
    }
}

/// The OpenTelemetry layer: the gateway's own spans, whatever the log level,
/// and none of its dependencies'.
fn otel_layer<S>(provider: &SdkTracerProvider) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    tracing_opentelemetry::layer()
        .with_tracer(provider.tracer("barbacane"))
        .with_filter(filter_fn(|metadata| {
            metadata.is_span() && metadata.target().starts_with("barbacane")
        }))
}

/// Initialize JSON logging for production.
fn init_json_logging(
    filter: EnvFilter,
    provider: &SdkTracerProvider,
) -> Result<(), TelemetryError> {
    let json_layer = fmt::layer().event_format(RequestJson).with_filter(filter);

    tracing_subscriber::registry()
        .with(RequestFieldsLayer)
        .with(json_layer)
        .with(otel_layer(provider))
        .try_init()
        .map_err(|e: tracing_subscriber::util::TryInitError| {
            TelemetryError::LoggingInit(e.to_string())
        })
}

/// The request fields a span carries, which every JSON line inside it repeats
/// at the top level.
const REQUEST_FIELDS: [&str; 3] = ["request_id", "trace_id", "upstream_endpoint"];

/// The values of [`REQUEST_FIELDS`] a span recorded.
#[derive(Default)]
struct RequestFields(serde_json::Map<String, serde_json::Value>);

impl Visit for RequestFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if REQUEST_FIELDS.contains(&field.name()) {
            self.0.insert(field.name().to_string(), value.into());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if REQUEST_FIELDS.contains(&field.name()) {
            self.0
                .insert(field.name().to_string(), format!("{value:?}").into());
        }
    }
}

/// Keeps each span's [`RequestFields`] in its extensions, as they are
/// recorded.
struct RequestFieldsLayer;

impl<S> Layer<S> for RequestFieldsLayer
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
        let mut fields = RequestFields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: LayerContext<'_, S>) {
        if let Some(span) = ctx.span(id) {
            if let Some(fields) = span.extensions_mut().get_mut::<RequestFields>() {
                values.record(fields);
            }
        }
    }
}

/// A JSON line per event: `timestamp`, `level`, `target`, the event's own
/// fields (`message` among them), and the [`REQUEST_FIELDS`] of the request
/// it belongs to, all at the top level.
struct RequestJson;

/// An event's fields, as JSON values.
struct EventFields<'a>(&'a mut serde_json::Map<String, serde_json::Value>);

impl Visit for EventFields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), value.into());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().to_string(), value.into());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}").into());
    }
}

impl<S, N> FormatEvent<S, N> for RequestJson
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut line = serde_json::Map::new();
        let mut timestamp = String::new();
        SystemTime.format_time(&mut Writer::new(&mut timestamp))?;
        line.insert("timestamp".into(), timestamp.into());
        line.insert("level".into(), event.metadata().level().as_str().into());
        line.insert("target".into(), event.metadata().target().into());
        event.record(&mut EventFields(&mut line));
        if let Some(scope) = ctx.event_scope() {
            for span in scope {
                if let Some(fields) = span.extensions().get::<RequestFields>() {
                    for (name, value) in &fields.0 {
                        line.entry(name.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
        }
        writeln!(writer, "{}", serde_json::Value::Object(line))
    }
}

/// Initialize pretty logging for development.
fn init_pretty_logging(
    filter: EnvFilter,
    provider: &SdkTracerProvider,
) -> Result<(), TelemetryError> {
    let pretty_layer = fmt::layer()
        .pretty()
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_filter(filter);

    tracing_subscriber::registry()
        .with(pretty_layer)
        .with(otel_layer(provider))
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

    /// A shared buffer the formatter writes into.
    #[derive(Clone, Default)]
    struct Buffer(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A line inside a request carries its `request_id` and `trace_id` at the
    /// top level, through nested spans, beside the event's own fields.
    #[test]
    fn request_fields_are_top_level() {
        use tracing_subscriber::layer::SubscriberExt;
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::registry()
            .with(super::RequestFieldsLayer)
            .with(
                tracing_subscriber::fmt::layer()
                    .event_format(super::RequestJson)
                    .with_writer(move || writer.clone()),
            );
        tracing::subscriber::with_default(subscriber, || {
            let request = tracing::error_span!(
                "request",
                request_id = "r-1",
                trace_id = tracing::field::Empty,
                upstream_endpoint = tracing::field::Empty
            );
            request.record("trace_id", "4bf92f3577b34da6a3ce929d0e0e4736");
            let _request = request.enter();
            let inner = tracing::info_span!("inner", other = 1);
            let _inner = inner.enter();
            tracing::info!(status = 200u64, "request completed");
        });
        let text = String::from_utf8(buffer.0.lock().clone()).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.trim()).expect(&text);
        assert_eq!(line["message"], "request completed");
        assert_eq!(line["status"], 200);
        assert_eq!(line["request_id"], "r-1");
        assert_eq!(line["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(line["level"], "INFO");
        assert!(line.get("span").is_none(), "{line}");
        assert!(line.get("upstream_endpoint").is_none(), "{line}");
    }
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
