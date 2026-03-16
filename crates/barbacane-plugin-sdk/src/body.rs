//! Side-channel body helpers for WASM plugins.
//!
//! Bodies travel as raw bytes via dedicated host functions instead of
//! base64-encoded inside JSON. This eliminates the ~3.65x memory overhead
//! per boundary crossing and allows 10MB+ bodies within the default WASM
//! memory limit.
//!
//! Plugin authors typically don't call these functions directly — the proc
//! macros (`#[barbacane_dispatcher]`, `#[barbacane_middleware]`) handle the
//! side-channel protocol transparently. These helpers are available for
//! plugins that need direct control (e.g. http-upstream's outbound HTTP calls).

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    fn host_body_len() -> i64;
    fn host_body_read(ptr: i32, len: i32) -> i32;
    fn host_body_set(ptr: i32, len: i32);
    fn host_body_clear();
    fn host_http_response_body_len() -> i64;
    fn host_http_response_body_read(ptr: i32, len: i32) -> i32;
    fn host_http_request_body_set(ptr: i32, len: i32);
}

/// Read the request/response body from the host side-channel.
///
/// Returns `None` if no body was set by the host.
#[cfg(target_arch = "wasm32")]
pub fn read_request_body() -> Option<Vec<u8>> {
    let len = unsafe { host_body_len() };
    if len < 0 {
        return None;
    }
    let len = len as usize;
    if len == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; len];
    let copied = unsafe { host_body_read(buf.as_mut_ptr() as i32, len as i32) };
    buf.truncate(copied as usize);
    Some(buf)
}

/// Set the output body via the host side-channel.
///
/// Call this to return a body from dispatch/on_request/on_response
/// without embedding it in JSON.
#[cfg(target_arch = "wasm32")]
pub fn set_response_body(body: &[u8]) {
    unsafe { host_body_set(body.as_ptr() as i32, body.len() as i32) }
}

/// Explicitly set the output body to None.
#[cfg(target_arch = "wasm32")]
pub fn clear_response_body() {
    unsafe { host_body_clear() }
}

/// Read the HTTP response body from the last `host_http_call`.
///
/// Returns `None` if the response had no body.
#[cfg(target_arch = "wasm32")]
pub fn read_http_response_body() -> Option<Vec<u8>> {
    let len = unsafe { host_http_response_body_len() };
    if len < 0 {
        return None;
    }
    let len = len as usize;
    if len == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; len];
    let copied = unsafe { host_http_response_body_read(buf.as_mut_ptr() as i32, len as i32) };
    buf.truncate(copied as usize);
    Some(buf)
}

/// Set the outbound HTTP request body for the next `host_http_call`.
///
/// This sends raw bytes to the host without base64 encoding.
#[cfg(target_arch = "wasm32")]
pub fn set_http_request_body(body: &[u8]) {
    unsafe { host_http_request_body_set(body.as_ptr() as i32, body.len() as i32) }
}

// --- Native stubs for cargo test (non-WASM targets) ---

#[cfg(not(target_arch = "wasm32"))]
pub fn read_request_body() -> Option<Vec<u8>> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub fn set_response_body(_body: &[u8]) {}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_response_body() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_http_response_body() -> Option<Vec<u8>> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub fn set_http_request_body(_body: &[u8]) {}
