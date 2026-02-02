//! Host functions exposed to WASM plugins.
//!
//! Per SPEC-003 section 4, plugins import host functions from the
//! `barbacane` namespace. Each function is only available if declared
//! in the plugin's capabilities.
//!
//! Note: The actual host function implementations are in the instance
//! module where they're registered with the wasmtime linker. This module
//! provides documentation and helper types.

/// Host function for writing output.
///
/// ```text
/// host_set_output(ptr: i32, len: i32)
/// ```
///
/// This function is always available (not a capability).
pub mod output {
    /// The capability name (none - always available).
    pub const CAPABILITY: Option<&str> = None;

    /// The function name in the barbacane namespace.
    pub const FUNCTION_NAME: &str = "host_set_output";
}

/// Host function for logging.
///
/// ```text
/// host_log(level: i32, msg_ptr: i32, msg_len: i32)
/// ```
///
/// Level values:
/// - 0 = error
/// - 1 = warn
/// - 2 = info
/// - 3 = debug
pub mod log {
    /// The capability name.
    pub const CAPABILITY: &str = "log";

    /// The function name in the barbacane namespace.
    pub const FUNCTION_NAME: &str = "host_log";

    /// Log level values.
    pub mod level {
        pub const ERROR: i32 = 0;
        pub const WARN: i32 = 1;
        pub const INFO: i32 = 2;
        pub const DEBUG: i32 = 3;
    }
}

/// Host functions for request context.
///
/// ```text
/// host_context_get(key_ptr: i32, key_len: i32) -> i32
/// host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32)
/// ```
pub mod context {
    /// The capability name for reading context.
    pub const GET_CAPABILITY: &str = "context_get";

    /// The capability name for writing context.
    pub const SET_CAPABILITY: &str = "context_set";

    /// The function names.
    pub const GET_FUNCTION: &str = "host_context_get";
    pub const READ_RESULT_FUNCTION: &str = "host_context_read_result";
    pub const SET_FUNCTION: &str = "host_context_set";
}

/// Host function for monotonic clock.
///
/// ```text
/// host_clock_now() -> i64
/// ```
///
/// Returns milliseconds since an arbitrary reference point.
pub mod clock {
    /// The capability name.
    pub const CAPABILITY: &str = "clock_now";

    /// The function name.
    pub const FUNCTION_NAME: &str = "host_clock_now";
}

/// Host functions for secrets.
///
/// ```text
/// host_get_secret(ref_ptr: i32, ref_len: i32) -> i32
/// host_secret_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// ```
///
/// Not yet implemented (M5 milestone).
pub mod secrets {
    /// The capability name.
    pub const CAPABILITY: &str = "get_secret";

    /// The function names.
    pub const GET_FUNCTION: &str = "host_get_secret";
    pub const READ_RESULT_FUNCTION: &str = "host_secret_read_result";
}

/// Host functions for HTTP calls.
///
/// ```text
/// host_http_call(req_ptr: i32, req_len: i32) -> i32
/// host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// ```
///
/// Not yet implemented (M4 milestone).
pub mod http {
    /// The capability name.
    pub const CAPABILITY: &str = "http_call";

    /// The function names.
    pub const CALL_FUNCTION: &str = "host_http_call";
    pub const READ_RESULT_FUNCTION: &str = "host_http_read_result";
}

/// Host functions for Kafka publishing.
///
/// ```text
/// host_kafka_publish(msg_ptr: i32, msg_len: i32) -> i32
/// host_broker_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// ```
///
/// The message JSON format:
/// ```json
/// {
///   "topic": "events",
///   "key": "optional-partition-key",
///   "payload": "{\"event\":\"data\"}",
///   "headers": {"key": "value"}
/// }
/// ```
///
/// Returns the length of the result JSON, or -1 on error.
/// Result format: `{ success: bool, error?: string, topic: string, partition?: i32, offset?: i64 }`
pub mod kafka {
    /// The capability name.
    pub const CAPABILITY: &str = "kafka_publish";

    /// The function name.
    pub const FUNCTION_NAME: &str = "host_kafka_publish";

    /// The read result function name.
    pub const READ_RESULT_FUNCTION: &str = "host_broker_read_result";
}

/// Host functions for NATS publishing.
///
/// ```text
/// host_nats_publish(msg_ptr: i32, msg_len: i32) -> i32
/// host_broker_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// ```
///
/// The message JSON format:
/// ```json
/// {
///   "topic": "subject.name",
///   "payload": "{\"event\":\"data\"}",
///   "headers": {"key": "value"}
/// }
/// ```
///
/// Returns the length of the result JSON, or -1 on error.
/// Result format: `{ success: bool, error?: string, topic: string }`
pub mod nats {
    /// The capability name.
    pub const CAPABILITY: &str = "nats_publish";

    /// The function name.
    pub const FUNCTION_NAME: &str = "host_nats_publish";

    /// The read result function name (shared with Kafka).
    pub const READ_RESULT_FUNCTION: &str = "host_broker_read_result";
}

/// Host functions for telemetry.
///
/// ```text
/// host_metric_counter_inc(name_ptr, name_len, labels_ptr, labels_len, value: f64)
/// host_metric_histogram_observe(name_ptr, name_len, labels_ptr, labels_len, value: f64)
/// host_span_start(name_ptr: i32, name_len: i32) -> i32
/// host_span_end()
/// host_span_set_attribute(key_ptr, key_len, val_ptr, val_len)
/// ```
///
/// Plugin metrics are auto-prefixed as `barbacane_plugin_{plugin_name}_{metric_name}`.
pub mod telemetry {
    /// The capability name.
    pub const CAPABILITY: &str = "telemetry";

    /// The function names.
    pub const COUNTER_INC: &str = "host_metric_counter_inc";
    pub const HISTOGRAM_OBSERVE: &str = "host_metric_histogram_observe";
    pub const SPAN_START: &str = "host_span_start";
    pub const SPAN_END: &str = "host_span_end";
    pub const SPAN_SET_ATTRIBUTE: &str = "host_span_set_attribute";
}

/// Host function for Unix timestamp.
///
/// ```text
/// host_get_unix_timestamp() -> u64
/// ```
///
/// Returns current Unix timestamp in seconds since epoch.
/// Used by JWT middleware for token expiration validation.
pub mod unix_timestamp {
    /// The capability name (none - always available for security).
    pub const CAPABILITY: Option<&str> = None;

    /// The function name.
    pub const FUNCTION_NAME: &str = "host_get_unix_timestamp";
}

/// Host functions for rate limiting.
///
/// ```text
/// host_rate_limit_check(key_ptr: i32, key_len: i32, quota: u32, window_secs: u32) -> i32
/// host_rate_limit_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// ```
///
/// Returns the length of the JSON result, or -1 on error.
/// Result contains: { allowed: bool, remaining: u32, reset: u64, limit: u32, retry_after?: u64 }
pub mod rate_limit {
    /// The capability name.
    pub const CAPABILITY: &str = "rate_limit";

    /// The function names.
    pub const CHECK_FUNCTION: &str = "host_rate_limit_check";
    pub const READ_RESULT_FUNCTION: &str = "host_rate_limit_read_result";
}

/// Host functions for response caching.
///
/// ```text
/// host_cache_get(key_ptr: i32, key_len: i32) -> i32
/// host_cache_set(key_ptr: i32, key_len: i32, entry_ptr: i32, entry_len: i32, ttl_secs: u32) -> i32
/// host_cache_read_result(buf_ptr: i32, buf_len: i32) -> i32
/// ```
///
/// host_cache_get returns the length of the JSON result, or -1 on error.
/// Result contains: { hit: bool, entry?: { status: u16, headers: {}, body?: string } }
///
/// host_cache_set returns 0 on success, -1 on error.
pub mod cache {
    /// The capability name.
    pub const CAPABILITY: &str = "cache";

    /// The function names.
    pub const GET_FUNCTION: &str = "host_cache_get";
    pub const SET_FUNCTION: &str = "host_cache_set";
    pub const READ_RESULT_FUNCTION: &str = "host_cache_read_result";
}
