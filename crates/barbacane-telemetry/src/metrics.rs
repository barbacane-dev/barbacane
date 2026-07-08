//! Prometheus metrics registry.
//!
//! Implements all metrics per ADR-0010 and SPEC-005.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use parking_lot::Mutex;
use prometheus_client::{
    encoding::EncodeLabelSet,
    metrics::{counter::Counter, family::Family, gauge::Gauge, histogram::Histogram},
    registry::Registry,
};

/// Untrusted plugins emit metrics across the WASM boundary. Bound what they can
/// allocate on the host heap (plugin metric storage is never evicted):
/// Maximum length of a plugin-supplied metric name.
const MAX_PLUGIN_METRIC_NAME_LEN: usize = 128;
/// Maximum length of a plugin-supplied `labels_json` string.
const MAX_PLUGIN_LABELS_JSON_LEN: usize = 1024;
/// Maximum number of distinct metric series (name + labels) a single plugin may
/// create. Further new series are dropped (existing ones keep updating).
const MAX_PLUGIN_METRIC_CARDINALITY: usize = 1000;

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

/// Connection labels.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ConnectionLabels {
    pub api: String,
}

/// Plugin metric labels (user-defined). `plugin` and `metric` are kept as
/// separate label fields: concatenating them (`"{plugin}_{name}"`) let a plugin
/// forge another plugin's series (e.g. plugin `a`/metric `b_c` collided with
/// plugin `a_b`/metric `c`).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PluginMetricLabels {
    pub plugin: String,
    pub metric: String,
    pub labels_json: String,
}

/// Labels for the drop counter that records rejected plugin metric writes.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PluginMetricDropLabels {
    pub plugin: String,
    /// Why the write was dropped: `oversize`, `cardinality`, or `non_finite`.
    pub reason: String,
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

    // Plugin metrics (dynamically registered)
    pub plugin_counters: Family<PluginMetricLabels, Counter>,
    pub plugin_histograms: Family<PluginMetricLabels, Histogram>,
    /// Count of plugin metric writes rejected by the guards (per plugin/reason).
    pub plugin_metrics_dropped_total: Family<PluginMetricDropLabels, Counter>,

    /// Per-plugin set of admitted series fingerprints, enforcing
    /// [`MAX_PLUGIN_METRIC_CARDINALITY`]. Not a Prometheus metric — internal
    /// accounting so a plugin cannot grow the registry without bound.
    plugin_series: Mutex<HashMap<String, HashSet<u64>>>,
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

        let request_size_bytes = Family::<RequestLabels, Histogram>::new_with_constructor(|| {
            Histogram::new(SIZE_BUCKETS.iter().cloned())
        });
        registry.register(
            "barbacane_request_size_bytes",
            "HTTP request body size in bytes",
            request_size_bytes.clone(),
        );

        let response_size_bytes = Family::<RequestLabels, Histogram>::new_with_constructor(|| {
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

        let plugin_metrics_dropped_total = Family::<PluginMetricDropLabels, Counter>::default();
        registry.register(
            "barbacane_plugin_metrics_dropped_total",
            "Plugin metric writes rejected by cardinality/size/finite guards",
            plugin_metrics_dropped_total.clone(),
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
            plugin_counters,
            plugin_histograms,
            plugin_metrics_dropped_total,
            plugin_series: Mutex::new(HashMap::new()),
        }
    }

    /// Record a completed request.
    #[allow(clippy::too_many_arguments)]
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

    /// Increment a connection.
    pub fn connection_opened(&self) {
        self.active_connections.inc();
        self.connections_total.inc();
    }

    /// Decrement active connections.
    pub fn connection_closed(&self) {
        self.active_connections.dec();
    }

    /// Record that a plugin metric write was dropped by a guard.
    fn record_plugin_metric_dropped(&self, plugin: &str, reason: &str) {
        self.plugin_metrics_dropped_total
            .get_or_create(&PluginMetricDropLabels {
                plugin: plugin.to_string(),
                reason: reason.to_string(),
            })
            .inc();
    }

    /// Whether a plugin may create/update the given series, enforcing the size
    /// and per-plugin cardinality caps. Returns `false` (and records a drop) when
    /// the write must be rejected; an already-seen series is always admitted so
    /// existing metrics keep updating.
    fn admit_plugin_series(&self, plugin: &str, name: &str, labels_json: &str) -> bool {
        if name.len() > MAX_PLUGIN_METRIC_NAME_LEN || labels_json.len() > MAX_PLUGIN_LABELS_JSON_LEN
        {
            self.record_plugin_metric_dropped(plugin, "oversize");
            return false;
        }
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        labels_json.hash(&mut hasher);
        let fingerprint = hasher.finish();

        let mut series = self.plugin_series.lock();
        let seen = series.entry(plugin.to_string()).or_default();
        if seen.contains(&fingerprint) {
            return true;
        }
        if seen.len() >= MAX_PLUGIN_METRIC_CARDINALITY {
            drop(series);
            self.record_plugin_metric_dropped(plugin, "cardinality");
            return false;
        }
        seen.insert(fingerprint);
        true
    }

    /// Increment a plugin counter metric.
    pub fn plugin_counter_inc(&self, plugin: &str, name: &str, labels_json: &str, value: u64) {
        if !self.admit_plugin_series(plugin, name, labels_json) {
            return;
        }
        let labels = PluginMetricLabels {
            plugin: plugin.to_string(),
            metric: name.to_string(),
            labels_json: labels_json.to_string(),
        };
        self.plugin_counters.get_or_create(&labels).inc_by(value);
    }

    /// Observe a plugin histogram metric.
    pub fn plugin_histogram_observe(
        &self,
        plugin: &str,
        name: &str,
        labels_json: &str,
        value: f64,
    ) {
        // A non-finite observation permanently poisons the series' _sum.
        if !value.is_finite() {
            self.record_plugin_metric_dropped(plugin, "non_finite");
            return;
        }
        if !self.admit_plugin_series(plugin, name, labels_json) {
            return;
        }
        let labels = PluginMetricLabels {
            plugin: plugin.to_string(),
            metric: name.to_string(),
            labels_json: labels_json.to_string(),
        };
        self.plugin_histograms.get_or_create(&labels).observe(value);
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
            registry
                .validation_failures_total
                .get_or_create(&labels)
                .get(),
            1
        );
    }

    fn dropped(registry: &MetricsRegistry, plugin: &str, reason: &str) -> u64 {
        registry
            .plugin_metrics_dropped_total
            .get_or_create(&PluginMetricDropLabels {
                plugin: plugin.to_string(),
                reason: reason.to_string(),
            })
            .get()
    }

    #[test]
    fn plugin_metric_name_and_plugin_do_not_collide() {
        // Regression: "{plugin}_{name}" concatenation let plugin `a`/metric `b_c`
        // forge plugin `a_b`/metric `c`. Separate label fields keep them distinct.
        let registry = MetricsRegistry::new();
        registry.plugin_counter_inc("a", "b_c", "{}", 1);
        registry.plugin_counter_inc("a_b", "c", "{}", 1);
        assert_eq!(
            registry
                .plugin_counters
                .get_or_create(&PluginMetricLabels {
                    plugin: "a".to_string(),
                    metric: "b_c".to_string(),
                    labels_json: "{}".to_string(),
                })
                .get(),
            1
        );
        assert_eq!(
            registry
                .plugin_counters
                .get_or_create(&PluginMetricLabels {
                    plugin: "a_b".to_string(),
                    metric: "c".to_string(),
                    labels_json: "{}".to_string(),
                })
                .get(),
            1
        );
    }

    #[test]
    fn plugin_metric_oversize_is_dropped() {
        let registry = MetricsRegistry::new();
        let long_name = "n".repeat(MAX_PLUGIN_METRIC_NAME_LEN + 1);
        registry.plugin_counter_inc("p", &long_name, "{}", 1);
        let long_labels = "x".repeat(MAX_PLUGIN_LABELS_JSON_LEN + 1);
        registry.plugin_counter_inc("p", "ok", &long_labels, 1);
        assert_eq!(dropped(&registry, "p", "oversize"), 2);
    }

    #[test]
    fn plugin_metric_cardinality_is_capped() {
        let registry = MetricsRegistry::new();
        // Fill the budget with distinct label sets, then exceed it.
        for i in 0..MAX_PLUGIN_METRIC_CARDINALITY {
            registry.plugin_counter_inc("p", "m", &format!("{{\"i\":{i}}}"), 1);
        }
        assert_eq!(dropped(&registry, "p", "cardinality"), 0);
        registry.plugin_counter_inc("p", "m", "{\"i\":999999}", 1);
        assert_eq!(dropped(&registry, "p", "cardinality"), 1);
        // An already-admitted series still updates after the cap is reached.
        registry.plugin_counter_inc("p", "m", "{\"i\":0}", 5);
        let v = registry
            .plugin_counters
            .get_or_create(&PluginMetricLabels {
                plugin: "p".to_string(),
                metric: "m".to_string(),
                labels_json: "{\"i\":0}".to_string(),
            })
            .get();
        assert_eq!(v, 6); // 1 (initial) + 5
    }

    #[test]
    fn plugin_histogram_rejects_non_finite() {
        let registry = MetricsRegistry::new();
        registry.plugin_histogram_observe("p", "h", "{}", f64::NAN);
        registry.plugin_histogram_observe("p", "h", "{}", f64::INFINITY);
        assert_eq!(dropped(&registry, "p", "non_finite"), 2);
    }
}
