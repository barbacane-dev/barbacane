//! The WASI functions a plugin may import.
//!
//! The gateway provides its own host functions (module `barbacane`) and these
//! WASI functions (module `wasi_snapshot_preview1`), nothing else. A plugin
//! importing anything outside them is refused at compile time (E1043) and when
//! the data plane loads it. The set is part of the plugin interface;
//! `barbacane_wasm::WASI_IMPORTS` holds the same list next to the stubs that
//! implement it.

/// The module WASI functions are imported from.
pub const WASI_MODULE: &str = "wasi_snapshot_preview1";

/// The WASI functions the gateway provides.
pub const WASI_IMPORTS: [&str; 7] = [
    "random_get",
    "clock_time_get",
    "fd_write",
    "sched_yield",
    "environ_get",
    "environ_sizes_get",
    "proc_exit",
];

/// The first import in `wasm` the gateway does not provide, as
/// `(module, name)`.
pub fn unsupported_import(wasm: &[u8]) -> Result<Option<(String, String)>, String> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|e| format!("not a WebAssembly module: {e}"))?;
        if let wasmparser::Payload::ImportSection(reader) = payload {
            for import in reader.into_imports() {
                let import = import.map_err(|e| format!("unreadable import: {e}"))?;
                let provided = match import.module {
                    "barbacane" => true,
                    WASI_MODULE => WASI_IMPORTS.contains(&import.name),
                    _ => false,
                };
                if !provided {
                    return Ok(Some((import.module.to_string(), import.name.to_string())));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module_importing(imports: &str) -> Vec<u8> {
        wat::parse_str(format!("(module {imports} (memory (export \"memory\") 1))")).unwrap()
    }

    #[test]
    fn the_provided_imports_pass() {
        let wasm = module_importing(
            r#"(import "barbacane" "host_set_output" (func (param i32 i32)))
               (import "wasi_snapshot_preview1" "random_get" (func (param i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "proc_exit" (func (param i32)))"#,
        );
        assert_eq!(unsupported_import(&wasm), Ok(None));
    }

    #[test]
    fn a_wasi_function_outside_the_set_is_named() {
        let wasm = module_importing(
            r#"(import "wasi_snapshot_preview1" "random_get" (func (param i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "fd_close" (func (param i32) (result i32)))"#,
        );
        assert_eq!(
            unsupported_import(&wasm),
            Ok(Some((WASI_MODULE.to_string(), "fd_close".to_string())))
        );
    }

    #[test]
    fn another_module_is_named() {
        let wasm = module_importing(r#"(import "env" "abort" (func))"#);
        assert_eq!(
            unsupported_import(&wasm),
            Ok(Some(("env".to_string(), "abort".to_string())))
        );
    }

    #[test]
    fn bytes_that_are_not_a_module_are_an_error() {
        assert!(unsupported_import(b"not wasm").is_err());
    }
}
