//! WASM plugin runtime for Barbacane API gateway.
//!
//! This crate provides the wasmtime-based runtime for loading and executing
//! WASM plugins (middlewares and dispatchers) according to SPEC-003.

// Allow dead code for M3 host function scaffolding not yet integrated
#![allow(dead_code)]

mod chain;
mod circuit_breaker;
mod engine;
mod error;
mod host;
mod http_client;
mod instance;
mod limits;
mod manifest;
mod pool;
mod schema;
mod trap;
mod validate;
pub mod version;

pub use chain::{
    execute_on_request, execute_on_response, execute_on_response_partial, ChainResult,
    MiddlewareChain, MiddlewareConfig, OnRequestResult,
};
pub use engine::WasmEngine;
pub use error::WasmError;
pub use instance::{PluginInstance, RequestContext};
pub use limits::PluginLimits;
pub use manifest::{Capabilities, PluginManifest, PluginMeta, PluginType};
pub use pool::InstancePool;
pub use schema::ConfigSchema;
pub use trap::{TrapContext, TrapResult};
pub use validate::{validate_exports, validate_imports};

// HTTP client for host_http_call
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use http_client::{HttpClient, HttpClientConfig, HttpClientError, HttpRequest, HttpResponse};

/// Re-export plugin SDK types for convenience.
pub use barbacane_plugin_sdk::prelude::{Action, Request, Response};
