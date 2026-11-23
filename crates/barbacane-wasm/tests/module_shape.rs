//! A plugin module holds one linear memory, one table and no GC heap, so the
//! memory and table limits cover everything an instance can hold.

use barbacane_wasm::{PluginInstance, PluginLimits, WasmEngine};

const EXPORTS: &str = r#"
  (func (export "alloc") (param i32) (result i32) (i32.const 512))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))"#;

fn module(body: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory (export \"memory\") 1) {body} {EXPORTS})"
    ))
    .expect("valid WAT")
}

fn instantiate(engine: &WasmEngine, body: &str) -> Result<PluginInstance, String> {
    let compiled = engine
        .compile(&module(body), "shape".into(), "0.1.0".into(), false)
        .map_err(|e| format!("compile: {e}"))?;
    PluginInstance::new(&compiled, PluginLimits::default()).map_err(|e| format!("instantiate: {e}"))
}

#[test]
fn a_plugin_with_one_memory_and_one_table_runs() {
    let engine = WasmEngine::new().expect("engine");
    instantiate(&engine, "(table 4 funcref)").expect("one memory, one table");
}

#[test]
fn features_that_add_memories_or_heaps_are_refused() {
    let engine = WasmEngine::new().expect("engine");
    for (feature, body) in [
        ("multi-memory", "(memory 1)"),
        ("memory64", "(memory i64 1)"),
        ("gc", "(type (struct (field i32)))"),
        ("exceptions", "(tag $t)"),
    ] {
        let error = instantiate(&engine, body)
            .err()
            .unwrap_or_else(|| panic!("{feature} was accepted"));
        assert!(error.starts_with("compile:"), "{feature}: {error}");
    }
}

#[test]
fn a_second_table_is_refused() {
    let engine = WasmEngine::new().expect("engine");
    let error = instantiate(&engine, "(table 1 funcref) (table 1 funcref)")
        .err()
        .expect("two tables were accepted");
    assert!(error.contains("table"), "{error}");
}
