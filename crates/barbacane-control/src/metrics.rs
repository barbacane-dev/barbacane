//! The control plane's Prometheus metrics, served at `GET /metrics`.

use std::sync::LazyLock;

use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RequestLabels {
    method: String,
    /// The route template (`/projects/{id}`), never the raw path.
    route: String,
    status: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct OutcomeLabels {
    outcome: &'static str,
}

/// Requests, compilations, and connected data planes.
pub struct ControlMetrics {
    registry: Registry,
    requests: Family<RequestLabels, Counter>,
    compilations: Family<OutcomeLabels, Counter>,
    data_planes_connected: Gauge,
}

impl ControlMetrics {
    fn new() -> Self {
        let mut registry = Registry::with_prefix("barbacane_control");
        let requests = Family::<RequestLabels, Counter>::default();
        registry.register(
            "requests",
            "API requests by method, route and status",
            requests.clone(),
        );
        let compilations = Family::<OutcomeLabels, Counter>::default();
        registry.register(
            "compilations",
            "Finished compilations by outcome",
            compilations.clone(),
        );
        let data_planes_connected = Gauge::default();
        registry.register(
            "data_planes_connected",
            "Data planes with an open session",
            data_planes_connected.clone(),
        );
        Self {
            registry,
            requests,
            compilations,
            data_planes_connected,
        }
    }

    /// Count a request to `route` answered with `status`.
    pub fn request(&self, method: &str, route: &str, status: u16) {
        self.requests
            .get_or_create(&RequestLabels {
                method: method.to_string(),
                route: route.to_string(),
                status,
            })
            .inc();
    }

    /// Count a finished compilation.
    pub fn compilation(&self, succeeded: bool) {
        let outcome = if succeeded { "succeeded" } else { "failed" };
        self.compilations
            .get_or_create(&OutcomeLabels { outcome })
            .inc();
    }

    /// The text exposition, with `connected` data planes.
    pub fn render(&self, connected: usize) -> String {
        self.data_planes_connected
            .set(i64::try_from(connected).unwrap_or(i64::MAX));
        let mut out = String::new();
        // Writing to a String cannot fail.
        let _ = encode(&mut out, &self.registry);
        out
    }
}

/// The process's metrics.
pub fn metrics() -> &'static ControlMetrics {
    static METRICS: LazyLock<ControlMetrics> = LazyLock::new(ControlMetrics::new);
    &METRICS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_families_are_exposed() {
        let metrics = ControlMetrics::new();
        metrics.request("GET", "/projects/{id}", 200);
        metrics.compilation(true);
        metrics.compilation(false);
        metrics.compilation(false);
        let text = metrics.render(3);
        assert!(
            text.contains(
                r#"barbacane_control_requests_total{method="GET",route="/projects/{id}",status="200"} 1"#
            ),
            "{text}"
        );
        assert!(
            text.contains(r#"barbacane_control_compilations_total{outcome="succeeded"} 1"#),
            "{text}"
        );
        assert!(
            text.contains(r#"barbacane_control_compilations_total{outcome="failed"} 2"#),
            "{text}"
        );
        assert!(
            text.contains("barbacane_control_data_planes_connected 3"),
            "{text}"
        );
    }
}
