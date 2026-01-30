//! WASM engine configuration and AOT compilation.
//!
//! This module provides the core wasmtime engine with settings optimized
//! for the Barbacane plugin runtime.

use wasmtime::{Config, Engine, Module, OptLevel};

use crate::error::WasmError;
use crate::limits::PluginLimits;

/// A compiled WASM module ready for instantiation.
#[derive(Clone)]
pub struct CompiledModule {
    module: Module,
    /// The plugin name this module belongs to.
    pub name: String,
    /// The plugin version.
    pub version: String,
}

impl CompiledModule {
    /// Get a reference to the underlying wasmtime module.
    pub fn module(&self) -> &Module {
        &self.module
    }
}

/// The WASM engine that compiles and manages plugin modules.
pub struct WasmEngine {
    engine: Engine,
    limits: PluginLimits,
}

impl WasmEngine {
    /// Create a new WASM engine with default configuration.
    pub fn new() -> Result<Self, WasmError> {
        Self::with_limits(PluginLimits::default())
    }

    /// Create a new WASM engine with custom resource limits.
    pub fn with_limits(limits: PluginLimits) -> Result<Self, WasmError> {
        let mut config = Config::new();

        // Disable async support for now - we use synchronous instantiation
        // Can re-enable when we need async host functions
        config.async_support(false);

        // Use Cranelift for AOT compilation with speed optimization
        config.cranelift_opt_level(OptLevel::Speed);

        // Enable fuel consumption for execution time limiting
        config.consume_fuel(true);

        // Configure memory settings
        config.max_wasm_stack(limits.max_stack_bytes);

        // Enable reference types (for modern WASM features)
        config.wasm_reference_types(true);

        // Enable bulk memory operations
        config.wasm_bulk_memory(true);

        // Enable multi-value returns
        config.wasm_multi_value(true);

        // Disable WASM features we don't need
        config.wasm_threads(false);

        let engine = Engine::new(&config).map_err(|e| WasmError::EngineCreation(e.to_string()))?;

        Ok(Self { engine, limits })
    }

    /// Get a reference to the underlying wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Get the configured resource limits.
    pub fn limits(&self) -> &PluginLimits {
        &self.limits
    }

    /// AOT-compile a WASM module from bytes.
    ///
    /// The compiled module can be instantiated multiple times efficiently.
    pub fn compile(
        &self,
        wasm_bytes: &[u8],
        name: String,
        version: String,
    ) -> Result<CompiledModule, WasmError> {
        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| WasmError::Compilation(e.to_string()))?;

        Ok(CompiledModule {
            module,
            name,
            version,
        })
    }

    /// Validate a WASM module without fully compiling it.
    ///
    /// This is faster than full compilation and useful for quick validation.
    pub fn validate(&self, wasm_bytes: &[u8]) -> Result<(), WasmError> {
        Module::validate(&self.engine, wasm_bytes)
            .map_err(|e| WasmError::Compilation(e.to_string()))
    }
}

impl Default for WasmEngine {
    fn default() -> Self {
        Self::new().expect("failed to create default WASM engine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid WASM module (empty module)
    const MINIMAL_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // magic number
        0x01, 0x00, 0x00, 0x00, // version
    ];

    #[test]
    fn create_engine() {
        let engine = WasmEngine::new();
        assert!(engine.is_ok());
    }

    #[test]
    fn create_engine_with_limits() {
        let limits = PluginLimits::default().with_memory(32 * 1024 * 1024);
        let engine = WasmEngine::with_limits(limits);
        assert!(engine.is_ok());
    }

    #[test]
    fn validate_minimal_wasm() {
        let engine = WasmEngine::new().unwrap();
        assert!(engine.validate(MINIMAL_WASM).is_ok());
    }

    #[test]
    fn validate_invalid_wasm() {
        let engine = WasmEngine::new().unwrap();
        let invalid = &[0x00, 0x00, 0x00, 0x00];
        assert!(engine.validate(invalid).is_err());
    }

    #[test]
    fn compile_minimal_wasm() {
        let engine = WasmEngine::new().unwrap();
        let result = engine.compile(MINIMAL_WASM, "test".into(), "1.0.0".into());
        assert!(result.is_ok());
    }

    #[test]
    fn compiled_module_has_metadata() {
        let engine = WasmEngine::new().unwrap();
        let module = engine
            .compile(MINIMAL_WASM, "my-plugin".into(), "2.1.0".into())
            .unwrap();
        assert_eq!(module.name, "my-plugin");
        assert_eq!(module.version, "2.1.0");
    }
}
