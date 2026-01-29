//! Error types for the WASM runtime.

use thiserror::Error;

/// Errors that can occur in the WASM runtime.
#[derive(Debug, Error)]
pub enum WasmError {
    /// Failed to create the WASM engine.
    #[error("failed to create WASM engine: {0}")]
    EngineCreation(String),

    /// Failed to compile a WASM module.
    #[error("failed to compile WASM module: {0}")]
    Compilation(String),

    /// Failed to instantiate a WASM module.
    #[error("failed to instantiate WASM module: {0}")]
    Instantiation(String),

    /// WASM execution trapped.
    #[error("WASM execution trapped: {0}")]
    Trap(String),

    /// WASM execution timed out.
    #[error("WASM execution timed out after {0}ms")]
    Timeout(u64),

    /// WASM memory limit exceeded.
    #[error("WASM memory limit exceeded: requested {requested} bytes, limit {limit} bytes")]
    MemoryLimitExceeded { requested: usize, limit: usize },

    /// Missing required export.
    #[error("missing required WASM export: {0}")]
    MissingExport(String),

    /// Invalid export signature.
    #[error("invalid export signature for '{name}': expected {expected}, got {actual}")]
    InvalidExportSignature {
        name: String,
        expected: String,
        actual: String,
    },

    /// Undeclared host function import.
    #[error("plugin imports undeclared host function: {0}")]
    UndeclaredImport(String),

    /// Unknown capability.
    #[error("unknown capability: {0}")]
    UnknownCapability(String),

    /// Plugin manifest parsing failed.
    #[error("failed to parse plugin manifest: {0}")]
    ManifestParse(String),

    /// Plugin manifest validation failed.
    #[error("invalid plugin manifest: {0}")]
    ManifestValidation(String),

    /// Config schema parsing failed.
    #[error("failed to parse config schema: {0}")]
    SchemaParse(String),

    /// Config validation failed.
    #[error("config validation failed: {0}")]
    ConfigValidation(String),

    /// Plugin initialization failed.
    #[error("plugin initialization failed: {0}")]
    InitFailed(String),

    /// Invalid return code from plugin.
    #[error("invalid return code from plugin: {0}")]
    InvalidReturnCode(i32),

    /// Failed to serialize/deserialize data.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<wasmtime::Error> for WasmError {
    fn from(err: wasmtime::Error) -> Self {
        WasmError::Compilation(err.to_string())
    }
}

impl From<serde_json::Error> for WasmError {
    fn from(err: serde_json::Error) -> Self {
        WasmError::Serialization(err.to_string())
    }
}

impl From<toml::de::Error> for WasmError {
    fn from(err: toml::de::Error) -> Self {
        WasmError::ManifestParse(err.to_string())
    }
}
