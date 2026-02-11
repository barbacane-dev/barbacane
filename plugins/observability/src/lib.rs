//! Observability middleware plugin for Barbacane API gateway.
//!
//! Provides per-operation observability features:
//! - Latency SLO monitoring with metric emission
//! - Detailed request/response logging
//! - Custom latency histogram per operation

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    #![allow(dead_code)]
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    thread_local! {
        static TIME_MS: Cell<u64> = const { Cell::new(0) };
        static CONTEXT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
        static COUNTERS: RefCell<Vec<(String, String, u64)>> = const { RefCell::new(Vec::new()) };
        static HISTOGRAMS: RefCell<Vec<(String, String, f64)>> = const { RefCell::new(Vec::new()) };
        static LOG_MESSAGES: RefCell<Vec<(i32, String)>> = const { RefCell::new(Vec::new()) };
    }
    pub fn set_time(ms: u64) {
        TIME_MS.with(|t| t.set(ms));
    }
    pub fn get_time() -> u64 {
        TIME_MS.with(|t| t.get())
    }
    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|c| c.borrow_mut().insert(key.to_string(), value.to_string()));
    }
    pub fn context_get(key: &str) -> Option<String> {
        CONTEXT.with(|c| c.borrow().get(key).cloned())
    }
    pub fn counter_inc(name: &str, labels: &str, value: u64) {
        COUNTERS.with(|c| {
            c.borrow_mut()
                .push((name.to_string(), labels.to_string(), value))
        });
    }
    pub fn histogram_observe(name: &str, labels: &str, value: f64) {
        HISTOGRAMS.with(|h| {
            h.borrow_mut()
                .push((name.to_string(), labels.to_string(), value))
        });
    }
    pub fn log(level: i32, msg: &str) {
        LOG_MESSAGES.with(|l| l.borrow_mut().push((level, msg.to_string())));
    }
    pub fn get_counters() -> Vec<(String, String, u64)> {
        COUNTERS.with(|c| c.borrow().clone())
    }
    pub fn get_histograms() -> Vec<(String, String, f64)> {
        HISTOGRAMS.with(|h| h.borrow().clone())
    }
    pub fn get_log_messages() -> Vec<(i32, String)> {
        LOG_MESSAGES.with(|l| l.borrow().clone())
    }
    pub fn reset() {
        TIME_MS.with(|t| t.set(0));
        CONTEXT.with(|c| c.borrow_mut().clear());
        COUNTERS.with(|c| c.borrow_mut().clear());
        HISTOGRAMS.with(|h| h.borrow_mut().clear());
        LOG_MESSAGES.with(|l| l.borrow_mut().clear());
    }
}

/// Observability middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct Observability {
    /// Latency SLO threshold in milliseconds.
    /// If set, emits `barbacane_plugin_observability_slo_violation` counter when exceeded.
    #[serde(default)]
    latency_slo_ms: Option<u64>,

    /// Log detailed request information.
    #[serde(default)]
    detailed_request_logs: bool,

    /// Log detailed response information.
    #[serde(default)]
    detailed_response_logs: bool,

    /// Emit a per-operation latency histogram.
    #[serde(default)]
    emit_latency_histogram: bool,
}

impl Observability {
    /// Handle incoming request - record start time and optionally log details.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Record request start time
        let start_time = host_time_now_ms();
        context_set("observability_start_ms", &start_time.to_string());

        // Log detailed request info if enabled
        if self.detailed_request_logs {
            let headers_count = req.headers.len();
            let body_size = req.body.as_ref().map(|b| b.len()).unwrap_or(0);
            log_message(
                1, // INFO
                &format!(
                    "observability: request method={} path={} headers={} body_size={}",
                    req.method, req.path, headers_count, body_size
                ),
            );
        }

        Action::Continue(req)
    }

    /// Handle response - check SLO, emit metrics, and optionally log details.
    pub fn on_response(&mut self, resp: Response) -> Response {
        // Get request start time from context
        let start_ms = context_get("observability_start_ms")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let end_ms = host_time_now_ms();
        let duration_ms = end_ms.saturating_sub(start_ms);

        // Check SLO and emit violation metric if exceeded
        if let Some(slo_ms) = self.latency_slo_ms {
            if duration_ms > slo_ms {
                // Emit SLO violation counter
                let labels = format!("{{\"slo_ms\":{},\"actual_ms\":{}}}", slo_ms, duration_ms);
                host_metric_counter_inc("slo_violation", &labels, 1);

                log_message(
                    2, // WARN
                    &format!(
                        "observability: SLO violation slo_ms={} actual_ms={}",
                        slo_ms, duration_ms
                    ),
                );
            }
        }

        // Emit latency histogram if enabled
        if self.emit_latency_histogram {
            let labels = format!("{{\"status\":{}}}", resp.status);
            host_metric_histogram_observe("latency_ms", &labels, duration_ms as f64);
        }

        // Log detailed response info if enabled
        if self.detailed_response_logs {
            let headers_count = resp.headers.len();
            let body_size = resp.body.as_ref().map(|b| b.len()).unwrap_or(0);
            log_message(
                1, // INFO
                &format!(
                    "observability: response status={} headers={} body_size={} duration_ms={}",
                    resp.status, headers_count, body_size, duration_ms
                ),
            );
        }

        resp
    }
}

/// Get current time in milliseconds.
#[cfg(target_arch = "wasm32")]
fn host_time_now_ms() -> u64 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_time_now() -> i64;
    }
    unsafe { host_time_now() as u64 }
}

#[cfg(not(target_arch = "wasm32"))]
fn host_time_now_ms() -> u64 {
    mock_host::get_time()
}

/// Log a message via host_log.
#[cfg(target_arch = "wasm32")]
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(level: i32, msg: &str) {
    mock_host::log(level, msg);
}

/// Store a value in the request context.
#[cfg(target_arch = "wasm32")]
fn context_set(key: &str, value: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
    }
    unsafe {
        host_context_set(
            key.as_ptr() as i32,
            key.len() as i32,
            value.as_ptr() as i32,
            value.len() as i32,
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn context_set(key: &str, value: &str) {
    mock_host::context_set(key, value);
}

/// Get a value from the request context.
#[cfg(target_arch = "wasm32")]
fn context_get(key: &str) -> Option<String> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_get(key_ptr: i32, key_len: i32) -> i32;
        fn host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    unsafe {
        let len = host_context_get(key.as_ptr() as i32, key.len() as i32);
        if len <= 0 {
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        let read_len = host_context_read_result(buf.as_mut_ptr() as i32, len);
        if read_len != len {
            return None;
        }

        String::from_utf8(buf).ok()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::context_get(key)
}

/// Increment a counter metric.
#[cfg(target_arch = "wasm32")]
fn host_metric_counter_inc(name: &str, labels_json: &str, value: u64) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_metric_counter_inc(
            name_ptr: i32,
            name_len: i32,
            labels_ptr: i32,
            labels_len: i32,
            value: f64,
        );
    }
    unsafe {
        host_metric_counter_inc(
            name.as_ptr() as i32,
            name.len() as i32,
            labels_json.as_ptr() as i32,
            labels_json.len() as i32,
            value as f64,
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn host_metric_counter_inc(name: &str, labels_json: &str, value: u64) {
    mock_host::counter_inc(name, labels_json, value);
}

/// Observe a histogram metric.
#[cfg(target_arch = "wasm32")]
fn host_metric_histogram_observe(name: &str, labels_json: &str, value: f64) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_metric_histogram_observe(
            name_ptr: i32,
            name_len: i32,
            labels_ptr: i32,
            labels_len: i32,
            value: f64,
        );
    }
    unsafe {
        host_metric_histogram_observe(
            name.as_ptr() as i32,
            name.len() as i32,
            labels_json.as_ptr() as i32,
            labels_json.len() as i32,
            value,
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn host_metric_histogram_observe(name: &str, labels_json: &str, value: f64) {
    mock_host::histogram_observe(name, labels_json, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn create_test_request() -> Request {
        Request {
            method: "GET".to_string(),
            path: "/api/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn create_test_response() -> Response {
        Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("test body".to_string()),
        }
    }

    #[test]
    fn test_config_deserialization_defaults() {
        mock_host::reset();

        let config_json = r#"{}"#;
        let config: Observability = serde_json::from_str(config_json).unwrap();

        assert_eq!(config.latency_slo_ms, None);
        assert!(!config.detailed_request_logs);
        assert!(!config.detailed_response_logs);
        assert!(!config.emit_latency_histogram);
    }

    #[test]
    fn test_config_deserialization_with_values() {
        mock_host::reset();

        let config_json = r#"{
            "latency_slo_ms": 100,
            "detailed_request_logs": true,
            "detailed_response_logs": true,
            "emit_latency_histogram": true
        }"#;
        let config: Observability = serde_json::from_str(config_json).unwrap();

        assert_eq!(config.latency_slo_ms, Some(100));
        assert!(config.detailed_request_logs);
        assert!(config.detailed_response_logs);
        assert!(config.emit_latency_histogram);
    }

    #[test]
    fn test_on_request_stores_start_time() {
        mock_host::reset();
        mock_host::set_time(12345);

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        let req = create_test_request();
        let result = plugin.on_request(req);

        // Verify request is passed through
        assert!(matches!(result, Action::Continue(_)));

        // Verify start time was stored in context
        let stored_time = mock_host::context_get("observability_start_ms");
        assert_eq!(stored_time, Some("12345".to_string()));
    }

    #[test]
    fn test_on_request_logs_when_detailed_request_logs_enabled() {
        mock_host::reset();
        mock_host::set_time(100);

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: true,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        let mut req = create_test_request();
        req.headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        req.body = Some("test".to_string());

        let _ = plugin.on_request(req);

        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 1);
        let (level, msg) = &logs[0];
        assert_eq!(*level, 1); // INFO
        assert!(msg.contains("observability: request"));
        assert!(msg.contains("method=GET"));
        assert!(msg.contains("path=/api/test"));
        assert!(msg.contains("headers=1"));
        assert!(msg.contains("body_size=4"));
    }

    #[test]
    fn test_on_request_no_logs_when_detailed_request_logs_disabled() {
        mock_host::reset();
        mock_host::set_time(100);

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        let req = create_test_request();
        let _ = plugin.on_request(req);

        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_on_response_calculates_duration() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        // Set end time to 600ms (500ms duration)
        mock_host::set_time(600);

        let resp = create_test_response();
        let result = plugin.on_response(resp);

        // Verify response is passed through unchanged
        assert_eq!(result.status, 200);
        assert_eq!(result.body, Some("test body".to_string()));
    }

    #[test]
    fn test_on_response_slo_violation_emits_counter_and_log() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: Some(200),
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        // Set end time to 600ms (500ms duration, exceeds 200ms SLO)
        mock_host::set_time(600);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify SLO violation counter was incremented
        let counters = mock_host::get_counters();
        assert_eq!(counters.len(), 1);
        let (name, labels, value) = &counters[0];
        assert_eq!(name, "slo_violation");
        assert_eq!(labels, r#"{"slo_ms":200,"actual_ms":500}"#);
        assert_eq!(*value, 1);

        // Verify warning log was emitted
        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 1);
        let (level, msg) = &logs[0];
        assert_eq!(*level, 2); // WARN
        assert!(msg.contains("observability: SLO violation"));
        assert!(msg.contains("slo_ms=200"));
        assert!(msg.contains("actual_ms=500"));
    }

    #[test]
    fn test_on_response_no_slo_violation_when_within_threshold() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: Some(500),
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        // Set end time to 400ms (300ms duration, within 500ms SLO)
        mock_host::set_time(400);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify no SLO violation counter was emitted
        let counters = mock_host::get_counters();
        assert_eq!(counters.len(), 0);

        // Verify no logs were emitted
        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_on_response_no_slo_check_when_not_configured() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        // Set end time to 10000ms (very long duration)
        mock_host::set_time(10000);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify no counters or logs were emitted
        let counters = mock_host::get_counters();
        assert_eq!(counters.len(), 0);
        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_on_response_emits_histogram_when_enabled() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: true,
        };

        // Set end time to 350ms (250ms duration)
        mock_host::set_time(350);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify histogram was emitted with correct values
        let histograms = mock_host::get_histograms();
        assert_eq!(histograms.len(), 1);
        let (name, labels, value) = &histograms[0];
        assert_eq!(name, "latency_ms");
        assert_eq!(labels, r#"{"status":200}"#);
        assert_eq!(*value, 250.0);
    }

    #[test]
    fn test_on_response_no_histogram_when_disabled() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        mock_host::set_time(350);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify no histogram was emitted
        let histograms = mock_host::get_histograms();
        assert_eq!(histograms.len(), 0);
    }

    #[test]
    fn test_on_response_logs_when_detailed_response_logs_enabled() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: true,
            emit_latency_histogram: false,
        };

        mock_host::set_time(250);

        let mut resp = create_test_response();
        resp.headers
            .insert("Content-Type".to_string(), "application/json".to_string());

        let _ = plugin.on_response(resp);

        // Verify response log was emitted
        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 1);
        let (level, msg) = &logs[0];
        assert_eq!(*level, 1); // INFO
        assert!(msg.contains("observability: response"));
        assert!(msg.contains("status=200"));
        assert!(msg.contains("headers=1"));
        assert!(msg.contains("body_size=9"));
        assert!(msg.contains("duration_ms=150"));
    }

    #[test]
    fn test_on_response_no_logs_when_detailed_response_logs_disabled() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: false,
        };

        mock_host::set_time(250);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify no logs were emitted
        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_on_response_passthrough_preserves_response() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: Some(100),
            detailed_request_logs: false,
            detailed_response_logs: true,
            emit_latency_histogram: true,
        };

        mock_host::set_time(250);

        let mut resp = create_test_response();
        resp.status = 404;
        resp.headers
            .insert("X-Custom".to_string(), "value".to_string());
        resp.body = Some("not found".to_string());

        let result = plugin.on_response(resp);

        // Verify response is completely unchanged
        assert_eq!(result.status, 404);
        assert_eq!(result.headers.get("X-Custom"), Some(&"value".to_string()));
        assert_eq!(result.body, Some("not found".to_string()));
    }

    #[test]
    fn test_on_response_all_features_enabled() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: Some(100),
            detailed_request_logs: false,
            detailed_response_logs: true,
            emit_latency_histogram: true,
        };

        // Set end time to 600ms (500ms duration, exceeds 100ms SLO)
        mock_host::set_time(600);

        let resp = create_test_response();
        let _ = plugin.on_response(resp);

        // Verify SLO violation counter
        let counters = mock_host::get_counters();
        assert_eq!(counters.len(), 1);
        assert_eq!(counters[0].0, "slo_violation");

        // Verify histogram
        let histograms = mock_host::get_histograms();
        assert_eq!(histograms.len(), 1);
        assert_eq!(histograms[0].0, "latency_ms");
        assert_eq!(histograms[0].2, 500.0);

        // Verify logs (SLO warning + response info)
        let logs = mock_host::get_log_messages();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].0, 2); // WARN - SLO violation
        assert_eq!(logs[1].0, 1); // INFO - response details
    }

    #[test]
    fn test_on_response_handles_missing_start_time() {
        mock_host::reset();
        mock_host::set_time(500);
        // Don't set observability_start_ms in context

        let mut plugin = Observability {
            latency_slo_ms: Some(100),
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: true,
        };

        let resp = create_test_response();
        let result = plugin.on_response(resp);

        // Should handle gracefully - duration will be calculated as 500 - 0 = 500
        assert_eq!(result.status, 200);

        // Verify metrics are still emitted with calculated duration
        let histograms = mock_host::get_histograms();
        assert_eq!(histograms.len(), 1);
        assert_eq!(histograms[0].2, 500.0);
    }

    #[test]
    fn test_histogram_status_label_varies_by_response_status() {
        mock_host::reset();
        mock_host::set_time(100);
        mock_host::context_set("observability_start_ms", "100");

        let mut plugin = Observability {
            latency_slo_ms: None,
            detailed_request_logs: false,
            detailed_response_logs: false,
            emit_latency_histogram: true,
        };

        mock_host::set_time(200);

        let mut resp = create_test_response();
        resp.status = 500;

        let _ = plugin.on_response(resp);

        let histograms = mock_host::get_histograms();
        assert_eq!(histograms.len(), 1);
        let (_, labels, _) = &histograms[0];
        assert_eq!(labels, r#"{"status":500}"#);
    }
}
