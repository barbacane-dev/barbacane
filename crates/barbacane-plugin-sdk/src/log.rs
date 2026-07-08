//! Host logging via the `host_log` import.
//!
//! Plugins each redeclared the `host_log` extern + a wasm/native cfg wrapper.
//! This centralizes it. On non-wasm targets (unit tests) logging is a no-op.
//!
//! ```
//! use barbacane_plugin_sdk::log;
//! log::warn("rate limit exceeded");
//! ```

/// Log levels understood by the host (`host_log` level argument).
pub const LEVEL_ERROR: i32 = 0;
pub const LEVEL_WARN: i32 = 1;
pub const LEVEL_INFO: i32 = 2;
pub const LEVEL_DEBUG: i32 = 3;

/// Emit a log line at the given level via the host.
#[cfg(target_arch = "wasm32")]
pub fn log(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

/// No-op on non-wasm targets (native unit tests).
#[cfg(not(target_arch = "wasm32"))]
pub fn log(_level: i32, _msg: &str) {}

/// Log at ERROR level.
pub fn error(msg: &str) {
    log(LEVEL_ERROR, msg);
}

/// Log at WARN level.
pub fn warn(msg: &str) {
    log(LEVEL_WARN, msg);
}

/// Log at INFO level.
pub fn info(msg: &str) {
    log(LEVEL_INFO, msg);
}

/// Log at DEBUG level.
pub fn debug(msg: &str) {
    log(LEVEL_DEBUG, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_is_noop_on_native() {
        // Just exercises the native path; must not panic.
        error("e");
        warn("w");
        info("i");
        debug("d");
    }
}
