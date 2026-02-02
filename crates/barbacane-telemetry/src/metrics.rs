//! Prometheus metrics registry.
//!
//! Implements all metrics per ADR-0010 and SPEC-005.

use prometheus_client::{
    encoding::EncodeLabelSet,
    metrics::{
        counter::Counter,
        family::Family,
        gauge::Gauge,
        histogram::Histogram,
    },
    registry::Registry,
};

/// Duration histogram buckets (in seconds).
/// Covers 1ms to 10s range with exponential growth.
const DURATION_BUCKETS: [f64; 12] = [
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Size histogram buckets (in bytes).
/// Covers 100B to 10MB range with exponential growth.
const SIZE_BUCKETS: [f64; 6] = [100.0, 1000.0, 10000.0, 100000.0, 1000000.0, 10000000.0];

/// Request labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub api: String,
}

/// Validation failure labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ValidationLabels {
    pub method: String,
    pub path: String,
    pub reason: String,
}

/// Middleware labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct MiddlewareLabels {
    pub middleware: String,
    pub phase: String,
}

/// Dispatch labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DispatchLabels {
    pub dispatcher: String,
    pub upstream: String,
}

/// WASM execution labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct WasmLabels {
    pub plugin: String,
    pub function: String,
}

/// Route labels (for deprecation metrics).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RouteLabels {
    pub method: String,
    pub path: String,
    pub api: String,
}

/// SLO labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct SloLabels {
    pub method: String,
    pub path: String,
    pub api: String,
    pub slo_ms: u64,
}

/// Connection labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ConnectionLabels {
    pub api: String,
}

/// Plugin metric labels (user-defined).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PluginMetricLabels {
    pub plugin: String,
    pub labels_json: String,
}

/// Metrics registry holding all Barbacane metrics.
pub struct MetricsRegistry {
    /// The prometheus-client registry for encoding.
    pub registry: Registry,

    // Request metrics
    pub requests_total: Family<RequestLabels, Counter>,
    pub request_duration_seconds: Family<RequestLabels, Histogram>,
    pub request_size_bytes: Family<RequestLabels, Histogram>,
    pub response_size_bytes: Family<RequestLabels, Histogram>,

    // Connection metrics
    pub active_connections: Gauge,
    pub connections_total: Counter,

    // Validation metrics
    pub validation_failures_total: Family<ValidationLabels, Counter>,

    // Middleware metrics
    pub middleware_duration_seconds: Family<MiddlewareLabels, Histogram>,
    pub middleware_short_circuits_total: Family<MiddlewareLabels, Counter>,

    // Dispatch metrics
    pub dispatch_duration_seconds: Family<DispatchLabels, Histogram>,
    pub dispatch_errors_total: Family<DispatchLabels, Counter>,

    // WASM metrics
    pub wasm_execution_duration_seconds: Family<WasmLabels, Histogram>,
    pub wasm_traps_total: Family<WasmLabels, Counter>,

    // Deprecation metrics
    pub deprecated_route_requests_total: Family<RouteLabels, Counter>,

    // SLO metrics
    pub slo_violation_total: Family<SloLabels, Counter>,

    // Plugin metrics (dynamically registered)
    pub plugin_counters: Family<PluginMetricLabels, Counter>,
    pub plugin_histograms: Family<PluginMetricLabels, Histogram>,
}

impl MetricsRegistry {
    /// Create a new metrics registry with all metrics registered.
    pub fn new() -> Self {
        let mut registry = Registry::default();

        // Request metrics
        let requests_total = Family::<RequestLabels, Counter>::default();
        registry.register(
            "barbacane_requests_total",
            "Total number of HTTP requests processed",
            requests_total.clone(),
        );

        let request_duration_seconds =
            Family::<RequestLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(DURATION_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_request_duration_seconds",
            "HTTP request duration in seconds",
            request_duration_seconds.clone(),
        );

        let request_size_bytes =
            Family::<RequestLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(SIZE_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_request_size_bytes",
            "HTTP request body size in bytes",
            request_size_bytes.clone(),
        );

        let response_size_bytes =
            Family::<RequestLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(SIZE_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_response_size_bytes",
            "HTTP response body size in bytes",
            response_size_bytes.clone(),
        );

        // Connection metrics
        let active_connections = Gauge::default();
        registry.register(
            "barbacane_active_connections",
            "Number of currently active connections",
            active_connections.clone(),
        );

        let connections_total = Counter::default();
        registry.register(
            "barbacane_connections_total",
            "Total number of connections accepted",
            connections_total.clone(),
        );

        // Validation metrics
        let validation_failures_total = Family::<ValidationLabels, Counter>::default();
        registry.register(
            "barbacane_validation_failures_total",
            "Total number of request validation failures",
            validation_failures_total.clone(),
        );

        // Middleware metrics
        let middleware_duration_seconds =
            Family::<MiddlewareLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(DURATION_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_middleware_duration_seconds",
            "Middleware execution duration in seconds",
            middleware_duration_seconds.clone(),
        );

        let middleware_short_circuits_total = Family::<MiddlewareLabels, Counter>::default();
        registry.register(
            "barbacane_middleware_short_circuits_total",
            "Total number of middleware short-circuits",
            middleware_short_circuits_total.clone(),
        );

        // Dispatch metrics
        let dispatch_duration_seconds =
            Family::<DispatchLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(DURATION_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_dispatch_duration_seconds",
            "Dispatcher execution duration in seconds",
            dispatch_duration_seconds.clone(),
        );

        let dispatch_errors_total = Family::<DispatchLabels, Counter>::default();
        registry.register(
            "barbacane_dispatch_errors_total",
            "Total number of dispatch errors",
            dispatch_errors_total.clone(),
        );

        // WASM metrics
        let wasm_execution_duration_seconds =
            Family::<WasmLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(DURATION_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_wasm_execution_duration_seconds",
            "WASM plugin execution duration in seconds",
            wasm_execution_duration_seconds.clone(),
        );

        let wasm_traps_total = Family::<WasmLabels, Counter>::default();
        registry.register(
            "barbacane_wasm_traps_total",
            "Total number of WASM plugin traps (panics/errors)",
            wasm_traps_total.clone(),
        );

        // Deprecation metrics
        let deprecated_route_requests_total = Family::<RouteLabels, Counter>::default();
        registry.register(
            "barbacane_deprecated_route_requests_total",
            "Total requests to deprecated routes",
            deprecated_route_requests_total.clone(),
        );

        // SLO metrics
        let slo_violation_total = Family::<SloLabels, Counter>::default();
        registry.register(
            "barbacane_slo_violation_total",
            "Total number of SLO latency violations",
            slo_violation_total.clone(),
        );

        // Plugin metrics
        let plugin_counters = Family::<PluginMetricLabels, Counter>::default();
        registry.register(
            "barbacane_plugin_counter",
            "Plugin-emitted counter metrics",
            plugin_counters.clone(),
        );

        let plugin_histograms =
            Family::<PluginMetricLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(DURATION_BUCKETS.iter().cloned())
            });
        registry.register(
            "barbacane_plugin_histogram",
            "Plugin-emitted histogram metrics",
            plugin_histograms.clone(),
        );

        Self {
            registry,
            requests_total,
            request_duration_seconds,
            request_size_bytes,
            response_size_bytes,
            active_connections,
            connections_total,
            validation_failures_total,
            middleware_duration_seconds,
            middleware_short_circuits_total,
            dispatch_duration_seconds,
            dispatch_errors_total,
            wasm_execution_duration_seconds,
            wasm_traps_total,
            deprecated_route_requests_total,
            slo_violation_total,
            plugin_counters,
            plugin_histograms,
        }
    }

    /// Record a completed request.
    pub fn record_request(
        &self,
        method: &str,
        path: &str,
        status: u16,
        api: &str,
        duration_secs: f64,
        request_size: u64,
        response_size: u64,
    ) {
        let labels = RequestLabels {
            method: method.to_string(),
            path: path.to_string(),
            status,
            api: api.to_string(),
        };

        self.requests_total.get_or_create(&labels).inc();
        self.request_duration_seconds
            .get_or_create(&labels)
            .observe(duration_secs);
        self.request_size_bytes
            .get_or_create(&labels)
            .observe(request_size as f64);
        self.response_size_bytes
            .get_or_create(&labels)
            .observe(response_size as f64);
    }

    /// Record a validation failure.
    pub fn record_validation_failure(&self, method: &str, path: &str, reason: &str) {
        let labels = ValidationLabels {
            method: method.to_string(),
            path: path.to_string(),
            reason: reason.to_string(),
        };
        self.validation_failures_total.get_or_create(&labels).inc();
    }

    /// Record middleware execution.
    pub fn record_middleware(
        &self,
        middleware: &str,
        phase: &str,
        duration_secs: f64,
        short_circuit: bool,
    ) {
        let labels = MiddlewareLabels {
            middleware: middleware.to_string(),
            phase: phase.to_string(),
        };
        self.middleware_duration_seconds
            .get_or_create(&labels)
            .observe(duration_secs);
        if short_circuit {
            self.middleware_short_circuits_total
                .get_or_create(&labels)
                .inc();
        }
    }

    /// Record dispatch execution.
    pub fn record_dispatch(
        &self,
        dispatcher: &str,
        upstream: &str,
        duration_secs: f64,
        error: bool,
    ) {
        let labels = DispatchLabels {
            dispatcher: dispatcher.to_string(),
            upstream: upstream.to_string(),
        };
        self.dispatch_duration_seconds
            .get_or_create(&labels)
            .observe(duration_secs);
        if error {
            self.dispatch_errors_total.get_or_create(&labels).inc();
        }
    }

    /// Record WASM plugin execution.
    pub fn record_wasm_execution(
        &self,
        plugin: &str,
        function: &str,
        duration_secs: f64,
        trap: bool,
    ) {
        let labels = WasmLabels {
            plugin: plugin.to_string(),
            function: function.to_string(),
        };
        self.wasm_execution_duration_seconds
            .get_or_create(&labels)
            .observe(duration_secs);
        if trap {
            self.wasm_traps_total.get_or_create(&labels).inc();
        }
    }

    /// Record a request to a deprecated route.
    pub fn record_deprecated_route(&self, method: &str, path: &str, api: &str) {
        let labels = RouteLabels {
            method: method.to_string(),
            path: path.to_string(),
            api: api.to_string(),
        };
        self.deprecated_route_requests_total
            .get_or_create(&labels)
            .inc();
    }

    /// Record an SLO violation.
    pub fn record_slo_violation(&self, method: &str, path: &str, api: &str, slo_ms: u64) {
        let labels = SloLabels {
            method: method.to_string(),
            path: path.to_string(),
            api: api.to_string(),
            slo_ms,
        };
        self.slo_violation_total.get_or_create(&labels).inc();
    }

    /// Increment a connection.
    pub fn connection_opened(&self) {
        self.active_connections.inc();
        self.connections_total.inc();
    }

    /// Decrement active connections.
    pub fn connection_closed(&self) {
        self.active_connections.dec();
    }

    /// Increment a plugin counter metric.
    pub fn plugin_counter_inc(&self, plugin: &str, name: &str, labels_json: &str, value: u64) {
        let labels = PluginMetricLabels {
            plugin: format!("{}_{}", plugin, name),
            labels_json: labels_json.to_string(),
        };
        self.plugin_counters
            .get_or_create(&labels)
            .inc_by(value);
    }

    /// Observe a plugin histogram metric.
    pub fn plugin_histogram_observe(&self, plugin: &str, name: &str, labels_json: &str, value: f64) {
        let labels = PluginMetricLabels {
            plugin: format!("{}_{}", plugin, name),
            labels_json: labels_json.to_string(),
        };
        self.plugin_histograms
            .get_or_create(&labels)
            .observe(value);
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registry_creation() {
        let registry = MetricsRegistry::new();
        // Verify we can access the metrics
        assert!(registry.active_connections.get() == 0);
    }

    #[test]
    fn test_record_request() {
        let registry = MetricsRegistry::new();
        registry.record_request("GET", "/api/users", 200, "users-api", 0.05, 0, 1024);

        // The counter should be incremented
        let labels = RequestLabels {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            status: 200,
            api: "users-api".to_string(),
        };
        assert_eq!(registry.requests_total.get_or_create(&labels).get(), 1);
    }

    #[test]
    fn test_connection_tracking() {
        let registry = MetricsRegistry::new();
        assert_eq!(registry.active_connections.get(), 0);

        registry.connection_opened();
        assert_eq!(registry.active_connections.get(), 1);
        assert_eq!(registry.connections_total.get(), 1);

        registry.connection_opened();
        assert_eq!(registry.active_connections.get(), 2);
        assert_eq!(registry.connections_total.get(), 2);

        registry.connection_closed();
        assert_eq!(registry.active_connections.get(), 1);
        assert_eq!(registry.connections_total.get(), 2);
    }

    #[test]
    fn test_validation_failure() {
        let registry = MetricsRegistry::new();
        registry.record_validation_failure("POST", "/api/users", "missing_required_field");

        let labels = ValidationLabels {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            reason: "missing_required_field".to_string(),
        };
        assert_eq!(
            registry.validation_failures_total.get_or_create(&labels).get(),
            1
        );
    }
}
