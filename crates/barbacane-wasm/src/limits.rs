//! Resource limits for WASM plugin execution.
//!
//! Per SPEC-003 section 6.2, each WASM instance is constrained:
//! - Linear memory: 16 MB
//! - Execution time per call: 100 ms
//! - Stack size: 1 MB

/// Resource limits for plugin execution.
#[derive(Debug, Clone)]
pub struct PluginLimits {
    /// Maximum linear memory in bytes (default: 16 MB).
    pub max_memory_bytes: usize,

    /// Maximum stack size in bytes (default: 1 MB). Set on the engine when it
    /// is built, for every plugin; a plugin's `[limits]` does not change it.
    pub max_stack_bytes: usize,

    /// Maximum execution time per call in milliseconds (default: 100 ms).
    pub max_execution_ms: u64,

    /// Fuel amount that approximates the execution time limit.
    /// This is calibrated to roughly correspond to max_execution_ms.
    pub max_fuel: u64,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 16 * 1024 * 1024, // 16 MB
            max_stack_bytes: 1024 * 1024,       // 1 MB
            max_execution_ms: 100,              // 100 ms
            // Fuel is calibrated experimentally. This value gives roughly 100ms
            // on typical hardware. May need adjustment based on benchmarks.
            max_fuel: 100_000_000,
        }
    }
}

impl PluginLimits {
    /// Create limits with custom memory size.
    pub fn with_memory(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Create limits with custom stack size.
    pub fn with_stack(mut self, bytes: usize) -> Self {
        self.max_stack_bytes = bytes;
        self
    }

    /// Create limits with custom execution timeout.
    pub fn with_timeout(mut self, ms: u64) -> Self {
        self.max_execution_ms = ms;
        // Adjust fuel proportionally
        self.max_fuel = ms * 1_000_000;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits() {
        let limits = PluginLimits::default();
        assert_eq!(limits.max_memory_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.max_stack_bytes, 1024 * 1024);
        assert_eq!(limits.max_execution_ms, 100);
    }

    #[test]
    fn custom_limits() {
        let limits = PluginLimits::default()
            .with_memory(32 * 1024 * 1024)
            .with_stack(2 * 1024 * 1024)
            .with_timeout(200);

        assert_eq!(limits.max_memory_bytes, 32 * 1024 * 1024);
        assert_eq!(limits.max_stack_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.max_execution_ms, 200);
    }
}
