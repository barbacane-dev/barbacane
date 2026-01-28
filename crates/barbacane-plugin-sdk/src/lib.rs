//! SDK for building Barbacane WASM plugins.
//!
//! Provides `Request`, `Response`, and `Action` types, along with
//! `#[barbacane_middleware]` and `#[barbacane_dispatcher]` macros
//! that generate the required WASM export glue.
//!
//! This crate is a stub until M3 (WASM Plugin System).

pub mod types;

pub mod prelude {
    pub use crate::types::*;
}
