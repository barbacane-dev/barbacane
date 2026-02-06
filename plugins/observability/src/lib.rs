//! Observability middleware plugin for Barbacane API gateway.
//!
//! Provides per-operation observability features:
//! - Latency SLO monitoring with metric emission
//! - Detailed request/response logging
//! - Custom latency histogram per operation

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

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
fn host_time_now_ms() -> u64 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_time_now() -> i64;
    }
    unsafe { host_time_now() as u64 }
}

/// Log a message via host_log.
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

/// Store a value in the request context.
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

/// Get a value from the request context.
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

/// Increment a counter metric.
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

/// Observe a histogram metric.
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
