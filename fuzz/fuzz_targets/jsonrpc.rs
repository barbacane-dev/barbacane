//! Fuzz the MCP JSON-RPC request parser.
//!
//! Target: deserialization of `barbacane_lib::mcp::jsonrpc::JsonRpcRequest`
//! (`crates/barbacane/src/mcp/jsonrpc.rs`), which is the wire-format type the
//! MCP endpoint parses every incoming request into via serde_json.
//!
//! The MCP endpoint accepts untrusted client JSON, so the parse step must never
//! panic or hang regardless of input: deeply nested params, gigantic numbers,
//! duplicate keys, non-UTF-8-escaped strings, etc.
//!
//! Invariant: parsing arbitrary bytes returns `Ok`/`Err`, never panics.
//!
//! Run: `cargo +nightly fuzz run jsonrpc`

#![no_main]

use libfuzzer_sys::fuzz_target;

use barbacane_lib::mcp::jsonrpc::JsonRpcRequest;

fuzz_target!(|data: &[u8]| {
    // Parse exactly as the MCP endpoint does: serde_json over the raw bytes.
    let _ = serde_json::from_slice::<JsonRpcRequest>(data);
});
