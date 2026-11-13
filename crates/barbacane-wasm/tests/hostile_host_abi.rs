//! Every host function called with pointers and lengths a plugin controls:
//! negative, past the end of memory, straddling it, and lengths near
//! `i32::MAX`. A host function must refuse such arguments (an error code, a
//! no-op, or a trap); it must not panic, since a panic unwinds out of the
//! plugin call into the gateway's worker thread.
//!
//! Each case builds a module whose `dispatch` makes the one call and returns 0.

use std::panic::{catch_unwind, AssertUnwindSafe};

use barbacane_wasm::{PluginInstance, PluginLimits, WasmEngine, WasmError};

/// One page of memory: the first address past it.
const MEM_END: i64 = 64 * 1024;

/// A parameter of a host function, for building its arguments.
#[derive(Clone, Copy)]
enum P {
    /// A pointer into plugin memory.
    Ptr,
    /// A length paired with the pointer before it.
    Len,
    /// Any other `i32` (a level, a file descriptor, a clock id, a count).
    I32,
    /// An `i64` argument.
    I64,
    /// An `f64` argument.
    F64,
}

/// Module, name, parameters, and whether the function returns a value.
type HostFn = (
    &'static str,
    &'static str,
    &'static [P],
    Option<&'static str>,
);

const HOST_FNS: &[HostFn] = &[
    ("barbacane", "host_set_output", &[P::Ptr, P::Len], None),
    ("barbacane", "host_body_set", &[P::Ptr, P::Len], None),
    (
        "barbacane",
        "host_http_request_body_set",
        &[P::Ptr, P::Len],
        None,
    ),
    (
        "barbacane",
        "host_sha256",
        &[P::Ptr, P::Len, P::Ptr],
        Some("i32"),
    ),
    ("barbacane", "host_log", &[P::I32, P::Ptr, P::Len], None),
    (
        "barbacane",
        "host_context_get",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_context_set",
        &[P::Ptr, P::Len, P::Ptr, P::Len],
        None,
    ),
    (
        "barbacane",
        "host_http_call",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_http_pool_call",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_http_stream",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_ws_upgrade",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_verify_signature",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_get_secret",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_rate_limit_check",
        &[P::Ptr, P::Len, P::I32, P::I32],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_cache_get",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_cache_set",
        &[P::Ptr, P::Len, P::Ptr, P::Len, P::I32],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_metric_counter_inc",
        &[P::Ptr, P::Len, P::Ptr, P::Len, P::F64],
        None,
    ),
    (
        "barbacane",
        "host_metric_histogram_observe",
        &[P::Ptr, P::Len, P::Ptr, P::Len, P::F64],
        None,
    ),
    (
        "barbacane",
        "host_span_start",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_span_set_attribute",
        &[P::Ptr, P::Len, P::Ptr, P::Len],
        None,
    ),
    (
        "barbacane",
        "host_kafka_publish",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_nats_publish",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_ldap_bind",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_ldap_search",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_body_read",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_http_response_body_read",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_context_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_uuid_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_http_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_secret_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_rate_limit_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_cache_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_broker_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "barbacane",
        "host_ldap_read_result",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "wasi_snapshot_preview1",
        "random_get",
        &[P::Ptr, P::Len],
        Some("i32"),
    ),
    (
        "wasi_snapshot_preview1",
        "clock_time_get",
        &[P::I32, P::I64, P::Ptr],
        Some("i32"),
    ),
    (
        "wasi_snapshot_preview1",
        "fd_write",
        &[P::I32, P::Ptr, P::Len, P::Ptr],
        Some("i32"),
    ),
    (
        "wasi_snapshot_preview1",
        "environ_get",
        &[P::Ptr, P::Ptr],
        Some("i32"),
    ),
    (
        "wasi_snapshot_preview1",
        "environ_sizes_get",
        &[P::Ptr, P::Ptr],
        Some("i32"),
    ),
];

/// Pointer and length values for one case.
const CASES: &[(&str, i64, i64)] = &[
    ("negative pointer, huge length", -1, i32::MAX as i64),
    ("pointer at the end of memory", MEM_END, 1),
    ("range straddling the end of memory", MEM_END - 1, 2),
    ("negative length", 0, -1),
    ("huge pointer and length", i32::MAX as i64, i32::MAX as i64),
    (
        "pointer and length that overflow together",
        MEM_END - 8,
        i32::MAX as i64,
    ),
];

fn wasm_type(p: P) -> &'static str {
    match p {
        P::Ptr | P::Len | P::I32 => "i32",
        P::I64 => "i64",
        P::F64 => "f64",
    }
}

fn arg(p: P, ptr: i64, len: i64) -> String {
    match p {
        P::Ptr => format!("(i32.const {})", ptr as i32),
        P::Len => format!("(i32.const {})", len as i32),
        P::I32 => "(i32.const 1)".to_string(),
        P::I64 => "(i64.const 1)".to_string(),
        P::F64 => "(f64.const 1)".to_string(),
    }
}

/// A dispatcher whose `dispatch` makes the one call. A `*_read_result` is
/// preceded by `host_uuid_generate`, so `host_uuid_read_result` has a pending
/// result to write.
fn module(f: &HostFn, ptr: i64, len: i64) -> String {
    let (module, name, params, result) = f;
    let param_types: Vec<&str> = params.iter().map(|p| wasm_type(*p)).collect();
    let sig = format!(
        "(param {}){}",
        param_types.join(" "),
        result.map(|r| format!(" (result {r})")).unwrap_or_default()
    );
    let args: Vec<String> = params.iter().map(|p| arg(*p, ptr, len)).collect();
    let call = format!("(call $f {})", args.join(" "));
    let call = if result.is_some() {
        format!("(drop {call})")
    } else {
        call
    };
    format!(
        r#"(module
  (import "{module}" "{name}" (func $f {sig}))
  (import "barbacane" "host_uuid_generate" (func $uuid (result i32)))
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32)
    (drop (call $uuid))
    {call}
    (i32.const 0)))"#
    )
}

/// Whether one call returned or trapped (both fine), or panicked.
fn run(engine: &WasmEngine, wat: &str) -> Result<Result<i32, WasmError>, String> {
    let wasm = wat::parse_str(wat).expect("valid WAT");
    let module = engine
        .compile(&wasm, "hostile".into(), "0.1.0".into(), false)
        .expect("compile");
    let mut instance =
        PluginInstance::new(engine.engine(), &module, PluginLimits::default()).expect("instance");
    catch_unwind(AssertUnwindSafe(|| instance.dispatch(b"{}"))).map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "non-string panic".to_string())
    })
}

#[test]
fn no_host_function_panics_on_hostile_pointers() {
    let engine = WasmEngine::new().expect("engine");
    let mut panics = Vec::new();
    let mut calls = 0;
    for f in HOST_FNS {
        for (case, ptr, len) in CASES {
            calls += 1;
            if let Err(panic) = run(&engine, &module(f, *ptr, *len)) {
                panics.push(format!("{}::{} with {case}: {panic}", f.0, f.1));
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} of {calls} calls panicked:\n{}",
        panics.len(),
        panics.join("\n")
    );
}

/// Positive control: the harness reports a panic as one, so a pass above
/// means none happened.
#[test]
fn the_harness_sees_a_panic() {
    let caught = catch_unwind(|| panic!("boom")).map_err(|p| p.downcast_ref::<&str>().copied());
    assert_eq!(caught, Err(Some("boom")));
}

/// Positive control: a well-formed call through the same harness returns.
#[test]
fn a_well_formed_call_returns() {
    let engine = WasmEngine::new().expect("engine");
    let wat = module(&HOST_FNS[0], 0, 4);
    assert!(matches!(run(&engine, &wat), Ok(Ok(0))));
}
