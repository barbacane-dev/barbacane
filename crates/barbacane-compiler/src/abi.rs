//! The plugin ABI: the version a plugin is built for, the exports the gateway
//! calls, and the host functions on their way out.
//!
//! A plugin built with `barbacane-plugin-sdk` carries its ABI version in a
//! `barbacane_abi` custom section (a little-endian `u32`), embedded by the
//! plugin macros. The compiler refuses a plugin without the section or with
//! another version (E1045), and the data plane refuses it again at load. A
//! plugin importing a deprecated host function compiles with a warning
//! (E1016) and loads with a warning in the gateway's log, until the function is
//! removed in the release the deprecation names.
//!
//! `barbacane_wasm` holds the same lists next to the host functions that
//! implement them.

/// The custom section holding the plugin's ABI version.
pub const ABI_SECTION: &str = "barbacane_abi";

/// The ABI version this compiler and its gateway speak.
pub const ABI_VERSION: u32 = 1;

/// Host functions a plugin may still import, each with its replacement and
/// the earliest release that may drop it.
pub const DEPRECATED_IMPORTS: &[DeprecatedImport] = &[DeprecatedImport {
    name: "host_time_now",
    replacement: "host_clock_now",
    removal: "1.2.0",
}];

/// A host function on its way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeprecatedImport {
    /// The import's name in the `barbacane` module.
    pub name: &'static str,
    /// What to import instead.
    pub replacement: &'static str,
    /// The earliest release that may remove it.
    pub removal: &'static str,
}

/// The exports the gateway calls on a plugin of `plugin_type` (`middleware`
/// or `dispatcher`). `barbacane_wasm::PluginType::required_exports` holds the
/// same lists.
pub fn required_exports(plugin_type: &str) -> &'static [&'static str] {
    match plugin_type {
        "middleware" => &["memory", "alloc", "init", "on_request", "on_response"],
        "dispatcher" => &["memory", "alloc", "init", "dispatch"],
        _ => &["memory", "alloc", "init"],
    }
}

/// Why a module is not a plugin this gateway runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiMismatch {
    /// No `barbacane_abi` section: built with an SDK before 1.0, or not with
    /// the SDK at all.
    NoVersion,
    /// A `barbacane_abi` section that is not a little-endian `u32`.
    MalformedVersion,
    /// Built for another ABI version.
    Version(u32),
    /// An export the gateway calls is missing.
    MissingExport(String),
}

impl std::fmt::Display for AbiMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVersion => write!(f, "it carries no plugin ABI version"),
            Self::MalformedVersion => write!(f, "its `{ABI_SECTION}` section is malformed"),
            Self::Version(v) => write!(
                f,
                "it is built for plugin ABI {v}, and this release supports ABI {ABI_VERSION}"
            ),
            Self::MissingExport(name) => write!(f, "it does not export `{name}`"),
        }
    }
}

/// The ABI version `wasm` carries, if any.
pub fn abi_version(wasm: &[u8]) -> Result<Option<u32>, String> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|e| format!("not a WebAssembly module: {e}"))?;
        if let wasmparser::Payload::CustomSection(section) = payload {
            if section.name() == ABI_SECTION {
                let bytes: [u8; 4] = section
                    .data()
                    .try_into()
                    .map_err(|_| AbiMismatch::MalformedVersion.to_string())?;
                return Ok(Some(u32::from_le_bytes(bytes)));
            }
        }
    }
    Ok(None)
}

/// Why `wasm` is not a plugin of `plugin_type` this gateway runs, or `None`
/// when it is one.
pub fn check(wasm: &[u8], plugin_type: &str) -> Result<Option<AbiMismatch>, String> {
    let mut version = None;
    let mut exports = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|e| format!("not a WebAssembly module: {e}"))?;
        match payload {
            wasmparser::Payload::CustomSection(section) if section.name() == ABI_SECTION => {
                let Ok(bytes) = <[u8; 4]>::try_from(section.data()) else {
                    return Ok(Some(AbiMismatch::MalformedVersion));
                };
                version = Some(u32::from_le_bytes(bytes));
            }
            wasmparser::Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.map_err(|e| format!("unreadable export: {e}"))?;
                    exports.push(export.name.to_string());
                }
            }
            _ => {}
        }
    }
    match version {
        None => return Ok(Some(AbiMismatch::NoVersion)),
        Some(ABI_VERSION) => {}
        Some(other) => return Ok(Some(AbiMismatch::Version(other))),
    }
    for required in required_exports(plugin_type) {
        if !exports.iter().any(|name| name == required) {
            return Ok(Some(AbiMismatch::MissingExport((*required).to_string())));
        }
    }
    Ok(None)
}

/// The deprecated host functions `wasm` imports.
pub fn deprecated_imports(wasm: &[u8]) -> Result<Vec<DeprecatedImport>, String> {
    let mut found = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|e| format!("not a WebAssembly module: {e}"))?;
        if let wasmparser::Payload::ImportSection(reader) = payload {
            for import in reader.into_imports() {
                let import = import.map_err(|e| format!("unreadable import: {e}"))?;
                if import.module != "barbacane" {
                    continue;
                }
                if let Some(d) = DEPRECATED_IMPORTS.iter().find(|d| d.name == import.name) {
                    found.push(*d);
                }
            }
        }
    }
    Ok(found)
}

/// The smallest module that passes [`check`] for `plugin_type`: the ABI
/// section and the required exports, every function returning 0 (so `init`
/// succeeds) or 1024 (`alloc`).
#[cfg(test)]
pub(crate) fn test_module(plugin_type: &str) -> Vec<u8> {
    let exports = required_exports(plugin_type)
        .iter()
        .filter(|name| **name != "memory")
        .map(|name| match *name {
            "alloc" => {
                r#"(func (export "alloc") (param i32) (result i32) (i32.const 1024))"#.to_string()
            }
            other => {
                format!(r#"(func (export "{other}") (param i32 i32) (result i32) (i32.const 0))"#)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    wat::parse_str(format!(
        r#"(module (@custom "barbacane_abi" "\01\00\00\00") (memory (export "memory") 1) {exports})"#
    ))
    .expect("valid WAT")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPATCHER_EXPORTS: &str = r#"
        (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) (i32.const 1024))
        (func (export "init") (param i32 i32) (result i32) (i32.const 0))
        (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))"#;

    fn module(body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module {body})")).unwrap()
    }

    #[test]
    fn a_plugin_of_this_abi_passes() {
        let wasm = module(&format!(
            r#"(@custom "barbacane_abi" "\01\00\00\00") {DISPATCHER_EXPORTS}"#
        ));
        assert_eq!(abi_version(&wasm), Ok(Some(1)));
        assert_eq!(check(&wasm, "dispatcher"), Ok(None));
    }

    #[test]
    fn a_plugin_without_a_version_is_named() {
        let wasm = module(DISPATCHER_EXPORTS);
        assert_eq!(abi_version(&wasm), Ok(None));
        assert_eq!(check(&wasm, "dispatcher"), Ok(Some(AbiMismatch::NoVersion)));
    }

    #[test]
    fn another_version_is_named() {
        let wasm = module(&format!(
            r#"(@custom "barbacane_abi" "\02\00\00\00") {DISPATCHER_EXPORTS}"#
        ));
        assert_eq!(
            check(&wasm, "dispatcher"),
            Ok(Some(AbiMismatch::Version(2)))
        );
        assert_eq!(
            AbiMismatch::Version(2).to_string(),
            "it is built for plugin ABI 2, and this release supports ABI 1"
        );
    }

    #[test]
    fn a_malformed_version_is_named() {
        let wasm = module(&format!(
            r#"(@custom "barbacane_abi" "1") {DISPATCHER_EXPORTS}"#
        ));
        assert_eq!(
            check(&wasm, "dispatcher"),
            Ok(Some(AbiMismatch::MalformedVersion))
        );
        assert!(abi_version(&wasm).is_err());
    }

    #[test]
    fn a_missing_export_is_named() {
        let wasm = module(
            r#"(@custom "barbacane_abi" "\01\00\00\00")
               (memory (export "memory") 1)
               (func (export "init") (param i32 i32) (result i32) (i32.const 0))
               (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))"#,
        );
        assert_eq!(
            check(&wasm, "dispatcher"),
            Ok(Some(AbiMismatch::MissingExport("alloc".into())))
        );
        let wasm = module(&format!(
            r#"(@custom "barbacane_abi" "\01\00\00\00") {DISPATCHER_EXPORTS}"#
        ));
        assert_eq!(
            check(&wasm, "middleware"),
            Ok(Some(AbiMismatch::MissingExport("on_request".into())))
        );
    }

    #[test]
    fn deprecated_imports_are_listed() {
        let wasm = module(
            r#"(import "barbacane" "host_time_now" (func (result i64)))
               (import "barbacane" "host_clock_now" (func (result i64)))
               (memory (export "memory") 1)"#,
        );
        let found = deprecated_imports(&wasm).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "host_time_now");
        assert_eq!(found[0].replacement, "host_clock_now");
        assert_eq!(found[0].removal, "1.2.0");
    }

    #[test]
    fn not_a_module_is_an_error() {
        assert!(check(b"nope", "dispatcher").is_err());
    }
}
