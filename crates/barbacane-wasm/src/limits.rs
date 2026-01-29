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

    /// Maximum stack size in bytes (default: 1 MB).
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
            max_memory_bytes: 16 * 1024 * 1024,  // 16 MB
            max_stack_bytes: 1024 * 1024,         // 1 MB
            max_execution_ms: 100,                // 100 ms
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

/// Resource limiter for wasmtime that enforces memory limits.
pub struct PluginResourceLimiter {
    limits: PluginLimits,
}

impl PluginResourceLimiter {
    /// Create a new resource limiter with the given limits.
    pub fn new(limits: PluginLimits) -> Self {
        Self { limits }
    }
}

impl wasmtime::ResourceLimiter for PluginResourceLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= self.limits.max_memory_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        // Allow reasonable table growth (for function references, etc.)
        Ok(desired <= 10_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::ResourceLimiter;

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

    #[test]
    fn resource_limiter_allows_within_limits() {
        let limits = PluginLimits::default();
        let mut limiter = PluginResourceLimiter::new(limits);

        // Should allow allocation within limits
        assert!(limiter.memory_growing(0, 1024 * 1024, None).unwrap());
        assert!(limiter.memory_growing(0, 16 * 1024 * 1024, None).unwrap());
    }

    #[test]
    fn resource_limiter_denies_over_limit() {
        let limits = PluginLimits::default();
        let mut limiter = PluginResourceLimiter::new(limits);

        // Should deny allocation over limits
        assert!(!limiter.memory_growing(0, 17 * 1024 * 1024, None).unwrap());
    }
}
