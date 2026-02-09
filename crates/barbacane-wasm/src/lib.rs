//! WASM plugin runtime for Barbacane API gateway.
//!
//! This crate provides the wasmtime-based runtime for loading and executing
//! WASM plugins (middlewares and dispatchers) according to SPEC-003.

// Allow dead code for M3 host function scaffolding not yet integrated
#![allow(dead_code)]

pub mod broker;
pub mod cache;
mod chain;
mod circuit_breaker;
mod engine;
mod error;
mod host;
mod http_client;
mod instance;
pub mod kafka_client;
mod limits;
mod manifest;
pub mod nats_client;
mod pool;
pub mod rate_limiter;
mod schema;
pub mod secrets;
mod trap;
mod validate;
pub mod version;

pub use chain::{
    execute_on_request, execute_on_request_with_metrics, execute_on_response,
    execute_on_response_partial, execute_on_response_with_metrics, ChainResult, MetricsCallback,
    MiddlewareChain, MiddlewareConfig, OnRequestResult,
};
pub use engine::WasmEngine;
pub use error::WasmError;
pub use instance::{PluginInstance, RequestContext};
pub use limits::PluginLimits;
pub use manifest::{Capabilities, PluginManifest, PluginMeta, PluginType};
pub use pool::{InstanceKey, InstancePool};
pub use schema::ConfigSchema;
pub use secrets::{
    collect_secret_references, is_secret_reference, resolve_all_secrets, resolve_config_secrets,
    resolve_secret, SecretsError, SecretsStore,
};
pub use trap::{TrapContext, TrapResult};
pub use validate::{validate_exports, validate_imports};

// Rate limiter for host_rate_limit_check
pub use rate_limiter::{RateLimitResult, RateLimiter, RateLimiterStats};

// Response cache for host_cache_get/set
pub use cache::{CacheEntry, CacheResult, CacheStats, ResponseCache};

// HTTP client for host_http_call
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
pub use http_client::{
    HttpClient, HttpClientConfig, HttpClientError, HttpRequest, HttpResponse, TlsConfig,
    TlsConfigError,
};

// Message broker types for event dispatch
pub use broker::{BrokerError, BrokerMessage, PublishResult};

// Kafka publisher for host_kafka_publish
pub use kafka_client::KafkaPublisher;

// NATS publisher for host_nats_publish
pub use nats_client::NatsPublisher;

/// Re-export plugin SDK types for convenience.
pub use barbacane_plugin_sdk::prelude::{Action, Request, Response};
