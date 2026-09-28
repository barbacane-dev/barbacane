//! `host_sha256` called from a real plugin instance, through the linker.
//!
//! Each module's `dispatch` calls `host_sha256` with fixed arguments, returns
//! its result code, and passes the digest out with `host_set_output`. The fuel
//! and deadline tests pair the call with a control module doing per-byte work
//! in WASM under the same budget, so a pass shows the budget was one that
//! mattered.

use barbacane_wasm::{PluginInstance, PluginLimits, WasmEngine, WasmError};

const PAGE: usize = 64 * 1024;
/// Where modules keep their result: the code, then the digest.
const OUT: usize = 64;
/// Where input bytes placed by a data segment start.
const DATA: usize = 1024;

struct Call {
    data_ptr: i64,
    data_len: i64,
    out_ptr: i64,
    pages: usize,
    segment: &'static [u8],
}

impl Call {
    fn at(data_ptr: usize, data_len: usize, pages: usize) -> Self {
        Self {
            data_ptr: data_ptr as i64,
            data_len: data_len as i64,
            out_ptr: (OUT + 4) as i64,
            pages,
            segment: b"",
        }
    }
}

fn wat_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

fn hashing_module(call: &Call) -> String {
    format!(
        r#"(module
  (import "barbacane" "host_sha256" (func $sha256 (param i32 i32 i32) (result i32)))
  (import "barbacane" "host_set_output" (func $set_output (param i32 i32)))
  (memory (export "memory") {pages})
  (data (i32.const {DATA}) "{segment}")
  (func (export "alloc") (param i32) (result i32) (i32.const 512))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32)
    (local $rc i32) (local $i i32)
    (local.set $rc (call $sha256 (i32.const {dp}) (i32.const {dl}) (i32.const {op})))
    ;; Carry on in WASM afterwards, as a plugin does once it has its digest:
    ;; the loop's back-edge is where the runtime checks the call's deadline.
    (block $done
      (loop $next
        (br_if $done (i32.ge_u (local.get $i) (i32.const 16)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $next)))
    (i32.store (i32.const {OUT}) (local.get $rc))
    (call $set_output (i32.const {OUT}) (i32.const 36))
    (local.get $rc))
)"#,
        pages = call.pages,
        segment = wat_bytes(call.segment),
        dp = call.data_ptr,
        dl = call.data_len,
        op = call.out_ptr,
    )
}

/// Reads every byte of `[DATA, DATA + len)` in WASM, `passes` times: the
/// per-byte work a software hash does, at a fraction of its cost.
fn per_byte_module(len: usize, pages: usize, passes: u32) -> String {
    format!(
        r#"(module
  (memory (export "memory") {pages})
  (func (export "alloc") (param i32) (result i32) (i32.const 512))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32)
    (local $pass i32) (local $i i32) (local $acc i32)
    (block $all_done
      (loop $next_pass
        (br_if $all_done (i32.ge_u (local.get $pass) (i32.const {passes})))
        (local.set $i (i32.const 0))
        (block $done
          (loop $next
            (br_if $done (i32.ge_u (local.get $i) (i32.const {len})))
            (local.set $acc (i32.add (local.get $acc)
              (i32.load8_u (i32.add (i32.const {DATA}) (local.get $i)))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $next)))
        (local.set $pass (i32.add (local.get $pass) (i32.const 1)))
        (br $next_pass)))
    (local.get $acc))
)"#
    )
}

fn instance(engine: &WasmEngine, wat: &str, limits: PluginLimits) -> PluginInstance {
    let wasm = wat::parse_str(wat).expect("valid WAT");
    let module = engine
        .compile(&wasm, "hash-test".into(), "0.1.0".into(), false)
        .expect("compile");
    PluginInstance::new(engine.engine(), &module, limits).expect("instantiate")
}

/// The result code and, when it is 0, the digest.
fn run(call: &Call, limits: PluginLimits) -> Result<(i32, Vec<u8>), WasmError> {
    let engine = WasmEngine::new().expect("engine");
    let mut plugin = instance(&engine, &hashing_module(call), limits);
    let rc = plugin.dispatch(b"{}")?;
    let output = plugin.take_output();
    assert_eq!(output.len(), 36, "code and digest");
    assert_eq!(i32::from_le_bytes(output[..4].try_into().unwrap()), rc);
    Ok((rc, output[4..].to_vec()))
}

fn sha256(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .to_vec()
}

// ── Results ──────────────────────────────────────────────────────────────────

#[test]
fn hashes_the_bytes_it_is_pointed_at() {
    let call = Call {
        segment: b"abc",
        ..Call::at(DATA, 3, 1)
    };
    let (rc, digest) = run(&call, PluginLimits::default()).expect("dispatch");
    assert_eq!(rc, 0);
    assert_eq!(digest, sha256(b"abc"));
}

#[test]
fn hashes_an_empty_range() {
    let (rc, digest) = run(&Call::at(DATA, 0, 1), PluginLimits::default()).expect("dispatch");
    assert_eq!(rc, 0);
    assert_eq!(digest, sha256(b""));
}

#[test]
fn hashes_the_whole_memory_up_to_its_last_byte() {
    let pages = 4;
    let len = pages * PAGE - DATA;
    let (rc, digest) = run(&Call::at(DATA, len, pages), PluginLimits::default()).expect("dispatch");
    assert_eq!(rc, 0);
    assert_eq!(digest, sha256(&vec![0u8; len]));
}

// ── Refused calls return -1 and leave the plugin running ────────────────────

#[test]
fn a_range_past_the_end_of_memory_returns_minus_one() {
    let len = PAGE - DATA + 1;
    let (rc, _) = run(&Call::at(DATA, len, 1), PluginLimits::default()).expect("no trap");
    assert_eq!(rc, -1);
}

#[test]
fn a_digest_that_would_not_fit_returns_minus_one() {
    let call = Call {
        out_ptr: (PAGE - 31) as i64,
        ..Call::at(DATA, 3, 1)
    };
    let (rc, _) = run(&call, PluginLimits::default()).expect("no trap");
    assert_eq!(rc, -1);
}

#[test]
fn negative_arguments_return_minus_one() {
    for call in [
        Call {
            data_ptr: -1,
            ..Call::at(DATA, 3, 1)
        },
        Call {
            data_len: -1,
            ..Call::at(DATA, 3, 1)
        },
        Call {
            out_ptr: -1,
            ..Call::at(DATA, 3, 1)
        },
    ] {
        let (rc, _) = run(&call, PluginLimits::default()).expect("no trap");
        assert_eq!(rc, -1);
    }
}

// ── Budgets ──────────────────────────────────────────────────────────────────

/// 12 MiB: over sixty times the input at which a software hash ran out of fuel.
const LARGE: usize = 12 * 1024 * 1024;
const LARGE_PAGES: usize = 256;

fn tight_fuel() -> PluginLimits {
    PluginLimits {
        max_fuel: 100_000,
        ..PluginLimits::default()
    }
}

#[test]
fn hashing_a_large_input_costs_no_fuel_that_grows_with_it() {
    let (rc, digest) = run(&Call::at(DATA, LARGE, LARGE_PAGES), tight_fuel()).expect("dispatch");
    assert_eq!(rc, 0);
    assert_eq!(digest, sha256(&vec![0u8; LARGE]));
}

#[test]
fn control_touching_each_byte_in_wasm_exhausts_that_fuel() {
    let engine = WasmEngine::new().expect("engine");
    let mut plugin = instance(
        &engine,
        &per_byte_module(LARGE, LARGE_PAGES, 1),
        tight_fuel(),
    );
    let err = plugin.dispatch(b"{}").expect_err("the budget must run out");
    assert!(err.to_string().to_lowercase().contains("fuel"), "{err}");
}

/// The call's time budget in the deadline tests. Generous next to the work a
/// module does after hashing, so a busy machine does not trip it there.
const DEADLINE_MS: u64 = 30;

/// An input the host takes at least three deadlines to hash on this machine,
/// so hashing overruns the budget unless the host refreshes it. Grows from
/// `LARGE` on fast machines, up to a cap.
fn input_slower_than_the_deadline() -> usize {
    use sha2::{Digest, Sha256};
    let mut len = LARGE;
    loop {
        let data = vec![0u8; len];
        let started = std::time::Instant::now();
        let _ = Sha256::digest(&data);
        if started.elapsed() >= std::time::Duration::from_millis(3 * DEADLINE_MS)
            || len >= 256 << 20
        {
            return len;
        }
        len *= 2;
    }
}

fn deadline_limits(pages: usize) -> PluginLimits {
    PluginLimits {
        max_execution_ms: DEADLINE_MS,
        max_memory_bytes: pages * PAGE,
        ..PluginLimits::default()
    }
}

#[test]
fn hashing_a_large_input_does_not_spend_the_plugins_time_budget() {
    let len = input_slower_than_the_deadline();
    let pages = (DATA + len).div_ceil(PAGE);
    let (rc, _) = run(&Call::at(DATA, len, pages), deadline_limits(pages)).expect("dispatch");
    assert_eq!(rc, 0);
}

#[test]
fn control_touching_each_byte_in_wasm_overruns_that_deadline() {
    let engine = WasmEngine::new().expect("engine");
    let limits = PluginLimits {
        max_fuel: u64::MAX / 2,
        ..deadline_limits(LARGE_PAGES)
    };
    // Enough passes to take far longer than the deadline on any machine.
    let mut plugin = instance(&engine, &per_byte_module(LARGE, LARGE_PAGES, 1_000), limits);
    let err = plugin.dispatch(b"{}").expect_err("the deadline must pass");
    assert!(
        matches!(err, WasmError::Timeout(_)),
        "trapped on time, not fuel: {err}"
    );
}
