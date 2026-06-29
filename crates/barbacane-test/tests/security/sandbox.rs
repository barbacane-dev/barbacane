//! BARB-SEC-004 — Plugin sandbox / capability confinement.
//!
//! Threat: the WASM linker registers ALL host functions
//! (`add_host_functions` in `crates/barbacane-wasm/src/instance.rs`) regardless
//! of what a plugin's manifest grants. A plugin whose manifest declares only the
//! `log` capability can therefore still *import and call* `host_get_secret`,
//! `host_http_call`, etc. — capability declarations are advisory at link time.
//!
//! Today the only enforcement is `validate_imports`
//! (`crates/barbacane-wasm/src/validate.rs`), which rejects a module whose
//! imports are not covered by its declared capabilities. That is load-time
//! import validation, not a per-plugin linker. The hardening goal is a
//! per-plugin, default-deny linker: a plugin only gets host functions for the
//! capabilities it was granted, so even a module that smuggles an import past
//! validation has nothing to call.
//!
//! ## Why this is expressed but `#[ignore]`d
//!
//! Asserting default-deny end-to-end requires either:
//!   * a purpose-built adversarial WASM plugin that declares `log` only but
//!     attempts `host_http_call` / `host_get_secret` (not present in the repo,
//!     and building wasm in the integration harness is out of scope), or
//!   * driving `barbacane-wasm` internals directly — but `barbacane-test` does
//!     not (and per the additive constraint must not be made to) depend on
//!     `barbacane-wasm`, and the linker entry points are private.
//!
//! So we document the intent and leave the test `#[ignore]`d with a precise
//! note on what the maintainer must expose / build. The `validate_imports`
//! positive control below *can* run today if exposed; it is also gated because
//! the crate dependency is missing.

/// A plugin granted only `log` must not be able to reach any other host
/// function. With a per-plugin default-deny linker, an adversarial module that
/// declares `capabilities = [log]` but imports `host_http_call` must fail to
/// instantiate (or the call must trap), never silently succeed.
#[tokio::test]
#[ignore = "BLOCKED: needs (a) an adversarial fixture plugin declaring only `log` but importing host_http_call, and (b) a per-plugin default-deny linker in barbacane-wasm (currently add_host_functions registers ALL host fns). See BARB-SEC-004."]
async fn log_only_plugin_cannot_call_other_host_functions() {
    // EXPECTED TO FAIL until BARB-SEC-004 is fixed.
    //
    // Intended shape (once the adversarial plugin + per-plugin linker exist):
    //   1. Compile a spec that bundles the `log-only-but-calls-http` plugin.
    //   2. Boot the gateway; route a request through that plugin.
    //   3. Assert the host call is denied — the plugin's attempt to invoke
    //      host_http_call must trap/error, not perform the outbound request.
}

/// Load-time positive control: a module whose imports exceed its declared
/// capabilities must be rejected by `validate_imports`.
#[tokio::test]
#[ignore = "BLOCKED: barbacane-test does not depend on barbacane-wasm; to run this, the maintainer must add barbacane-wasm as a dev-dependency and keep validate::validate_imports public. See BARB-SEC-004."]
async fn imports_exceeding_declared_capabilities_are_rejected() {
    // EXPECTED TO FAIL TO COMPILE without the barbacane-wasm dev-dependency.
    //
    // Intended body:
    //   let module = Module::new(&engine, adversarial_wasm)?;
    //   let result = barbacane_wasm::validate::validate_imports(
    //       &module,
    //       &["log".to_string()], // declares only `log`
    //   );
    //   assert!(result.is_err(), "module importing host_http_call under a
    //           log-only manifest must be rejected");
}
