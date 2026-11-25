//! Distributed tracing: W3C Trace Context read from a request and written to
//! the requests it causes, and the span names and attributes the gateway
//! records.

use std::collections::HashMap;
use std::time::SystemTime;

use opentelemetry::{
    global::{self, BoxedSpan},
    propagation::{Extractor, Injector},
    trace::{Span as _, SpanKind, Status, TraceContextExt, Tracer},
    Context, KeyValue, Value,
};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Span names.
pub mod spans {
    /// Prefix of a middleware's span, followed by `.{name}.request` or
    /// `.{name}.response`.
    pub const MIDDLEWARE_PREFIX: &str = "barbacane.middleware";

    /// Prefix of a dispatcher's span, followed by `.{name}`.
    pub const DISPATCH_PREFIX: &str = "barbacane.dispatch";

    /// Prefix of a span a plugin opens with `host_span_start`, followed by
    /// `.{plugin}.{name}`.
    pub const PLUGIN_PREFIX: &str = "barbacane.plugin";
}

/// Span attribute keys: the OpenTelemetry HTTP semantic conventions, and the
/// gateway's own under `barbacane.`.
pub mod attributes {
    pub const HTTP_REQUEST_METHOD: &str = "http.request.method";
    pub const HTTP_ROUTE: &str = "http.route";
    pub const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
    pub const URL_PATH: &str = "url.path";
    pub const CLIENT_ADDRESS: &str = "client.address";
    pub const NETWORK_PROTOCOL_VERSION: &str = "network.protocol.version";
    pub const API: &str = "barbacane.api";
    pub const OPERATION_ID: &str = "barbacane.operation_id";
    pub const REQUEST_ID: &str = "barbacane.request_id";
    pub const MIDDLEWARE: &str = "barbacane.middleware";
    pub const DISPATCHER: &str = "barbacane.dispatcher";
    pub const SHORT_CIRCUIT: &str = "barbacane.middleware.short_circuit";
}

/// The trace context headers.
pub const TRACE_HEADERS: [&str; 2] = ["traceparent", "tracestate"];

struct MapExtractor<'a>(&'a HashMap<String, String>);

impl Extractor for MapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

struct MapInjector<'a>(&'a mut HashMap<String, String>);

impl Injector for MapInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

/// The context a request's `traceparent` and `tracestate` name as its parent,
/// keyed by lower-case header name. Without a valid `traceparent` it carries
/// no span, and the request's span starts a trace.
pub fn extract(headers: &HashMap<String, String>) -> Context {
    global::get_text_map_propagator(|propagator| propagator.extract(&MapExtractor(headers)))
}

/// The `traceparent` and `tracestate` headers that make `cx`'s span the
/// parent of the request they are sent with.
pub fn inject(cx: &Context) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(cx, &mut MapInjector(&mut headers))
    });
    headers
}

/// Make the trace context `headers` carry (lower-case names) the parent of
/// `span`, and return the trace id `span` is recorded under, as 32 hex
/// digits. `None` when the gateway records no OpenTelemetry span (no layer
/// installed, as in tests).
pub fn adopt_parent(span: &tracing::Span, headers: &HashMap<String, String>) -> Option<String> {
    let _ = span.set_parent(extract(headers));
    let cx = span.context();
    let span_context = cx.span().span_context().clone();
    span_context
        .is_valid()
        .then(|| span_context.trace_id().to_string())
}

/// Set an OpenTelemetry attribute on `span`, without making it a field of
/// the log lines inside it.
pub fn set_attribute(span: &tracing::Span, key: &'static str, value: impl Into<Value>) {
    span.set_attribute(key, value);
}

/// Rename `span`'s OpenTelemetry span, once more is known of the request.
pub fn set_name(span: &tracing::Span, name: String) {
    span.context().span().update_name(name);
}

/// Mark `span` as failed.
pub fn set_error(span: &tracing::Span, description: impl Into<String>) {
    span.set_status(Status::error(description.into()));
}

/// The `traceparent` and `tracestate` that make the current span the parent
/// of the request they are sent with; empty when no span is recorded.
pub fn current_trace_headers() -> HashMap<String, String> {
    inject(&tracing::Span::current().context())
}

/// A span the gateway opens for a part of a request (a middleware, the
/// dispatch, a plugin's own span), a child of the request's span. It ends
/// when dropped.
pub struct ChildSpan(BoxedSpan);

impl ChildSpan {
    /// Start `name` now, as a child of the current span.
    pub fn start(name: String, attributes: Vec<KeyValue>) -> Self {
        Self::start_at(name, attributes, SystemTime::now())
    }

    /// Start `name` at `start`, as a child of the current span: for a part
    /// whose start is known once it has run.
    pub fn start_at(name: String, attributes: Vec<KeyValue>, start: SystemTime) -> Self {
        let parent = tracing::Span::current().context();
        let span = global::tracer("barbacane")
            .span_builder(name)
            .with_kind(SpanKind::Internal)
            .with_start_time(start)
            .with_attributes(attributes)
            .start_with_context(&global::tracer("barbacane"), &parent);
        Self(span)
    }

    /// Set an attribute.
    pub fn set_attribute(&mut self, key: impl Into<opentelemetry::Key>, value: impl Into<Value>) {
        self.0.set_attribute(KeyValue::new(key, value));
    }

    /// Mark the span as failed.
    pub fn set_error(&mut self, description: impl Into<String>) {
        self.0.set_status(Status::error(description.into()));
    }

    /// End the span at `end`.
    pub fn end_at(mut self, end: SystemTime) {
        self.0.end_with_timestamp(end);
    }
}

/// Record the span of a middleware call that has run for `seconds`:
/// `barbacane.middleware.{name}.{phase}`, a child of the current span.
pub fn record_middleware_span(
    name: &str,
    phase: &str,
    seconds: f64,
    short_circuit: bool,
    failed: bool,
) {
    let end = SystemTime::now();
    let start = std::time::Duration::try_from_secs_f64(seconds)
        .ok()
        .and_then(|elapsed| end.checked_sub(elapsed))
        .unwrap_or(end);
    let mut span = ChildSpan::start_at(
        format!("{}.{name}.{phase}", spans::MIDDLEWARE_PREFIX),
        vec![KeyValue::new(attributes::MIDDLEWARE, name.to_string())],
        start,
    );
    if short_circuit {
        span.set_attribute(attributes::SHORT_CIRCUIT, true);
    }
    if failed {
        span.set_error(format!("middleware {name} failed"));
    }
    span.end_at(end);
}

/// Start the span of a dispatch to `name`: `barbacane.dispatch.{name}`, a
/// child of the current span, ended when dropped.
pub fn start_dispatch_span(name: &str) -> ChildSpan {
    ChildSpan::start(
        format!("{}.{name}", spans::DISPATCH_PREFIX),
        vec![KeyValue::new(attributes::DISPATCHER, name.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry_sdk::propagation::TraceContextPropagator;

    #[test]
    fn a_traceparent_is_read_and_written_back() {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let incoming = HashMap::from([(
            "traceparent".to_string(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
        )]);
        let cx = extract(&incoming);
        assert_eq!(
            cx.span().span_context().trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(
            inject(&cx).get("traceparent").map(String::as_str),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
    }

    #[test]
    fn an_invalid_traceparent_names_no_parent() {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let incoming = HashMap::from([("traceparent".to_string(), "garbage".to_string())]);
        assert!(!extract(&incoming).span().span_context().is_valid());
    }
}
