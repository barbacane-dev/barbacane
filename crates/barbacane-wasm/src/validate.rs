//! WASM module validation for exports and imports.
//!
//! Per SPEC-003:
//! - Section 3: Plugins must export specific functions based on type
//! - Section 4: Plugins can only import declared host functions

use std::collections::HashSet;

use wasmtime::Module;

use crate::error::WasmError;
use crate::manifest::{capability_to_imports, PluginType};

/// Validate that a WASM module exports the required functions for its type.
pub fn validate_exports(module: &Module, plugin_type: PluginType) -> Result<(), WasmError> {
    let exports: HashSet<&str> = module.exports().map(|e| e.name()).collect();

    // All plugins must export memory
    if !exports.contains("memory") {
        return Err(WasmError::MissingExport("memory".into()));
    }

    // Check type-specific required exports
    for required in plugin_type.required_exports() {
        if !exports.contains(required) {
            return Err(WasmError::MissingExport((*required).into()));
        }
    }

    // Validate export signatures
    for export in module.exports() {
        match export.name() {
            "init" => validate_init_signature(&export)?,
            "on_request" => validate_on_request_signature(&export)?,
            "on_response" => validate_on_response_signature(&export)?,
            "dispatch" => validate_dispatch_signature(&export)?,
            _ => {} // Ignore other exports
        }
    }

    Ok(())
}

/// Validate that a WASM module only imports declared host functions.
pub fn validate_imports(
    module: &Module,
    declared_capabilities: &[String],
) -> Result<(), WasmError> {
    // Build the set of allowed imports
    let mut allowed: HashSet<&str> = HashSet::new();

    // host_set_output is always allowed (not a capability)
    allowed.insert("host_set_output");

    // Add imports for each declared capability
    for capability in declared_capabilities {
        for import_name in capability_to_imports(capability) {
            allowed.insert(import_name);
        }
    }

    // Check each import from the "barbacane" module
    for import in module.imports() {
        if import.module() == "barbacane" && !allowed.contains(import.name()) {
            return Err(WasmError::UndeclaredImport(import.name().into()));
        }
    }

    Ok(())
}

/// Validate the signature of the `init` export.
/// Expected: (config_ptr: i32, config_len: i32) -> i32
fn validate_init_signature(export: &wasmtime::ExportType) -> Result<(), WasmError> {
    let func = match export.ty() {
        wasmtime::ExternType::Func(f) => f,
        _ => {
            return Err(WasmError::InvalidExportSignature {
                name: "init".into(),
                expected: "function".into(),
                actual: format!("{:?}", export.ty()),
            })
        }
    };

    // Check params: (i32, i32)
    let params: Vec<_> = func.params().collect();
    if params.len() != 2
        || !matches!(params[0], wasmtime::ValType::I32)
        || !matches!(params[1], wasmtime::ValType::I32)
    {
        return Err(WasmError::InvalidExportSignature {
            name: "init".into(),
            expected: "(i32, i32) -> i32".into(),
            actual: format!("{:?}", func),
        });
    }

    // Check results: (i32)
    let results: Vec<_> = func.results().collect();
    if results.len() != 1 || !matches!(results[0], wasmtime::ValType::I32) {
        return Err(WasmError::InvalidExportSignature {
            name: "init".into(),
            expected: "(i32, i32) -> i32".into(),
            actual: format!("{:?}", func),
        });
    }

    Ok(())
}

/// Validate the signature of the `on_request` export.
/// Expected: (request_ptr: i32, request_len: i32) -> i32
fn validate_on_request_signature(export: &wasmtime::ExportType) -> Result<(), WasmError> {
    validate_standard_handler_signature(export, "on_request")
}

/// Validate the signature of the `on_response` export.
/// Expected: (response_ptr: i32, response_len: i32) -> i32
fn validate_on_response_signature(export: &wasmtime::ExportType) -> Result<(), WasmError> {
    validate_standard_handler_signature(export, "on_response")
}

/// Validate the signature of the `dispatch` export.
/// Expected: (request_ptr: i32, request_len: i32) -> i32
fn validate_dispatch_signature(export: &wasmtime::ExportType) -> Result<(), WasmError> {
    validate_standard_handler_signature(export, "dispatch")
}

/// Validate a standard handler signature: (ptr: i32, len: i32) -> i32
fn validate_standard_handler_signature(
    export: &wasmtime::ExportType,
    name: &str,
) -> Result<(), WasmError> {
    let func = match export.ty() {
        wasmtime::ExternType::Func(f) => f,
        _ => {
            return Err(WasmError::InvalidExportSignature {
                name: name.into(),
                expected: "function".into(),
                actual: format!("{:?}", export.ty()),
            })
        }
    };

    let params: Vec<_> = func.params().collect();
    if params.len() != 2
        || !matches!(params[0], wasmtime::ValType::I32)
        || !matches!(params[1], wasmtime::ValType::I32)
    {
        return Err(WasmError::InvalidExportSignature {
            name: name.into(),
            expected: "(i32, i32) -> i32".into(),
            actual: format!("{:?}", func),
        });
    }

    let results: Vec<_> = func.results().collect();
    if results.len() != 1 || !matches!(results[0], wasmtime::ValType::I32) {
        return Err(WasmError::InvalidExportSignature {
            name: name.into(),
            expected: "(i32, i32) -> i32".into(),
            actual: format!("{:?}", func),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests require actual WASM modules with proper exports,
    // which would be created by the plugin SDK. For now, we test
    // the logic with unit tests on the helper functions.

    #[test]
    fn capability_to_imports_log() {
        let imports = capability_to_imports("log");
        assert!(imports.contains(&"host_log"));
    }

    #[test]
    fn capability_to_imports_context() {
        let imports = capability_to_imports("context_get");
        assert!(imports.contains(&"host_context_get"));
        assert!(imports.contains(&"host_context_read_result"));

        let imports = capability_to_imports("context_set");
        assert!(imports.contains(&"host_context_set"));
    }

    #[test]
    fn capability_to_imports_telemetry() {
        let imports = capability_to_imports("telemetry");
        assert!(imports.contains(&"host_metric_counter_inc"));
        assert!(imports.contains(&"host_span_start"));
    }

    #[test]
    fn unknown_capability_returns_empty() {
        let imports = capability_to_imports("unknown");
        assert!(imports.is_empty());
    }
}
