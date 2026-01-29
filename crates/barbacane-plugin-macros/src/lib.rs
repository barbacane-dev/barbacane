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
/// The plugin must call `host_set_output` to return data to the host.
#[proc_macro_attribute]
pub fn barbacane_middleware(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;

    let expanded = quote! {
        #input

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

        /// Process an incoming request.
        /// Returns 0 to continue, 1 to short-circuit with response.
        #[no_mangle]
        pub extern "C" fn on_request(ptr: i32, len: i32) -> i32 {
            let request_bytes = unsafe {
                core::slice::from_raw_parts(ptr as *const u8, len as usize)
            };

            let request: barbacane_plugin_sdk::prelude::Request = match serde_json::from_slice(request_bytes) {
                Ok(r) => r,
                Err(_) => return 1, // Parse error, short-circuit
            };

            let instance = unsafe {
                match PLUGIN_INSTANCE.as_mut() {
                    Some(i) => i,
                    None => return 1, // Not initialized
                }
            };

            match instance.on_request(request) {
                barbacane_plugin_sdk::prelude::Action::Continue(req) => {
                    // Serialize the request and set output
                    if let Ok(output) = serde_json::to_vec(&serde_json::json!({
                        "action": 0,
                        "data": req
                    })) {
                        set_output(&output);
                    }
                    0 // Continue
                }
                barbacane_plugin_sdk::prelude::Action::ShortCircuit(resp) => {
                    // Serialize the response and set output
                    if let Ok(output) = serde_json::to_vec(&serde_json::json!({
                        "action": 1,
                        "data": resp
                    })) {
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

            let response: barbacane_plugin_sdk::prelude::Response = match serde_json::from_slice(response_bytes) {
                Ok(r) => r,
                Err(_) => return 1,
            };

            let instance = unsafe {
                match PLUGIN_INSTANCE.as_mut() {
                    Some(i) => i,
                    None => return 1,
                }
            };

            let result = instance.on_response(response);

            if let Ok(output) = serde_json::to_vec(&result) {
                set_output(&output);
            }

            0
        }

        /// Helper to set output via host function.
        fn set_output(data: &[u8]) {
            extern "C" {
                fn host_set_output(ptr: i32, len: i32);
            }
            unsafe {
                host_set_output(data.as_ptr() as i32, data.len() as i32);
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
/// The plugin must call `host_set_output` to return the response to the host.
#[proc_macro_attribute]
pub fn barbacane_dispatcher(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;

    let expanded = quote! {
        #input

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

            let request: barbacane_plugin_sdk::prelude::Request = match serde_json::from_slice(request_bytes) {
                Ok(r) => r,
                Err(_) => {
                    // Return error response
                    let error_resp = barbacane_plugin_sdk::prelude::Response {
                        status: 500,
                        headers: std::collections::HashMap::new(),
                        body: Some(r#"{"error":"failed to parse request"}"#.to_string()),
                    };
                    if let Ok(output) = serde_json::to_vec(&error_resp) {
                        set_output(&output);
                    }
                    return 1;
                }
            };

            let instance = unsafe {
                match PLUGIN_INSTANCE.as_mut() {
                    Some(i) => i,
                    None => {
                        let error_resp = barbacane_plugin_sdk::prelude::Response {
                            status: 500,
                            headers: std::collections::HashMap::new(),
                            body: Some(r#"{"error":"plugin not initialized"}"#.to_string()),
                        };
                        if let Ok(output) = serde_json::to_vec(&error_resp) {
                            set_output(&output);
                        }
                        return 1;
                    }
                }
            };

            let response = instance.dispatch(request);

            if let Ok(output) = serde_json::to_vec(&response) {
                set_output(&output);
            }

            0
        }

        /// Helper to set output via host function.
        fn set_output(data: &[u8]) {
            extern "C" {
                fn host_set_output(ptr: i32, len: i32);
            }
            unsafe {
                host_set_output(data.as_ptr() as i32, data.len() as i32);
            }
        }
    };

    TokenStream::from(expanded)
}
