//! Test harnesses for Barbacane gateway and plugins.
//!
//! Provides `TestGateway` for full-stack integration tests
//! and `PluginHarness` for isolated WASM plugin testing.

pub mod gateway;

pub use gateway::{TestError, TestGateway};
