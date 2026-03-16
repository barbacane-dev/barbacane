//! Proc macros for building Barbacane WASM plugins.
//!
//! This crate provides the `#[barbacane_middleware]` and `#[barbacane_dispatcher]`
//! attribute macros that generate the necessary WASM exports for plugin entry points.
//!
//! # Example
//!
//! ```ignore
//! use barbacane_plugin_sdk::prelude::*;
//!
//! #[barbacane_middleware]
//! struct RateLimiter {
//!     quota: u32,
//!     window: u32,
//! }
//!
//! impl RateLimiter {
//!     fn on_request(&mut self, req: Request) -> Action<Request> {
//!         // Rate limiting logic
//!         Action::Continue(req)
//!     }
//!
//!     fn on_response(&mut self, resp: Response) -> Response {
//!         resp
//!     }
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemStruct};

/// Generates WASM exports for a middleware plugin.
///
/// The annotated struct must implement:
/// - `fn on_request(&mut self, req: Request) -> Action<Request>`
/// - `fn on_response(&mut self, resp: Response) -> Response`
///
/// The macro generates:
/// - `init(ptr, len) -> i32` - Initialize with JSON config
/// - `on_request(ptr, len) -> i32` - Process request (0=continue, 1=short-circuit)
/// - `on_response(ptr, len) -> i32` - Process response
///
/// Bodies travel via side-channel host functions (host_body_read/host_body_set),
/// not embedded in JSON. The glue code handles this transparently.
#[proc_macro_attribute]
pub fn barbacane_middleware(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;

    let expanded = quote! {
        #input

        // WASM ABI glue — only compiled when targeting wasm32.
        // On native targets (e.g. cargo test), only the struct and its
        // impl blocks are compiled, enabling unit testing of plugin logic.
        #[cfg(target_arch = "wasm32")]
        mod __barbacane_wasm_abi {
            use super::*;

            // Global state for the plugin instance
            static mut PLUGIN_INSTANCE: Option<#struct_name> = None;

            /// Initialize the plugin with JSON config.
            #[no_mangle]
            pub extern "C" fn init(ptr: i32, len: i32) -> i32 {
                let config_bytes = unsafe {
                    core::slice::from_raw_parts(ptr as *const u8, len as usize)
                };

                match serde_json::from_slice::<#struct_name>(config_bytes) {
                    Ok(instance) => {
                        unsafe { PLUGIN_INSTANCE = Some(instance); }
                        0 // Success
                    }
                    Err(_) => 1 // Failed to parse config
                }
            }

            /// Lightweight serialization wrapper — avoids the heap cost of
            /// an intermediate `serde_json::Value` tree.
            #[derive(serde::Serialize)]
            struct ActionOutput<T: serde::Serialize> { action: i32, data: T }

            /// Process an incoming request.
            /// Returns 0 to continue, 1 to short-circuit with response.
            #[no_mangle]
            pub extern "C" fn on_request(ptr: i32, len: i32) -> i32 {
                let request_bytes = unsafe {
                    core::slice::from_raw_parts(ptr as *const u8, len as usize)
                };

                let mut request: barbacane_plugin_sdk::prelude::Request = match serde_json::from_slice(request_bytes) {
                    Ok(r) => r,
                    Err(_) => return 1, // Parse error, short-circuit
                };

                // Free the input buffer — data is now owned by `request`.
                dealloc(ptr, len);

                // Read body from side-channel (host_body_read).
                request.body = barbacane_plugin_sdk::body::read_request_body();

                let instance = unsafe {
                    match PLUGIN_INSTANCE.as_mut() {
                        Some(i) => i,
                        None => return 1, // Not initialized
                    }
                };

                match instance.on_request(request) {
                    barbacane_plugin_sdk::prelude::Action::Continue(mut req) => {
                        // Extract body and send via side-channel.
                        match req.body.take() {
                            Some(body) => barbacane_plugin_sdk::body::set_response_body(&body),
                            None => barbacane_plugin_sdk::body::clear_response_body(),
                        }
                        if let Ok(output) = serde_json::to_vec(&ActionOutput { action: 0, data: req }) {
                            set_output(&output);
                        }
                        0 // Continue
                    }
                    barbacane_plugin_sdk::prelude::Action::ShortCircuit(mut resp) => {
                        // Extract body and send via side-channel.
                        match resp.body.take() {
                            Some(body) => barbacane_plugin_sdk::body::set_response_body(&body),
                            None => barbacane_plugin_sdk::body::clear_response_body(),
                        }
                        if let Ok(output) = serde_json::to_vec(&ActionOutput { action: 1, data: resp }) {
                            set_output(&output);
                        }
                        1 // Short-circuit
                    }
                }
            }

            /// Process an outgoing response.
            #[no_mangle]
            pub extern "C" fn on_response(ptr: i32, len: i32) -> i32 {
                let response_bytes = unsafe {
                    core::slice::from_raw_parts(ptr as *const u8, len as usize)
                };

                let mut response: barbacane_plugin_sdk::prelude::Response = match serde_json::from_slice(response_bytes) {
                    Ok(r) => r,
                    Err(_) => return 1,
                };

                // Free the input buffer.
                dealloc(ptr, len);

                // Read body from side-channel (host_body_read).
                response.body = barbacane_plugin_sdk::body::read_request_body();

                let instance = unsafe {
                    match PLUGIN_INSTANCE.as_mut() {
                        Some(i) => i,
                        None => return 1,
                    }
                };

                let mut result = instance.on_response(response);

                // Extract body and send via side-channel.
                match result.body.take() {
                    Some(body) => barbacane_plugin_sdk::body::set_response_body(&body),
                    None => barbacane_plugin_sdk::body::clear_response_body(),
                }

                if let Ok(output) = serde_json::to_vec(&result) {
                    set_output(&output);
                }

                0
            }

            /// Allocate `size` bytes via the plugin's allocator and return the pointer.
            ///
            /// Called by the host before writing input data into linear memory so that
            /// dlmalloc is aware of the allocation and will not reuse the region.
            #[no_mangle]
            pub extern "C" fn alloc(size: i32) -> i32 {
                let mut buf = Vec::<u8>::with_capacity(size as usize);
                let ptr = buf.as_mut_ptr();
                core::mem::forget(buf);
                ptr as i32
            }

            /// Free a region previously returned by `alloc`.
            #[no_mangle]
            pub extern "C" fn dealloc(ptr: i32, size: i32) {
                unsafe {
                    drop(Vec::from_raw_parts(ptr as *mut u8, 0, size as usize));
                }
            }

            /// Helper to set output via host function.
            fn set_output(data: &[u8]) {
                #[link(wasm_import_module = "barbacane")]
                extern "C" {
                    fn host_set_output(ptr: i32, len: i32);
                }
                unsafe {
                    host_set_output(data.as_ptr() as i32, data.len() as i32);
                }
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates WASM exports for a dispatcher plugin.
///
/// The annotated struct must implement:
/// - `fn dispatch(&mut self, req: Request) -> Response`
///
/// The macro generates:
/// - `init(ptr, len) -> i32` - Initialize with JSON config
/// - `dispatch(ptr, len) -> i32` - Handle request and return response
///
/// Bodies travel via side-channel host functions (host_body_read/host_body_set),
/// not embedded in JSON. The glue code handles this transparently.
#[proc_macro_attribute]
pub fn barbacane_dispatcher(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;

    let expanded = quote! {
        #input

        // WASM ABI glue — only compiled when targeting wasm32.
        #[cfg(target_arch = "wasm32")]
        mod __barbacane_wasm_abi {
            use super::*;

            // Global state for the plugin instance
            static mut PLUGIN_INSTANCE: Option<#struct_name> = None;

            /// Initialize the plugin with JSON config.
            #[no_mangle]
            pub extern "C" fn init(ptr: i32, len: i32) -> i32 {
                let config_bytes = unsafe {
                    core::slice::from_raw_parts(ptr as *const u8, len as usize)
                };

                match serde_json::from_slice::<#struct_name>(config_bytes) {
                    Ok(instance) => {
                        unsafe { PLUGIN_INSTANCE = Some(instance); }
                        0 // Success
                    }
                    Err(_) => 1 // Failed to parse config
                }
            }

            /// Dispatch a request and return a response.
            #[no_mangle]
            pub extern "C" fn dispatch(ptr: i32, len: i32) -> i32 {
                let request_bytes = unsafe {
                    core::slice::from_raw_parts(ptr as *const u8, len as usize)
                };

                let mut request: barbacane_plugin_sdk::prelude::Request = match serde_json::from_slice(request_bytes) {
                    Ok(r) => r,
                    Err(_) => {
                        // Free input buffer before returning error.
                        dealloc(ptr, len);
                        let error_resp = barbacane_plugin_sdk::prelude::Response {
                            status: 500,
                            headers: std::collections::BTreeMap::new(),
                            body: Some(br#"{"error":"failed to parse request"}"#.to_vec()),
                        };
                        // Send error body via side-channel.
                        if let Some(ref body) = error_resp.body {
                            barbacane_plugin_sdk::body::set_response_body(body);
                        }
                        let mut err_resp_for_json = error_resp;
                        err_resp_for_json.body = None;
                        if let Ok(output) = serde_json::to_vec(&err_resp_for_json) {
                            set_output(&output);
                        }
                        return 1;
                    }
                };

                // Free the input buffer — data is now owned by `request`.
                dealloc(ptr, len);

                // Read body from side-channel (host_body_read).
                request.body = barbacane_plugin_sdk::body::read_request_body();

                let instance = unsafe {
                    match PLUGIN_INSTANCE.as_mut() {
                        Some(i) => i,
                        None => {
                            let error_body = br#"{"error":"plugin not initialized"}"#;
                            barbacane_plugin_sdk::body::set_response_body(error_body);
                            let error_resp = barbacane_plugin_sdk::prelude::Response {
                                status: 500,
                                headers: std::collections::BTreeMap::new(),
                                body: None,
                            };
                            if let Ok(output) = serde_json::to_vec(&error_resp) {
                                set_output(&output);
                            }
                            return 1;
                        }
                    }
                };

                let mut response = instance.dispatch(request);

                // Extract body and send via side-channel.
                match response.body.take() {
                    Some(body) => barbacane_plugin_sdk::body::set_response_body(&body),
                    None => barbacane_plugin_sdk::body::clear_response_body(),
                }

                if let Ok(output) = serde_json::to_vec(&response) {
                    set_output(&output);
                }

                0
            }

            /// Allocate `size` bytes via the plugin's allocator and return the pointer.
            #[no_mangle]
            pub extern "C" fn alloc(size: i32) -> i32 {
                let mut buf = Vec::<u8>::with_capacity(size as usize);
                let ptr = buf.as_mut_ptr();
                core::mem::forget(buf);
                ptr as i32
            }

            /// Free a region previously returned by `alloc`.
            #[no_mangle]
            pub extern "C" fn dealloc(ptr: i32, size: i32) {
                unsafe {
                    drop(Vec::from_raw_parts(ptr as *mut u8, 0, size as usize));
                }
            }

            /// Helper to set output via host function.
            fn set_output(data: &[u8]) {
                #[link(wasm_import_module = "barbacane")]
                extern "C" {
                    fn host_set_output(ptr: i32, len: i32);
                }
                unsafe {
                    host_set_output(data.as_ptr() as i32, data.len() as i32);
                }
            }
        }
    };

    TokenStream::from(expanded)
}
