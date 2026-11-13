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

/// The module WASI functions are imported from.
pub const WASI_MODULE: &str = "wasi_snapshot_preview1";

/// The WASI functions the host provides, as stubs in the linker. A plugin
/// importing any other WASI function, or from any module other than this one
/// and `barbacane`, is refused by [`validate_import_modules`]. Part of the
/// plugin interface; `barbacane_compiler::wasi::WASI_IMPORTS` holds the same
/// list for the compile-time check.
pub const WASI_IMPORTS: [&str; 7] = [
    "random_get",
    "clock_time_get",
    "fd_write",
    "sched_yield",
    "environ_get",
    "environ_sizes_get",
    "proc_exit",
];

/// Refuse a module that imports anything the host does not provide: a function
/// from a module other than `barbacane` and [`WASI_MODULE`], or a WASI function
/// outside [`WASI_IMPORTS`].
pub fn validate_import_modules(module: &Module) -> Result<(), WasmError> {
    for import in module.imports() {
        let provided = match import.module() {
            "barbacane" => true,
            WASI_MODULE => WASI_IMPORTS.contains(&import.name()),
            _ => false,
        };
        if !provided {
            return Err(WasmError::UnsupportedImport {
                module: import.module().to_string(),
                name: import.name().to_string(),
                wasi: WASI_MODULE,
                supported: WASI_IMPORTS.join(", "),
            });
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

    // Core ABI functions are always allowed — they are part of the plugin
    // contract (output + the request/response body side-channel governed by the
    // manifest `body_access` flag), not capability-gated host functions.
    for core in [
        "host_set_output",
        "host_body_len",
        "host_body_read",
        "host_body_set",
        "host_body_clear",
    ] {
        allowed.insert(core);
    }

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

    fn module(body: &str) -> Module {
        let engine = wasmtime::Engine::default();
        let wasm = wat::parse_str(format!("(module (memory (export \"memory\") 1) {body})"))
            .expect("valid WAT");
        Module::new(&engine, wasm).expect("valid module")
    }

    const ALLOC: &str = r#"(func (export "alloc") (param i32) (result i32) (i32.const 1024))"#;
    const INIT: &str = r#"(func (export "init") (param i32 i32) (result i32) (i32.const 0))"#;
    const DISPATCH: &str =
        r#"(func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))"#;

    #[test]
    fn a_dispatcher_with_its_exports_passes() {
        let m = module(&format!("{ALLOC} {INIT} {DISPATCH}"));
        assert!(validate_exports(&m, PluginType::Dispatcher).is_ok());
    }

    #[test]
    fn alloc_is_required() {
        let m = module(&format!("{INIT} {DISPATCH}"));
        assert!(matches!(
            validate_exports(&m, PluginType::Dispatcher),
            Err(WasmError::MissingExport(name)) if name == "alloc"
        ));
    }

    #[test]
    fn a_middleware_needs_both_hooks() {
        let m = module(&format!(
            "{ALLOC} {INIT} (func (export \"on_request\") (param i32 i32) (result i32) (i32.const 0))"
        ));
        assert!(matches!(
            validate_exports(&m, PluginType::Middleware),
            Err(WasmError::MissingExport(name)) if name == "on_response"
        ));
    }

    #[test]
    fn an_export_with_another_signature_is_refused() {
        let m = module(&format!(
            "{ALLOC} {INIT} (func (export \"dispatch\") (param i32) (result i32) (i32.const 0))"
        ));
        assert!(matches!(
            validate_exports(&m, PluginType::Dispatcher),
            Err(WasmError::InvalidExportSignature { name, .. }) if name == "dispatch"
        ));
    }

    #[test]
    fn plugin_type_parses_its_two_names() {
        assert_eq!(
            PluginType::parse("middleware"),
            Some(PluginType::Middleware)
        );
        assert_eq!(
            PluginType::parse("dispatcher"),
            Some(PluginType::Dispatcher)
        );
        assert_eq!(PluginType::parse("plugin"), None);
    }

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
    fn capability_to_imports_uuid() {
        let imports = capability_to_imports("generate_uuid");
        assert!(imports.contains(&"host_uuid_generate"));
        assert!(imports.contains(&"host_uuid_read_result"));
    }

    #[test]
    fn capability_to_imports_ldap() {
        let imports = capability_to_imports("ldap");
        assert!(imports.contains(&"host_ldap_bind"));
        assert!(imports.contains(&"host_ldap_search"));
        assert!(imports.contains(&"host_ldap_read_result"));
    }

    #[test]
    fn unknown_capability_returns_empty() {
        let imports = capability_to_imports("unknown");
        assert!(imports.is_empty());
    }

    fn module_importing(wat_imports: &str) -> wasmtime::Module {
        let engine = wasmtime::Engine::default();
        let wasm = wat::parse_str(format!(
            "(module {wat_imports} (memory (export \"memory\") 1))"
        ))
        .expect("valid wat");
        wasmtime::Module::new(&engine, &wasm).expect("valid module")
    }

    #[test]
    fn a_wasi_function_outside_the_set_is_refused_by_name() {
        let module = module_importing(
            r#"(import "wasi_snapshot_preview1" "fd_close" (func (param i32) (result i32)))"#,
        );
        let err = validate_import_modules(&module).unwrap_err().to_string();
        assert!(err.contains("wasi_snapshot_preview1::fd_close"), "{err}");
        assert!(
            err.contains("random_get, clock_time_get"),
            "lists the set: {err}"
        );
    }

    #[test]
    fn an_import_from_another_module_is_refused() {
        let module = module_importing(r#"(import "env" "abort" (func))"#);
        let err = validate_import_modules(&module).unwrap_err().to_string();
        assert!(err.contains("env::abort"), "{err}");
    }

    #[test]
    fn host_functions_and_listed_wasi_functions_pass() {
        let module = module_importing(
            r#"(import "barbacane" "host_log" (func (param i32 i32 i32)))
               (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#,
        );
        assert!(validate_import_modules(&module).is_ok());
    }

    fn module_with_import(import: &str) -> wasmtime::Module {
        let engine = wasmtime::Engine::default();
        let wat = format!(
            r#"(module
                 (import "barbacane" "{import}" (func (param i32 i32)))
                 (memory (export "memory") 1))"#
        );
        let wasm = wat::parse_str(&wat).expect("valid wat");
        wasmtime::Module::new(&engine, &wasm).expect("valid module")
    }

    #[test]
    fn validate_imports_rejects_undeclared_capability() {
        // A plugin that declares only `log` must not be able to import
        // host_http_call (the http_call capability) — default-deny.
        let module = module_with_import("host_http_call");
        let declared = vec!["log".to_string()];
        let err = validate_imports(&module, &declared).unwrap_err();
        assert!(matches!(err, WasmError::UndeclaredImport(name) if name == "host_http_call"));
    }

    #[test]
    fn validate_imports_gates_ldap_behind_its_capability() {
        let module = module_with_import("host_ldap_search");
        let err = validate_imports(&module, &["log".to_string()]).unwrap_err();
        assert!(matches!(err, WasmError::UndeclaredImport(name) if name == "host_ldap_search"));
        assert!(validate_imports(&module, &["ldap".to_string()]).is_ok());
    }

    #[test]
    fn validate_imports_gates_hashing_behind_its_capability() {
        let module = module_with_import("host_sha256");
        for declared in [
            vec![],
            vec!["log".to_string()],
            vec!["verify_signature".to_string()],
        ] {
            let err = validate_imports(&module, &declared).unwrap_err();
            assert!(
                matches!(err, WasmError::UndeclaredImport(ref name) if name == "host_sha256"),
                "{declared:?} must not grant host_sha256: {err}"
            );
        }
        assert!(validate_imports(&module, &["hash".to_string()]).is_ok());
    }

    #[test]
    fn the_hash_capability_grants_only_host_sha256() {
        assert_eq!(capability_to_imports("hash"), &["host_sha256"]);
        let module = module_with_import("host_verify_signature");
        assert!(validate_imports(&module, &["hash".to_string()]).is_err());
    }

    #[test]
    fn validate_imports_allows_declared_capability_and_core_abi() {
        // host_log is covered by the declared `log` capability...
        assert!(validate_imports(&module_with_import("host_log"), &["log".to_string()]).is_ok());
        // ...and the core body ABI is always allowed regardless of capabilities.
        assert!(validate_imports(&module_with_import("host_body_read"), &[]).is_ok());
        assert!(validate_imports(&module_with_import("host_set_output"), &[]).is_ok());
    }
}
