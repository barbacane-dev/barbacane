//! Fuzz the WASM guest-slice read helper (ptr/len bounds checking).
//!
//! ## Status: BLOCKED — documented, intentionally inert.
//!
//! The guest-memory read pattern we want to fuzz lives INLINE inside every host
//! function closure registered by `add_host_functions`
//! (`crates/barbacane-wasm/src/instance.rs`), e.g.:
//!
//! ```ignore
//! let memory = caller.get_export("memory").and_then(|e| e.into_memory())?;
//! let data = memory.data(&caller);
//! if end > data.len() { return -1; }      // <-- the bounds check
//! let slice = &data[start..end];
//! ```
//!
//! There is NO standalone, public `fn read_guest_slice(ptr, len, mem_len) -> ..`
//! to call: the check is duplicated per host function and is only reachable with
//! a live `wasmtime::Caller` + instantiated module + linear memory. Reaching it
//! from a libFuzzer target would mean instantiating a full plugin instance per
//! input, which is far too slow for fuzzing and pulls the entire runtime into
//! the harness.
//!
//! ### Maintainer action to ENABLE this target
//!
//! Extract the bounds arithmetic into a pure, public, side-effect-free helper in
//! `barbacane-wasm`, e.g.:
//!
//! ```ignore
//! /// Returns the valid `[start, end)` range, or an error if out of bounds.
//! pub fn guest_slice_bounds(ptr: i32, len: i32, mem_len: usize)
//!     -> Result<(usize, usize), GuestMemoryError>;
//! ```
//!
//! and have every host function call it. Then this target becomes:
//!
//! ```ignore
//! use arbitrary::Arbitrary;
//! #[derive(Arbitrary, Debug)]
//! struct In { ptr: i32, len: i32, mem_len: u32 }
//! fuzz_target!(|i: In| {
//!     let _ = barbacane_wasm::guest_slice_bounds(i.ptr, i.len, i.mem_len as usize);
//! });
//! ```
//!
//! and `barbacane-wasm = { path = "../crates/barbacane-wasm" }` must be added to
//! `fuzz/Cargo.toml`. Until that helper is exposed, this target compiles and
//! runs but exercises nothing, so the framework stays buildable without an
//! src-side change.
//!
//! Run: `cargo +nightly fuzz run wasm_host_memory` (currently a no-op).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|_data: &[u8]| {
    // Intentionally inert. See the module docs above: the guest-slice bounds
    // check is not reachable as a pure function without an src change in
    // barbacane-wasm. This stub keeps the fuzz crate building with all five
    // targets declared while clearly flagging the blocker for the maintainer.
});
