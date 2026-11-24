//! Barbacane data plane library.
//!
//! The entry point is the `barbacane` binary. These modules are public so the
//! integration tests can reach them; they are not a stable API
//! ([stability policy](https://docs.barbacane.dev/reference/stability.html)).

#[doc(hidden)]
pub mod admin;
#[doc(hidden)]
pub mod client_ip;
#[doc(hidden)]
pub mod coercion;
#[doc(hidden)]
pub mod control;
#[doc(hidden)]
pub mod control_plane;
#[doc(hidden)]
pub mod dev;
#[doc(hidden)]
pub mod hot_reload;
#[doc(hidden)]
pub mod lifecycle;
#[doc(hidden)]
pub mod metered_body;
#[doc(hidden)]
pub mod params;
#[doc(hidden)]
pub mod router;
#[doc(hidden)]
pub mod tls;
#[doc(hidden)]
pub mod validator;

#[cfg(test)]
mod tests {
    /// A call's budget never passes what a plugin may declare: the runtime's
    /// ceilings are the compiler's `E1044` ceilings.
    #[test]
    fn the_call_budget_ceilings_are_the_declared_ceilings() {
        assert_eq!(
            barbacane_wasm::MAX_CALL_FUEL,
            barbacane_compiler::limits::MAX_FUEL
        );
        assert_eq!(
            barbacane_wasm::MAX_CALL_TIMEOUT_MS,
            barbacane_compiler::limits::MAX_TIMEOUT_MS
        );
    }
}
