//! Distributed tracing with W3C Trace Context.
//!
//! Implements:
//! - W3C Trace Context extraction from incoming requests
//! - Context injection into outgoing upstream requests
//! - Span tree management for request pipeline phases

use opentelemetry::{
    global,
    propagation::{Extractor, Injector, TextMapPropagator},
    trace::{SpanKind, TraceContextExt, Tracer},
    Context, KeyValue,
};
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{Sampler, TracerProvider},
    Resource,
};
use std::collections::HashMap;
use uuid::Uuid;

/// Standard span names per ADR-0010.
pub mod spans {
    /// Root span for the entire request.
    pub const REQUEST: &str = "barbacane.request";

    /// Span for routing phase.
    pub const ROUTING: &str = "barbacane.routing";

    /// Span for validation phase.
    pub const VALIDATION: &str = "barbacane.validation";

    /// Span prefix for middleware execution.
    /// Format: barbacane.middleware.{name}
    pub const MIDDLEWARE_PREFIX: &str = "barbacane.middleware";

    /// Span prefix for dispatcher execution.
    /// Format: barbacane.dispatch.{name}
    pub const DISPATCH_PREFIX: &str = "barbacane.dispatch";

    /// Span for upstream HTTP call.
    pub const UPSTREAM: &str = "barbacane.upstream";

    /// Span for response building.
    pub const RESPONSE: &str = "barbacane.response";
}

/// Standard span attribute keys.
pub mod attributes {
    pub const HTTP_METHOD: &str = "http.method";
    pub const HTTP_PATH: &str = "http.path";
    pub const HTTP_STATUS_CODE: &str = "http.status_code";
    pub const HTTP_REQUEST_SIZE: &str = "http.request.size";
    pub const HTTP_RESPONSE_SIZE: &str = "http.response.size";
    pub const HTTP_ROUTE: &str = "http.route";
    pub const CLIENT_IP: &str = "client.ip";
    pub const API_NAME: &str = "barbacane.api.name";
    pub const ARTIFACT_HASH: &str = "barbacane.artifact.hash";
    pub const MIDDLEWARE_NAME: &str = "barbacane.middleware.name";
    pub const MIDDLEWARE_SHORT_CIRCUIT: &str = "barbacane.middleware.short_circuit";
    pub const DISPATCHER_NAME: &str = "barbacane.dispatcher.name";
    pub const UPSTREAM_URL: &str = "barbacane.upstream.url";
    pub const VALIDATION_VALID: &str = "barbacane.validation.valid";
    pub const VALIDATION_ERROR_COUNT: &str = "barbacane.validation.error_count";
}

/// Header extractor for W3C Trace Context.
struct HeaderExtractor<'a>(&'a HashMap<String, String>);

impl<'a> Extractor for HeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|v| v.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Header injector for W3C Trace Context.
struct HeaderInjector<'a>(&'a mut HashMap<String, String>);

impl<'a> Injector for HeaderInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

/// Tracing context for a request.
///
/// Holds the current OpenTelemetry context and provides methods
/// for span management.
#[derive(Clone)]
pub struct TracingContext {
    /// The OpenTelemetry context.
    context: Context,

    /// Request ID (X-Request-Id header or generated UUID).
    pub request_id: String,

    /// Trace ID from context.
    pub trace_id: String,
}

impl TracingContext {
    /// Create a new tracing context from incoming request headers.
    ///
    /// Extracts W3C Trace Context from headers, or creates a new root context.
    pub fn from_headers(headers: &HashMap<String, String>) -> Self {
        // Set up propagator if not already set
        let propagator = TraceContextPropagator::new();

        // Extract context from headers
        let context = propagator.extract(&HeaderExtractor(headers));

        // Get or generate request ID
        let request_id = headers
            .get("x-request-id")
            .cloned()
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        // Get trace ID from context
        let trace_id = context.span().span_context().trace_id().to_string();

        Self {
            context,
            request_id,
            trace_id,
        }
    }

    /// Create a new root tracing context (no parent).
    pub fn new_root() -> Self {
        let request_id = Uuid::new_v4().to_string();
        let context = Context::current();
        let trace_id = Uuid::new_v4().to_string();

        Self {
            context,
            request_id,
            trace_id,
        }
    }

    /// Get the current OpenTelemetry context.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Inject trace context into outgoing request headers.
    pub fn inject_into_headers(&self, headers: &mut HashMap<String, String>) {
        let propagator = TraceContextPropagator::new();
        propagator.inject_context(&self.context, &mut HeaderInjector(headers));

        // Also inject request ID
        headers.insert("x-request-id".to_string(), self.request_id.clone());
    }

    /// Get headers to add to outgoing requests.
    pub fn get_propagation_headers(&self) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        self.inject_into_headers(&mut headers);
        headers
    }
}

impl Default for TracingContext {
    fn default() -> Self {
        Self::new_root()
    }
}

/// Initialize the global tracer provider.
///
/// This sets up the W3C Trace Context propagator and a basic tracer.
/// For OTLP export, call `init_otlp_tracer` instead (Phase 6).
pub fn init_basic_tracer(service_name: &str, sampling_rate: f64) {
    let sampler = if sampling_rate >= 1.0 {
        Sampler::AlwaysOn
    } else if sampling_rate <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(sampling_rate)
    };

    let provider = TracerProvider::builder()
        .with_sampler(sampler)
        .with_resource(Resource::new(vec![KeyValue::new(
            "service.name",
            service_name.to_string(),
        )]))
        .build();

    global::set_tracer_provider(provider);

    // Set up W3C Trace Context propagator
    global::set_text_map_propagator(TraceContextPropagator::new());
}

/// Shutdown the tracer provider gracefully.
pub fn shutdown_tracer() {
    global::shutdown_tracer_provider();
}

/// Get a tracer for creating spans.
pub fn tracer() -> impl Tracer {
    global::tracer("barbacane")
}

/// Create a span builder with common attributes.
pub fn span_builder(name: &str, kind: SpanKind) -> SpanBuilder {
    SpanBuilder {
        name: name.to_string(),
        kind,
        attributes: Vec::new(),
    }
}

/// Builder for creating spans with attributes.
pub struct SpanBuilder {
    name: String,
    kind: SpanKind,
    attributes: Vec<KeyValue>,
}

impl SpanBuilder {
    /// Add an attribute to the span.
    pub fn with_attribute(mut self, key: &str, value: impl Into<opentelemetry::Value>) -> Self {
        self.attributes
            .push(KeyValue::new(key.to_string(), value.into()));
        self
    }

    /// Add the HTTP method attribute.
    pub fn with_method(self, method: &str) -> Self {
        self.with_attribute(attributes::HTTP_METHOD, method.to_string())
    }

    /// Add the HTTP path attribute.
    pub fn with_path(self, path: &str) -> Self {
        self.with_attribute(attributes::HTTP_PATH, path.to_string())
    }

    /// Add the HTTP status code attribute.
    pub fn with_status_code(self, status: u16) -> Self {
        self.with_attribute(attributes::HTTP_STATUS_CODE, status as i64)
    }

    /// Add the API name attribute.
    pub fn with_api_name(self, api: &str) -> Self {
        self.with_attribute(attributes::API_NAME, api.to_string())
    }

    /// Add the artifact hash attribute.
    pub fn with_artifact_hash(self, hash: &str) -> Self {
        self.with_attribute(attributes::ARTIFACT_HASH, hash.to_string())
    }

    /// Get the span name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the span kind.
    pub fn kind(&self) -> SpanKind {
        self.kind.clone()
    }

    /// Get the attributes.
    pub fn attributes(&self) -> &[KeyValue] {
        &self.attributes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracing_context_from_empty_headers() {
        let headers = HashMap::new();
        let ctx = TracingContext::from_headers(&headers);

        // Should have a generated request ID
        assert!(!ctx.request_id.is_empty());
    }

    #[test]
    fn test_tracing_context_with_request_id() {
        let mut headers = HashMap::new();
        headers.insert("x-request-id".to_string(), "test-request-123".to_string());

        let ctx = TracingContext::from_headers(&headers);
        assert_eq!(ctx.request_id, "test-request-123");
    }

    #[test]
    fn test_inject_headers() {
        let ctx = TracingContext::new_root();
        let mut headers = HashMap::new();
        ctx.inject_into_headers(&mut headers);

        // Should have injected x-request-id
        assert!(headers.contains_key("x-request-id"));
        assert_eq!(headers.get("x-request-id").unwrap(), &ctx.request_id);
    }

    #[test]
    fn test_span_builder() {
        let builder = span_builder("test.span", SpanKind::Internal)
            .with_method("GET")
            .with_path("/api/users")
            .with_status_code(200);

        assert_eq!(builder.name(), "test.span");
        assert_eq!(builder.attributes().len(), 3);
    }
}
