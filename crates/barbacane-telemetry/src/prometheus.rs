//! Prometheus text exposition format rendering.
//!
//! Exposes metrics at `/__barbacane/metrics` in Prometheus text format.

use crate::MetricsRegistry;
use prometheus_client::encoding::text::encode;

/// Content-Type header value for Prometheus text format.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Render the metrics registry to Prometheus text format.
pub fn render_metrics(registry: &MetricsRegistry) -> String {
    let mut buffer = String::new();
    encode(&mut buffer, &registry.registry).expect("encoding metrics should not fail");
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_metrics_empty() {
        let registry = MetricsRegistry::new();
        let output = render_metrics(&registry);

        // Should contain the metric definitions even if no samples recorded
        assert!(output.contains("barbacane_requests_total"));
        assert!(output.contains("barbacane_active_connections"));
    }

    #[test]
    fn test_render_metrics_with_data() {
        let registry = MetricsRegistry::new();

        // Record some data
        registry.record_request("GET", "/api/users", 200, "users-api", 0.05, 0, 1024);
        registry.connection_opened();

        let output = render_metrics(&registry);

        // Should contain the recorded data
        assert!(output.contains("barbacane_requests_total"));
        assert!(output.contains("barbacane_active_connections 1"));
    }
}
