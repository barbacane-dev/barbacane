//! Per-request context via the `host_context_get` / `host_context_set` imports
//! (capabilities `context_get` and `context_set`).
//!
//! The context is a per-request key-value map shared along the middleware
//! chain and with the dispatcher. Auth plugins write the identity keys
//! `auth.sub` (consumer id) and `auth.groups` (comma-separated groups) after a
//! successful authentication, so later plugins can read them as
//! `context:auth.sub` / `context:auth.groups` instead of trusting headers.
//!
//! On non-wasm targets (unit tests) the map is a thread-local store, so a test
//! can [`set`] a value, run the code under test, and inspect it with [`get`];
//! [`clear`] resets it between tests.
//!
//! ```
//! use barbacane_plugin_sdk::context;
//! context::set("auth.sub", "alice");
//! assert_eq!(context::get("auth.sub").as_deref(), Some("alice"));
//! context::clear();
//! ```

/// Key under which auth plugins publish the consumer id.
pub const AUTH_SUB: &str = "auth.sub";

/// Key under which auth plugins publish the comma-separated groups.
pub const AUTH_GROUPS: &str = "auth.groups";

/// Read a context value set earlier in the chain.
#[cfg(target_arch = "wasm32")]
pub fn get(key: &str) -> Option<String> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_get(key_ptr: i32, key_len: i32) -> i32;
        fn host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    unsafe {
        let len = host_context_get(key.as_ptr() as i32, key.len() as i32);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let read = host_context_read_result(buf.as_mut_ptr() as i32, len);
        if read != len {
            return None;
        }
        String::from_utf8(buf).ok()
    }
}

/// Write a context value for plugins later in the chain and the dispatcher.
#[cfg(target_arch = "wasm32")]
pub fn set(key: &str, value: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
    }
    unsafe {
        host_context_set(
            key.as_ptr() as i32,
            key.len() as i32,
            value.as_ptr() as i32,
            value.len() as i32,
        );
    }
}

/// No-op on wasm: the host owns the map and clears it per request.
#[cfg(target_arch = "wasm32")]
pub fn clear() {}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        pub(super) static CONTEXT: RefCell<BTreeMap<String, String>> =
            const { RefCell::new(BTreeMap::new()) };
    }
}

/// Read a value from the thread-local test store.
#[cfg(not(target_arch = "wasm32"))]
pub fn get(key: &str) -> Option<String> {
    native::CONTEXT.with(|c| c.borrow().get(key).cloned())
}

/// Write a value to the thread-local test store.
#[cfg(not(target_arch = "wasm32"))]
pub fn set(key: &str, value: &str) {
    native::CONTEXT.with(|c| {
        c.borrow_mut().insert(key.to_string(), value.to_string());
    });
}

/// Empty the thread-local test store.
#[cfg(not(target_arch = "wasm32"))]
pub fn clear() {
    native::CONTEXT.with(|c| c.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_store_round_trips_and_clears() {
        clear();
        assert_eq!(get(AUTH_SUB), None);
        set(AUTH_SUB, "alice");
        set(AUTH_GROUPS, "admin,editor");
        assert_eq!(get(AUTH_SUB).as_deref(), Some("alice"));
        assert_eq!(get(AUTH_GROUPS).as_deref(), Some("admin,editor"));
        set(AUTH_SUB, "bob");
        assert_eq!(get(AUTH_SUB).as_deref(), Some("bob"));
        clear();
        assert_eq!(get(AUTH_SUB), None);
    }
}
