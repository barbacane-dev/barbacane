//! The host's clocks, via the `clock_now` capability (`host_clock_now` and
//! `host_get_unix_timestamp`).
//!
//! On non-wasm targets (unit tests) the process clocks are used.
//!
//! ```
//! use barbacane_plugin_sdk::clock;
//! let started = clock::now_ms();
//! let elapsed_ms = clock::now_ms() - started;
//! ```

/// Milliseconds on a monotonic clock. Only differences are meaningful.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u64 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_clock_now() -> i64;
    }
    // SAFETY: the host function takes no pointer and returns a scalar.
    unsafe { host_clock_now().max(0) as u64 }
}

/// Seconds since the Unix epoch.
#[cfg(target_arch = "wasm32")]
pub fn unix_time() -> u64 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_get_unix_timestamp() -> u64;
    }
    // SAFETY: the host function takes no pointer and returns a scalar.
    unsafe { host_get_unix_timestamp() }
}

/// Milliseconds since the first call, on non-wasm targets.
#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

/// Seconds since the Unix epoch, on non-wasm targets.
#[cfg(not(target_arch = "wasm32"))]
pub fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monotonic_clock_does_not_go_back() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a);
    }

    #[test]
    fn unix_time_is_after_2024() {
        assert!(unix_time() > 1_704_067_200);
    }
}
