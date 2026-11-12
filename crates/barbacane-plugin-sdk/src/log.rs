//! Host logging via the `host_log` import.
//!
//! On non-wasm targets (unit tests) logging is a no-op.
//!
//! ```
//! use barbacane_plugin_sdk::log;
//! log::warn("rate limit exceeded");
//! log::log(log::Level::Debug, "verbose detail");
//! ```

/// A log level, as `host_log` takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    /// The level named by `name`, matched case-insensitively (`error`, `warn`,
    /// `warning`, `info`, `debug`).
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }
}

/// Emit a log line at the given level via the host.
#[cfg(target_arch = "wasm32")]
pub fn log(level: Level, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    // SAFETY: the pointer and length describe `msg`, which outlives the call;
    // the host reads the bytes and writes nothing back.
    unsafe {
        host_log(level as i32, msg.as_ptr() as i32, msg.len() as i32);
    }
}

/// No-op on non-wasm targets (native unit tests).
#[cfg(not(target_arch = "wasm32"))]
pub fn log(_level: Level, _msg: &str) {}

/// Log at ERROR level.
pub fn error(msg: &str) {
    log(Level::Error, msg);
}

/// Log at WARN level.
pub fn warn(msg: &str) {
    log(Level::Warn, msg);
}

/// Log at INFO level.
pub fn info(msg: &str) {
    log(Level::Info, msg);
}

/// Log at DEBUG level.
pub fn debug(msg: &str) {
    log(Level::Debug, msg);
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

    #[test]
    fn levels_match_the_host_numbering() {
        assert_eq!(Level::Error as i32, 0);
        assert_eq!(Level::Warn as i32, 1);
        assert_eq!(Level::Info as i32, 2);
        assert_eq!(Level::Debug as i32, 3);
    }

    #[test]
    fn parses_level_names() {
        assert_eq!(Level::parse("WARNING"), Some(Level::Warn));
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("trace"), None);
    }
}
