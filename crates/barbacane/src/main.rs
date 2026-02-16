//! Barbacane API gateway.
//!
//! Compiles OpenAPI specs into artifacts and runs the data plane server.

use barbacane_lib::{control_plane, hot_reload};

use std::convert::Infallible;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use bytes::Bytes;
use clap::{Parser, Subcommand};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Incoming};
use hyper::header::HeaderValue;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

/// Server version for the Server header.
const SERVER_VERSION: &str = concat!("barbacane/", env!("CARGO_PKG_VERSION"));

use barbacane_telemetry::MetricsRegistry;
use std::collections::HashMap;

use barbacane_compiler::{
    compile_with_manifest, load_manifest, load_plugins, load_routes, load_specs, CompileOptions,
    CompiledOperation, Manifest, ProjectManifest,
};
use barbacane_lib::router::{RouteEntry, RouteMatch, Router};
use barbacane_lib::validator::{
    OperationValidator, ProblemDetails, RequestLimits, ValidationError2,
};

/// Extract a reason string from a validation error for metrics.
fn validation_error_reason(err: &ValidationError2) -> String {
    match err {
        ValidationError2::MissingRequiredParameter { .. } => {
            "missing_required_parameter".to_string()
        }
        ValidationError2::InvalidParameter { .. } => "invalid_parameter".to_string(),
        ValidationError2::MissingRequiredBody => "missing_required_body".to_string(),
        ValidationError2::UnsupportedContentType(_) => "unsupported_content_type".to_string(),
        ValidationError2::InvalidBody { .. } => "invalid_body".to_string(),
        ValidationError2::BodyTooLarge { .. } => "body_too_large".to_string(),
        ValidationError2::TooManyHeaders { .. } => "too_many_headers".to_string(),
        ValidationError2::HeaderTooLarge { .. } => "header_too_large".to_string(),
        ValidationError2::UriTooLong { .. } => "uri_too_long".to_string(),
    }
}

/// Recursively remove all keys starting with "x-barbacane-" from a JSON value.
/// Preserves standard OpenAPI/AsyncAPI fields and the x-sunset extension (RFC 8594).
fn strip_barbacane_keys_recursive(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // Remove x-barbacane-* keys
            map.retain(|k, _| !k.starts_with("x-barbacane-"));
            // Recurse into remaining values
            for v in map.values_mut() {
                strip_barbacane_keys_recursive(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                strip_barbacane_keys_recursive(item);
            }
        }
        _ => {}
    }
}

/// Detected spec type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecType {
    OpenApi,
    AsyncApi,
    Unknown,
}

/// Detect whether a spec is OpenAPI or AsyncAPI by checking root keys.
fn detect_spec_type(content: &str) -> SpecType {
    // Try to parse as YAML (also handles JSON)
    let parsed: Result<serde_json::Value, _> = serde_yaml::from_str(content);
    match parsed {
        Ok(value) => {
            if value.get("openapi").is_some() {
                SpecType::OpenApi
            } else if value.get("asyncapi").is_some() {
                SpecType::AsyncApi
            } else {
                SpecType::Unknown
            }
        }
        Err(_) => SpecType::Unknown,
    }
}

/// Merge multiple OpenAPI specs into one.
/// Combines paths, components, and uses the first spec's info as base.
fn merge_openapi_specs(specs: &[(&String, &String)]) -> serde_json::Value {
    let mut merged = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Merged API",
            "version": "1.0.0"
        },
        "paths": {},
        "components": {
            "schemas": {},
            "securitySchemes": {},
            "parameters": {},
            "responses": {},
            "headers": {},
            "requestBodies": {}
        }
    });

    let mut titles = Vec::new();

    for (filename, content) in specs {
        let parsed: Option<serde_json::Value> = serde_yaml::from_str(content).ok();
        if let Some(mut spec) = parsed {
            // Strip barbacane extensions
            strip_barbacane_keys_recursive(&mut spec);

            // Collect title for merged info
            if let Some(title) = spec.pointer("/info/title").and_then(|t| t.as_str()) {
                titles.push(title.to_string());
            }

            // Use first spec's info as base
            if titles.len() == 1 {
                if let Some(info) = spec.get("info") {
                    merged["info"] = info.clone();
                }
                if let Some(version) = spec.get("openapi") {
                    merged["openapi"] = version.clone();
                }
            }

            // Merge paths
            if let Some(paths) = spec.get("paths").and_then(|p| p.as_object()) {
                let merged_paths = merged["paths"]
                    .as_object_mut()
                    .expect("json macro produces object");
                for (path, methods) in paths {
                    merged_paths.insert(path.clone(), methods.clone());
                }
            }

            // Merge components
            if let Some(components) = spec.get("components").and_then(|c| c.as_object()) {
                let merged_components = merged["components"]
                    .as_object_mut()
                    .expect("json macro produces object");
                for (component_type, items) in components {
                    if let Some(items_obj) = items.as_object() {
                        let target = merged_components
                            .entry(component_type.clone())
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(target_obj) = target.as_object_mut() {
                            for (name, value) in items_obj {
                                // Prefix with source filename to avoid conflicts
                                let key = if specs.len() > 1 && target_obj.contains_key(name) {
                                    let base = filename
                                        .trim_end_matches(".yaml")
                                        .trim_end_matches(".json");
                                    format!("{}_{}", base, name)
                                } else {
                                    name.clone()
                                };
                                target_obj.insert(key, value.clone());
                            }
                        }
                    }
                }
            }

            // Merge servers
            if let Some(servers) = spec.get("servers").and_then(|s| s.as_array()) {
                let merged_servers = merged
                    .as_object_mut()
                    .expect("json macro produces object")
                    .entry("servers")
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(arr) = merged_servers.as_array_mut() {
                    for server in servers {
                        if !arr.contains(server) {
                            arr.push(server.clone());
                        }
                    }
                }
            }

            // Merge tags
            if let Some(tags) = spec.get("tags").and_then(|t| t.as_array()) {
                let merged_tags = merged
                    .as_object_mut()
                    .expect("json macro produces object")
                    .entry("tags")
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(arr) = merged_tags.as_array_mut() {
                    for tag in tags {
                        if !arr.contains(tag) {
                            arr.push(tag.clone());
                        }
                    }
                }
            }
        }
    }

    // Update title if multiple specs were merged
    if titles.len() > 1 {
        merged["info"]["title"] = serde_json::json!(titles.join(" + "));
    }

    // Clean up empty component sections
    if let Some(components) = merged.get_mut("components").and_then(|c| c.as_object_mut()) {
        components.retain(|_, v| v.as_object().is_some_and(|o| !o.is_empty()));
    }
    if merged
        .get("components")
        .and_then(|c| c.as_object())
        .is_some_and(|o| o.is_empty())
    {
        merged
            .as_object_mut()
            .expect("json macro produces object")
            .remove("components");
    }

    merged
}

/// Merge multiple AsyncAPI specs into one.
/// Combines channels, operations, components, and uses the first spec's info as base.
fn merge_asyncapi_specs(specs: &[(&String, &String)]) -> serde_json::Value {
    let mut merged = serde_json::json!({
        "asyncapi": "3.0.0",
        "info": {
            "title": "Merged Async API",
            "version": "1.0.0"
        },
        "channels": {},
        "operations": {},
        "components": {
            "schemas": {},
            "messages": {},
            "securitySchemes": {},
            "parameters": {}
        }
    });

    let mut titles = Vec::new();

    for (filename, content) in specs {
        let parsed: Option<serde_json::Value> = serde_yaml::from_str(content).ok();
        if let Some(mut spec) = parsed {
            // Strip barbacane extensions
            strip_barbacane_keys_recursive(&mut spec);

            // Collect title for merged info
            if let Some(title) = spec.pointer("/info/title").and_then(|t| t.as_str()) {
                titles.push(title.to_string());
            }

            // Use first spec's info as base
            if titles.len() == 1 {
                if let Some(info) = spec.get("info") {
                    merged["info"] = info.clone();
                }
                if let Some(version) = spec.get("asyncapi") {
                    merged["asyncapi"] = version.clone();
                }
            }

            // Merge channels
            if let Some(channels) = spec.get("channels").and_then(|c| c.as_object()) {
                let merged_channels = merged["channels"]
                    .as_object_mut()
                    .expect("json macro produces object");
                for (name, channel) in channels {
                    // Prefix with source filename to avoid conflicts
                    let key = if specs.len() > 1 && merged_channels.contains_key(name) {
                        let base = filename.trim_end_matches(".yaml").trim_end_matches(".json");
                        format!("{}_{}", base, name)
                    } else {
                        name.clone()
                    };
                    merged_channels.insert(key, channel.clone());
                }
            }

            // Merge operations
            if let Some(operations) = spec.get("operations").and_then(|o| o.as_object()) {
                let merged_ops = merged["operations"]
                    .as_object_mut()
                    .expect("json macro produces object");
                for (name, op) in operations {
                    let key = if specs.len() > 1 && merged_ops.contains_key(name) {
                        let base = filename.trim_end_matches(".yaml").trim_end_matches(".json");
                        format!("{}_{}", base, name)
                    } else {
                        name.clone()
                    };
                    merged_ops.insert(key, op.clone());
                }
            }

            // Merge components
            if let Some(components) = spec.get("components").and_then(|c| c.as_object()) {
                let merged_components = merged["components"]
                    .as_object_mut()
                    .expect("json macro produces object");
                for (component_type, items) in components {
                    if let Some(items_obj) = items.as_object() {
                        let target = merged_components
                            .entry(component_type.clone())
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(target_obj) = target.as_object_mut() {
                            for (name, value) in items_obj {
                                let key = if specs.len() > 1 && target_obj.contains_key(name) {
                                    let base = filename
                                        .trim_end_matches(".yaml")
                                        .trim_end_matches(".json");
                                    format!("{}_{}", base, name)
                                } else {
                                    name.clone()
                                };
                                target_obj.insert(key, value.clone());
                            }
                        }
                    }
                }
            }

            // Merge servers
            if let Some(servers) = spec.get("servers").and_then(|s| s.as_object()) {
                let merged_servers = merged
                    .as_object_mut()
                    .expect("json macro produces object")
                    .entry("servers")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(map) = merged_servers.as_object_mut() {
                    for (name, server) in servers {
                        if !map.contains_key(name) {
                            map.insert(name.clone(), server.clone());
                        }
                    }
                }
            }
        }
    }

    // Update title if multiple specs were merged
    if titles.len() > 1 {
        merged["info"]["title"] = serde_json::json!(titles.join(" + "));
    }

    // Clean up empty sections
    let obj = merged.as_object_mut().expect("json macro produces object");
    if obj
        .get("channels")
        .and_then(|c| c.as_object())
        .is_some_and(|c| c.is_empty())
    {
        obj.remove("channels");
    }
    if obj
        .get("operations")
        .and_then(|o| o.as_object())
        .is_some_and(|o| o.is_empty())
    {
        obj.remove("operations");
    }
    if let Some(components) = obj.get_mut("components").and_then(|c| c.as_object_mut()) {
        components.retain(|_, v| v.as_object().is_some_and(|o| !o.is_empty()));
    }
    if obj
        .get("components")
        .and_then(|c| c.as_object())
        .is_some_and(|o| o.is_empty())
    {
        obj.remove("components");
    }

    merged
}
use barbacane_wasm::{
    HttpClient, HttpClientConfig, InstancePool, PluginLimits, RateLimiter, ResponseCache,
    WasmEngine,
};

#[derive(Parser, Debug)]
#[command(name = "barbacane", about = "Barbacane API gateway", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Compile OpenAPI spec(s) into a .bca artifact.
    Compile {
        /// Input spec file(s) (YAML or JSON).
        #[arg(short, long, required = true, num_args = 1..)]
        spec: Vec<String>,

        /// Output artifact path.
        #[arg(short, long)]
        output: String,

        /// Path to barbacane.yaml manifest.
        #[arg(short, long, required = true)]
        manifest: String,

        /// Allow plaintext HTTP upstream URLs (development only).
        #[arg(long)]
        allow_plaintext: bool,
    },

    /// Validate OpenAPI spec(s) without compiling.
    ///
    /// Checks for spec validity (E1001-E1004) and extension validity (E1010-E1014).
    /// E1015 (unknown x-barbacane-* extension) is checked during compile.
    /// Does not resolve plugins or produce an artifact.
    Validate {
        /// Input spec file(s) (YAML or JSON).
        #[arg(short, long, required = true, num_args = 1..)]
        spec: Vec<String>,

        /// Output format (text or json).
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Initialize a new Barbacane project.
    ///
    /// Creates a project directory with barbacane.yaml, spec files, and plugins directory.
    Init {
        /// Project name (creates a directory with this name).
        #[arg(default_value = ".")]
        name: String,

        /// Template to use: basic (full example) or minimal (bare bones).
        #[arg(short, long, default_value = "basic")]
        template: String,

        /// Download official plugins (mock, http-upstream) from GitHub releases.
        #[arg(long)]
        fetch_plugins: bool,
    },

    /// Run the gateway server.
    Serve {
        /// Path to the .bca artifact file.
        #[arg(long)]
        artifact: String,

        /// Listen address.
        #[arg(long, default_value = "0.0.0.0:8080")]
        listen: String,

        /// Enable development mode (verbose errors, detailed logs).
        #[arg(long)]
        dev: bool,

        /// Log level (error, warn, info, debug, trace).
        #[arg(long, default_value = "info")]
        log_level: String,

        /// Log format (json or pretty).
        #[arg(long, default_value = "json")]
        log_format: String,

        /// OTLP endpoint for telemetry export (e.g., http://localhost:4317).
        /// If not set, telemetry is collected locally but not exported.
        #[arg(long)]
        otlp_endpoint: Option<String>,

        /// Trace sampling rate (0.0 to 1.0). Default: 1.0 (100% sampling).
        /// Set to 0.0 to disable tracing, 0.1 for 10% sampling, etc.
        #[arg(long, default_value = "1.0")]
        trace_sampling: f64,

        /// Maximum request body size in bytes (default: 1048576 = 1MB).
        #[arg(long, default_value = "1048576")]
        max_body_size: usize,

        /// Maximum number of request headers (default: 100).
        #[arg(long, default_value = "100")]
        max_headers: usize,

        /// Maximum size of a single header in bytes (default: 8192 = 8KB).
        #[arg(long, default_value = "8192")]
        max_header_size: usize,

        /// Maximum URI length in characters (default: 8192 = 8KB).
        #[arg(long, default_value = "8192")]
        max_uri_length: usize,

        /// Allow plaintext HTTP upstream connections (development only).
        /// In production, only HTTPS upstreams are allowed.
        #[arg(long)]
        allow_plaintext_upstream: bool,

        /// Path to TLS certificate file (PEM format).
        /// If provided, --tls-key must also be specified.
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to TLS private key file (PEM format).
        /// If provided, --tls-cert must also be specified.
        #[arg(long)]
        tls_key: Option<String>,

        /// Minimum TLS version (1.2 or 1.3). Default: 1.2.
        /// Use 1.3 for maximum security (modern clients only).
        #[arg(long, default_value = "1.2")]
        tls_min_version: String,

        /// HTTP keep-alive idle timeout in seconds (default: 60).
        #[arg(long, default_value = "60")]
        keepalive_timeout: u64,

        /// Graceful shutdown timeout in seconds (default: 30).
        /// After SIGTERM, wait this long for in-flight requests to complete.
        #[arg(long, default_value = "30")]
        shutdown_timeout: u64,

        // Connected mode options (optional)
        /// Control plane WebSocket URL (e.g., ws://control:8080/ws/data-plane).
        /// When provided, the data plane connects to the control plane for centralized management.
        #[arg(long)]
        control_plane: Option<String>,

        /// Project ID (UUID) for control plane registration.
        /// Required if --control-plane is specified.
        #[arg(long)]
        project_id: Option<String>,

        /// API key for control plane authentication.
        /// Required if --control-plane is specified.
        #[arg(long, env = "BARBACANE_API_KEY")]
        api_key: Option<String>,

        /// Data plane name for identification in control plane.
        #[arg(long)]
        data_plane_name: Option<String>,
    },
}

// =============================================================================
// Hot-Reload Types
// =============================================================================

/// Shared gateway state that can be atomically swapped for hot-reload.
type SharedGateway = Arc<ArcSwap<Gateway>>;

// Re-export from library for local use
use hot_reload::HotReloadResult;

// =============================================================================
// Gateway
// =============================================================================

/// Shared gateway state.
struct Gateway {
    manifest: Manifest,
    router: Router,
    operations: Vec<CompiledOperation>,
    /// Pre-compiled validators for each operation.
    validators: Vec<OperationValidator>,
    /// Source specs embedded in the artifact (filename -> content).
    specs: HashMap<String, String>,
    /// Request limits (body size, headers, URI length).
    limits: RequestLimits,
    dev_mode: bool,
    /// WASM engine for plugin execution (kept alive for engine lifetime).
    _wasm_engine: Arc<WasmEngine>,
    /// Plugin instance pool.
    plugin_pool: Arc<InstancePool>,
    /// Plugin resource limits (kept for future dynamic limit adjustment).
    _plugin_limits: PluginLimits,
    /// HTTP client for plugins making outbound calls (kept alive for pool lifetime).
    _http_client: Arc<HttpClient>,
    /// Metrics registry for observability.
    metrics: Arc<MetricsRegistry>,
    /// API name from the first spec's title (for metrics labels).
    api_name: String,
    /// Request counter for generating request IDs (fallback if UUID too slow).
    _request_counter: AtomicU64,
}

impl Gateway {
    /// Load a gateway from a .bca artifact.
    fn load(
        artifact_path: &Path,
        dev_mode: bool,
        limits: RequestLimits,
        allow_plaintext_upstream: bool,
        metrics: Arc<MetricsRegistry>,
    ) -> Result<Self, String> {
        let manifest =
            load_manifest(artifact_path).map_err(|e| format!("failed to load manifest: {}", e))?;

        let routes =
            load_routes(artifact_path).map_err(|e| format!("failed to load routes: {}", e))?;

        let specs =
            load_specs(artifact_path).map_err(|e| format!("failed to load specs: {}", e))?;

        // Initialize HTTP client for upstream requests and plugin outbound calls
        let http_client_config = HttpClientConfig {
            allow_plaintext: allow_plaintext_upstream,
            ..Default::default()
        };
        let http_client = HttpClient::new(http_client_config)
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;
        let http_client = Arc::new(http_client);

        // Initialize WASM engine
        let plugin_limits = PluginLimits::default();
        let wasm_engine = WasmEngine::with_limits(plugin_limits.clone())
            .map_err(|e| format!("failed to create WASM engine: {}", e))?;
        let wasm_engine = Arc::new(wasm_engine);

        // Load plugins from the artifact
        let bundled_plugins = load_plugins(artifact_path)
            .map_err(|e| format!("failed to load plugins from artifact: {}", e))?;

        if bundled_plugins.is_empty() {
            tracing::warn!("no plugins bundled in artifact - ensure barbacane.yaml manifest was used during compilation");
        }

        // Compile all plugin modules first (we'll register them after creating the final pool)
        let mut compiled_modules = Vec::new();
        for (name, (version, wasm_bytes)) in bundled_plugins {
            let module = wasm_engine
                .compile(&wasm_bytes, name.clone(), version.clone())
                .map_err(|e| format!("failed to compile plugin '{}': {}", name, e))?;
            compiled_modules.push((name, version, module));
        }

        // Collect all configs to find secret references
        let all_configs: Vec<&serde_json::Value> = routes
            .operations
            .iter()
            .flat_map(|op| {
                let mut configs: Vec<&serde_json::Value> =
                    op.middlewares.iter().map(|m| &m.config).collect();
                configs.push(&op.dispatch.config);
                configs
            })
            .collect();

        // Debug: log all configs being checked for secrets
        if dev_mode {
            for config in &all_configs {
                tracing::debug!(config = %config, "checking config for secret references");
            }
            let refs = all_configs
                .iter()
                .flat_map(|c| barbacane_wasm::collect_secret_references(c))
                .collect::<Vec<_>>();
            tracing::debug!(references = ?refs, "found secret references");
        }

        // Resolve all secrets
        let secrets_store =
            barbacane_wasm::resolve_all_secrets(&all_configs).map_err(|errors| {
                let messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
                format!("failed to resolve secrets: {}", messages.join(", "))
            })?;

        // Replace secret references in route configs with resolved values
        let mut resolved_operations = routes.operations;
        for op in &mut resolved_operations {
            for mw in &mut op.middlewares {
                mw.config = barbacane_wasm::resolve_config_secrets(&mw.config, &secrets_store);
            }
            op.dispatch.config =
                barbacane_wasm::resolve_config_secrets(&op.dispatch.config, &secrets_store);
        }

        // Create rate limiter for host_rate_limit_check calls
        let rate_limiter = RateLimiter::new();

        // Create response cache for host_cache_get/set calls
        let response_cache = ResponseCache::new();

        // Create NATS publisher for host_nats_publish calls
        let nats_publisher = barbacane_wasm::NatsPublisher::new();

        // Create Kafka publisher for host_kafka_publish calls
        let kafka_publisher = barbacane_wasm::KafkaPublisher::new();

        // Create pool with all options: HTTP client, secrets, rate limiter, cache, NATS, and Kafka
        let plugin_pool = InstancePool::with_all_options(
            wasm_engine.clone(),
            plugin_limits.clone(),
            Some(http_client.clone()),
            Some(secrets_store),
            Some(rate_limiter),
            Some(response_cache),
            Some(Arc::new(nats_publisher)),
            Some(Arc::new(kafka_publisher)),
        );

        // Register all compiled modules in the pool
        for (name, version, module) in compiled_modules {
            plugin_pool.register_module(module);
            if dev_mode {
                tracing::debug!(plugin = %name, version = %version, "loaded plugin from artifact");
            }
        }

        let mut router = Router::new();
        let mut validators = Vec::new();

        for op in &resolved_operations {
            // Map AsyncAPI methods to HTTP methods for sync-to-async bridge pattern:
            // - SEND → POST (publish message via HTTP POST, get 202 Accepted)
            // - RECEIVE → GET (for SSE/WebSocket subscriptions, less common)
            let http_method = match op.method.as_str() {
                "SEND" => "POST",
                "RECEIVE" => "GET",
                other => other,
            };
            router.insert(
                &op.path,
                http_method,
                RouteEntry {
                    operation_index: op.index,
                },
            );

            // Pre-compile validator for this operation
            let validator = OperationValidator::new(&op.parameters, op.request_body.as_ref());
            validators.push(validator);

            // Log middleware chain for this operation (informational)
            if !op.middlewares.is_empty() && dev_mode {
                let names: Vec<_> = op.middlewares.iter().map(|m| m.name.as_str()).collect();
                tracing::debug!(
                    path = %op.path,
                    method = %op.method,
                    middlewares = ?names,
                    "configured middleware chain"
                );
            }
        }

        // Extract API name from manifest (first source spec file or "default")
        let api_name = manifest
            .source_specs
            .first()
            .map(|s| {
                // Remove extension and path, just keep the file name
                Path::new(&s.file)
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("default")
                    .to_string()
            })
            .unwrap_or_else(|| "default".to_string());

        Ok(Gateway {
            manifest,
            router,
            operations: resolved_operations,
            validators,
            specs,
            limits,
            dev_mode,
            _wasm_engine: wasm_engine,
            plugin_pool: Arc::new(plugin_pool),
            _plugin_limits: plugin_limits,
            _http_client: http_client,
            metrics,
            api_name,
            _request_counter: AtomicU64::new(0),
        })
    }

    /// Add standard headers to a response.
    ///
    /// Includes:
    /// - Server version
    /// - Request/trace IDs for observability
    /// - Security headers (X-Content-Type-Options, X-Frame-Options)
    fn add_standard_headers(
        mut response: Response<Full<Bytes>>,
        request_id: &str,
        trace_id: &str,
    ) -> Response<Full<Bytes>> {
        let headers = response.headers_mut();

        // Observability headers
        headers.insert("server", HeaderValue::from_static(SERVER_VERSION));
        headers.insert(
            "x-request-id",
            HeaderValue::from_str(request_id).expect("uuid is valid ASCII"),
        );
        headers.insert(
            "x-trace-id",
            HeaderValue::from_str(trace_id).expect("uuid is valid ASCII"),
        );

        // Security headers (enabled by default)
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        headers.insert("x-frame-options", HeaderValue::from_static("DENY"));

        response
    }

    /// Add deprecation headers to a response if the operation is deprecated.
    /// Implements RFC 8594 (Sunset header) and draft-ietf-httpapi-deprecation-header.
    fn add_deprecation_headers(
        mut response: Response<Full<Bytes>>,
        operation: &CompiledOperation,
    ) -> Response<Full<Bytes>> {
        if operation.deprecated {
            let headers = response.headers_mut();
            // Deprecation header per draft-ietf-httpapi-deprecation-header
            // Value "true" indicates the endpoint is deprecated
            headers.insert("deprecation", HeaderValue::from_static("true"));

            // Sunset header per RFC 8594 if a sunset date is specified
            if let Some(sunset_date) = &operation.sunset {
                if let Ok(val) = sunset_date.parse() {
                    headers.insert("sunset", val);
                }
            }
        }
        response
    }

    /// Handle an incoming HTTP request.
    async fn handle_request(
        &self,
        req: Request<Incoming>,
        client_addr: Option<SocketAddr>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let start_time = Instant::now();
        let uri_string = req.uri().to_string();
        let path = req.uri().path().to_string();
        let query_string = req.uri().query().map(|s| s.to_string());
        let method = req.method().clone();
        let method_str = method.as_str().to_string();

        // Generate or extract request ID (from incoming header or new UUID)
        let request_id = req
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        // Generate or extract trace ID (from traceparent header or new UUID)
        let trace_id = req
            .headers()
            .get("traceparent")
            .and_then(|v| v.to_str().ok())
            .and_then(|tp| {
                // traceparent format: 00-<trace-id>-<span-id>-<flags>
                let parts: Vec<&str> = tp.split('-').collect();
                if parts.len() >= 2 {
                    Some(parts[1].to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());

        // Check URI length limit early
        if let Err(e) = self.limits.validate_uri(&uri_string) {
            let response = self.validation_error_response(&[e]);
            self.record_request_metrics(
                &method_str,
                &path,
                response.status().as_u16(),
                0,
                0,
                start_time,
            );
            return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
        }

        // Reserved /__barbacane/* endpoints (skip other limits for internal endpoints)
        if path.starts_with("/__barbacane/") {
            let response = self.handle_barbacane_endpoint(&path, &method, query_string.as_deref());
            return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
        }

        // Extract headers for validation
        let headers: HashMap<String, String> = req
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();

        // Check header limits
        if let Err(e) = self.limits.validate_headers(&headers) {
            let response = self.validation_error_response(&[e]);
            self.record_request_metrics(
                &method_str,
                &path,
                response.status().as_u16(),
                0,
                0,
                start_time,
            );
            return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
        }

        // Check content-length before reading body (if present)
        if let Some(content_length) = headers.get("content-length") {
            if let Ok(len) = content_length.parse::<usize>() {
                if let Err(e) = self.limits.validate_body_size(len) {
                    let response = self.validation_error_response(&[e]);
                    self.record_request_metrics(
                        &method_str,
                        &path,
                        response.status().as_u16(),
                        0,
                        0,
                        start_time,
                    );
                    return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
                }
            }
        }

        // Route lookup
        match self.router.lookup(&path, &method_str) {
            RouteMatch::Found { entry, params } => {
                let operation = &self.operations[entry.operation_index];
                let validator = &self.validators[entry.operation_index];
                let route_path = operation.path.clone();

                let content_type = headers.get("content-type").map(|s| s.as_str());

                // Collect body bytes
                let body_bytes = match req.collect().await {
                    Ok(collected) => collected.to_bytes(),
                    Err(_) => {
                        let response = self.bad_request_response("failed to read request body");
                        self.record_request_metrics(
                            &method_str,
                            &route_path,
                            response.status().as_u16(),
                            0,
                            0,
                            start_time,
                        );
                        return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
                    }
                };

                let request_size = body_bytes.len() as u64;

                // Validate actual body size (in case content-length was missing or wrong)
                if let Err(e) = self.limits.validate_body_size(body_bytes.len()) {
                    self.metrics.record_validation_failure(
                        &method_str,
                        &route_path,
                        "body_too_large",
                    );
                    let response = self.validation_error_response(&[e]);
                    self.record_request_metrics(
                        &method_str,
                        &route_path,
                        response.status().as_u16(),
                        request_size,
                        0,
                        start_time,
                    );
                    return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
                }

                // Validate request against OpenAPI spec
                if let Err(errors) = validator.validate_request(
                    &params,
                    query_string.as_deref(),
                    &headers,
                    content_type,
                    &body_bytes,
                ) {
                    // Record validation failures - use error variant name as reason
                    for err in &errors {
                        let reason = validation_error_reason(err);
                        self.metrics
                            .record_validation_failure(&method_str, &route_path, &reason);
                    }
                    let response = self.validation_error_response(&errors);
                    self.record_request_metrics(
                        &method_str,
                        &route_path,
                        response.status().as_u16(),
                        request_size,
                        0,
                        start_time,
                    );
                    return Ok(Self::add_standard_headers(response, &request_id, &trace_id));
                }

                let response = self
                    .dispatch(
                        operation,
                        params,
                        query_string,
                        &body_bytes,
                        &headers,
                        client_addr,
                    )
                    .await?;

                // Add deprecation headers if the operation is deprecated
                let response = Self::add_deprecation_headers(response, operation);

                let response_size = response.body().size_hint().exact().unwrap_or(0);
                self.record_request_metrics(
                    &method_str,
                    &route_path,
                    response.status().as_u16(),
                    request_size,
                    response_size,
                    start_time,
                );
                Ok(Self::add_standard_headers(response, &request_id, &trace_id))
            }
            RouteMatch::MethodNotAllowed { allowed } => {
                // Check if this is a CORS preflight request
                // Preflight = OPTIONS + Origin + Access-Control-Request-Method headers
                if method == Method::OPTIONS
                    && headers.contains_key("origin")
                    && headers.contains_key("access-control-request-method")
                {
                    // Try to handle as CORS preflight by finding an operation with CORS middleware
                    if let Some(first_method) = allowed.first() {
                        if let RouteMatch::Found { entry, params: _ } =
                            self.router.lookup(&path, first_method)
                        {
                            let operation = &self.operations[entry.operation_index];

                            // Check if this operation has a CORS middleware
                            let cors_middleware =
                                operation.middlewares.iter().find(|mw| mw.name == "cors");

                            if let Some(cors_mw) = cors_middleware {
                                // Execute only the CORS middleware for preflight
                                let response = self
                                    .handle_cors_preflight(
                                        cors_mw,
                                        &headers,
                                        &request_id,
                                        &trace_id,
                                    )
                                    .await;
                                self.record_request_metrics(
                                    &method_str,
                                    &path,
                                    response.status().as_u16(),
                                    0,
                                    0,
                                    start_time,
                                );
                                return Ok(response);
                            }
                        }
                    }
                }

                // Not a CORS preflight or no CORS middleware found - return 405
                let response = self.method_not_allowed_response(allowed);
                self.record_request_metrics(
                    &method_str,
                    &path,
                    response.status().as_u16(),
                    0,
                    0,
                    start_time,
                );
                Ok(Self::add_standard_headers(response, &request_id, &trace_id))
            }
            RouteMatch::NotFound => {
                let response = self.not_found_response();
                self.record_request_metrics(
                    &method_str,
                    &path,
                    response.status().as_u16(),
                    0,
                    0,
                    start_time,
                );
                Ok(Self::add_standard_headers(response, &request_id, &trace_id))
            }
        }
    }

    /// Record request metrics.
    fn record_request_metrics(
        &self,
        method: &str,
        path: &str,
        status: u16,
        request_size: u64,
        response_size: u64,
        start_time: Instant,
    ) {
        let duration = start_time.elapsed().as_secs_f64();
        self.metrics.record_request(
            method,
            path,
            status,
            &self.api_name,
            duration,
            request_size,
            response_size,
        );
    }

    /// Dispatch a request to the appropriate handler.
    async fn dispatch(
        &self,
        operation: &CompiledOperation,
        params: Vec<(String, String)>,
        query_string: Option<String>,
        request_body: &[u8],
        headers: &HashMap<String, String>,
        client_addr: Option<SocketAddr>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let dispatch = &operation.dispatch;

        // Build the Request object for plugins (using BTreeMap for WASM compatibility)
        let path_params: std::collections::BTreeMap<String, String> = params.into_iter().collect();
        let headers_btree: std::collections::BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let plugin_request = barbacane_wasm::Request {
            method: operation.method.clone(),
            path: operation.path.clone(),
            query: query_string,
            headers: headers_btree,
            body: if request_body.is_empty() {
                None
            } else {
                String::from_utf8(request_body.to_vec()).ok()
            },
            client_ip: client_addr
                .map(|addr| addr.ip().to_string())
                .unwrap_or_else(|| "0.0.0.0".to_string()),
            path_params,
        };

        let request_json = match serde_json::to_vec(&plugin_request) {
            Ok(j) => j,
            Err(e) => {
                return Ok(
                    self.dev_error_response(format_args!("failed to serialize request: {}", e))
                );
            }
        };

        // Execute middleware on_request chain
        let (final_request_json, middleware_instances, middleware_context) =
            if !operation.middlewares.is_empty() {
                match self.execute_middleware_on_request(&operation.middlewares, &request_json) {
                    Ok((req, instances, ctx)) => (req, instances, ctx),
                    Err(resp) => return Ok(resp), // Short-circuit response
                }
            } else {
                (
                    request_json,
                    Vec::new(),
                    barbacane_wasm::RequestContext::default(),
                )
            };

        // All dispatchers must be WASM plugins loaded from the artifact
        if !self.plugin_pool.has_plugin(&dispatch.name) {
            return Ok(self.dev_error_response(format_args!(
                "unknown dispatcher '{}' - not found in artifact plugins",
                dispatch.name
            )));
        }

        // Dispatch to the plugin (returns raw plugin response for middleware chain)
        let plugin_response = match self
            .dispatch_wasm_plugin_inner(&dispatch.name, &dispatch.config, &final_request_json)
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(e),
        };

        // Execute middleware on_response chain (reverse order)
        let final_response = if !middleware_instances.is_empty() {
            self.execute_middleware_on_response(
                middleware_instances,
                plugin_response,
                middleware_context,
            )
        } else {
            plugin_response
        };

        Ok(self.build_response_from_plugin(&final_response))
    }

    /// Execute middleware on_request chain.
    /// Returns the final request JSON, middleware instances, and context (for on_response),
    /// or a short-circuit response.
    #[allow(clippy::result_large_err, clippy::type_complexity)]
    fn execute_middleware_on_request(
        &self,
        middlewares: &[barbacane_compiler::MiddlewareConfig],
        request_json: &[u8],
    ) -> Result<
        (
            Vec<u8>,
            Vec<barbacane_wasm::PluginInstance>,
            barbacane_wasm::RequestContext,
        ),
        Response<Full<Bytes>>,
    > {
        use barbacane_wasm::{execute_on_request_with_metrics, ChainResult, RequestContext};

        let mut instances = Vec::new();

        // Create instances for each middleware
        for mw in middlewares {
            if !self.plugin_pool.has_plugin(&mw.name) {
                tracing::error!(middleware = %mw.name, "middleware plugin not found in artifact");
                return Err(self.dev_error_response(format_args!(
                    "middleware '{}' not found - ensure it's declared in barbacane.yaml",
                    mw.name
                )));
            }

            let instance_key = barbacane_wasm::InstanceKey::new(&mw.name, &mw.config);
            let config_json = serde_json::to_vec(&mw.config).unwrap_or_default();
            self.plugin_pool
                .register_config(instance_key.clone(), config_json);

            match self.plugin_pool.get_instance(&instance_key) {
                Ok(instance) => instances.push(instance),
                Err(e) => {
                    tracing::error!(middleware = %mw.name, error = %e, "failed to get middleware instance");
                    return Err(self.dev_error_response(format_args!(
                        "failed to get middleware '{}': {}",
                        mw.name, e
                    )));
                }
            }
        }

        if instances.is_empty() {
            return Ok((request_json.to_vec(), instances, RequestContext::default()));
        }

        // Execute the on_request chain with metrics recording
        let context = RequestContext::default();
        let metrics = &self.metrics;
        let metrics_callback = |name: &str, phase: &str, duration: f64, short_circuit: bool| {
            metrics.record_middleware(name, phase, duration, short_circuit);
        };
        match execute_on_request_with_metrics(
            &mut instances,
            request_json,
            context,
            Some(&metrics_callback),
        ) {
            ChainResult::Continue { request, context } => Ok((request, instances, context)),
            ChainResult::ShortCircuit {
                response,
                middleware_index: _,
                context: _,
            } => {
                // Parse and return the short-circuit response
                match serde_json::from_slice::<barbacane_wasm::Response>(&response) {
                    Ok(plugin_response) => Err(self.build_response_from_plugin(&plugin_response)),
                    Err(e) => {
                        tracing::error!(error = %e, "failed to parse middleware response");
                        Err(self.dev_error_response(format_args!(
                            "failed to parse middleware response: {}",
                            e
                        )))
                    }
                }
            }
            ChainResult::Error {
                error,
                trap_result: _,
            } => {
                tracing::error!(error = %error, "middleware chain execution failed");
                Err(self.dev_error_response(format_args!("middleware chain error: {}", error)))
            }
        }
    }

    /// Execute middleware on_response chain.
    fn execute_middleware_on_response(
        &self,
        mut instances: Vec<barbacane_wasm::PluginInstance>,
        response: barbacane_wasm::Response,
        context: barbacane_wasm::RequestContext,
    ) -> barbacane_wasm::Response {
        use barbacane_wasm::execute_on_response_with_metrics;

        let response_json = match serde_json::to_vec(&response) {
            Ok(j) => j,
            Err(_) => return response,
        };

        let metrics = &self.metrics;
        let metrics_callback = |name: &str, phase: &str, duration: f64, short_circuit: bool| {
            metrics.record_middleware(name, phase, duration, short_circuit);
        };
        let final_response_json = execute_on_response_with_metrics(
            &mut instances,
            &response_json,
            context,
            Some(&metrics_callback),
        );

        // Parse the final response - middlewares can modify status/headers/body
        serde_json::from_slice::<barbacane_wasm::Response>(&final_response_json).unwrap_or(response)
    }

    /// Build an HTTP response from a plugin Response.
    fn build_response_from_plugin(
        &self,
        plugin_response: &barbacane_wasm::Response,
    ) -> Response<Full<Bytes>> {
        let status = StatusCode::from_u16(plugin_response.status).unwrap_or(StatusCode::OK);
        let mut builder = Response::builder().status(status);

        for (key, value) in &plugin_response.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        let body = plugin_response.body.clone().unwrap_or_default();
        builder
            .body(Full::new(Bytes::from(body)))
            .expect("valid response")
    }

    /// Dispatch via a WASM plugin (inner function taking pre-serialized request).
    /// Returns the raw plugin response for middleware chain processing.
    async fn dispatch_wasm_plugin_inner(
        &self,
        plugin_name: &str,
        config: &serde_json::Value,
        request_json: &[u8],
    ) -> Result<barbacane_wasm::Response, Response<Full<Bytes>>> {
        // Create instance key for this (plugin, config) pair
        let instance_key = barbacane_wasm::InstanceKey::new(plugin_name, config);

        // Register config if not already registered
        let config_json = serde_json::to_vec(config).unwrap_or_default();
        self.plugin_pool
            .register_config(instance_key.clone(), config_json);

        // Get a plugin instance
        let mut instance = match self.plugin_pool.get_instance(&instance_key) {
            Ok(i) => i,
            Err(e) => {
                return Err(
                    self.dev_error_response(format_args!("failed to get plugin instance: {}", e))
                );
            }
        };

        // Call the dispatch function
        if let Err(e) = instance.dispatch(request_json) {
            return Err(self.dev_error_response(format_args!("plugin dispatch failed: {}", e)));
        }

        // Get the output
        let output = instance.take_output();
        if output.is_empty() {
            return Err(self.dev_error_response("plugin returned empty output"));
        }

        // Parse the response
        match serde_json::from_slice(&output) {
            Ok(r) => Ok(r),
            Err(e) => {
                Err(self.dev_error_response(format_args!("failed to parse plugin response: {}", e)))
            }
        }
    }

    /// Handle reserved /__barbacane/* endpoints.
    fn handle_barbacane_endpoint(
        &self,
        path: &str,
        method: &Method,
        query: Option<&str>,
    ) -> Response<Full<Bytes>> {
        if method != Method::GET {
            return self.method_not_allowed_response(vec!["GET".to_string()]);
        }

        // Parse format from query string (default: yaml for specs, json for index)
        let format = query
            .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("format=")))
            .unwrap_or("yaml");

        match path {
            "/__barbacane/health" => self.health_response(),
            "/__barbacane/metrics" => self.metrics_response(),
            "/__barbacane/specs" => self.specs_index_response(),
            "/__barbacane/specs/openapi" => self.merged_openapi_response(format),
            "/__barbacane/specs/asyncapi" => self.merged_asyncapi_response(format),
            _ => {
                // Check for specific spec file: /__barbacane/specs/{filename}
                if let Some(filename) = path.strip_prefix("/__barbacane/specs/") {
                    self.spec_file_response(filename, format)
                } else {
                    self.not_found_response()
                }
            }
        }
    }

    /// Build the specs index response (always JSON).
    fn specs_index_response(&self) -> Response<Full<Bytes>> {
        let mut openapi_specs = Vec::new();
        let mut asyncapi_specs = Vec::new();

        for (name, content) in &self.specs {
            let spec_type = detect_spec_type(content);
            let entry = serde_json::json!({
                "name": name,
                "url": format!("/__barbacane/specs/{}", name),
            });
            match spec_type {
                SpecType::OpenApi => openapi_specs.push(entry),
                SpecType::AsyncApi => asyncapi_specs.push(entry),
                SpecType::Unknown => {} // Skip unknown specs
            }
        }

        let body = serde_json::json!({
            "openapi": {
                "specs": openapi_specs,
                "count": openapi_specs.len(),
                "merged_url": "/__barbacane/specs/openapi"
            },
            "asyncapi": {
                "specs": asyncapi_specs,
                "count": asyncapi_specs.len(),
                "merged_url": "/__barbacane/specs/asyncapi"
            }
        });

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Serve merged OpenAPI spec (all OpenAPI specs combined).
    fn merged_openapi_response(&self, format: &str) -> Response<Full<Bytes>> {
        // Collect all OpenAPI specs
        let openapi_specs: Vec<_> = self
            .specs
            .iter()
            .filter(|(_, content)| matches!(detect_spec_type(content), SpecType::OpenApi))
            .collect();

        if openapi_specs.is_empty() {
            return self.not_found_response();
        }

        // Merge specs
        let merged = merge_openapi_specs(&openapi_specs);
        self.serve_spec_content(&merged, format)
    }

    /// Serve merged AsyncAPI spec (all AsyncAPI specs combined).
    fn merged_asyncapi_response(&self, format: &str) -> Response<Full<Bytes>> {
        // Collect all AsyncAPI specs
        let asyncapi_specs: Vec<_> = self
            .specs
            .iter()
            .filter(|(_, content)| matches!(detect_spec_type(content), SpecType::AsyncApi))
            .collect();

        if asyncapi_specs.is_empty() {
            return self.not_found_response();
        }

        // Merge specs
        let merged = merge_asyncapi_specs(&asyncapi_specs);
        self.serve_spec_content(&merged, format)
    }

    /// Serve spec content in requested format.
    fn serve_spec_content(&self, value: &serde_json::Value, format: &str) -> Response<Full<Bytes>> {
        let (content, content_type) = if format == "json" {
            (
                serde_json::to_string_pretty(value).unwrap_or_default(),
                "application/json",
            )
        } else {
            (
                serde_yaml::to_string(value).unwrap_or_default(),
                "text/yaml",
            )
        };

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", content_type)
            .body(Full::new(Bytes::from(content)))
            .expect("valid response")
    }

    /// Serve a specific spec file.
    fn spec_file_response(&self, filename: &str, format: &str) -> Response<Full<Bytes>> {
        if let Some(content) = self.specs.get(filename) {
            let is_source_json = filename.ends_with(".json");

            // Parse the spec
            let parsed: Option<serde_json::Value> = if is_source_json {
                serde_json::from_str(content).ok()
            } else {
                serde_yaml::from_str(content).ok()
            };

            match parsed {
                Some(mut value) => {
                    // Strip x-barbacane-* extensions
                    strip_barbacane_keys_recursive(&mut value);
                    self.serve_spec_content(&value, format)
                }
                None => {
                    // If parsing fails, return original content
                    let content_type = if is_source_json {
                        "application/json"
                    } else {
                        "text/yaml"
                    };
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", content_type)
                        .body(Full::new(Bytes::from(content.clone())))
                        .expect("valid response")
                }
            }
        } else {
            self.not_found_response()
        }
    }

    /// Build the health response.
    fn health_response(&self) -> Response<Full<Bytes>> {
        let body = serde_json::json!({
            "status": "healthy",
            "artifact_version": self.manifest.barbacane_artifact_version,
            "compiler_version": self.manifest.compiler_version,
            "routes_count": self.manifest.routes_count,
        });

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Build the Prometheus metrics response.
    fn metrics_response(&self) -> Response<Full<Bytes>> {
        let body = barbacane_telemetry::prometheus::render_metrics(&self.metrics);

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", barbacane_telemetry::PROMETHEUS_CONTENT_TYPE)
            .body(Full::new(Bytes::from(body)))
            .expect("valid response")
    }

    /// Build a 404 Not Found response.
    fn not_found_response(&self) -> Response<Full<Bytes>> {
        let body = r#"{"error":"not found"}"#;

        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .expect("valid response")
    }

    /// Build a 405 Method Not Allowed response.
    fn method_not_allowed_response(&self, allowed: Vec<String>) -> Response<Full<Bytes>> {
        let body = r#"{"error":"method not allowed"}"#;
        let allow_header = allowed.join(", ");

        Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header("content-type", "application/json")
            .header("allow", allow_header)
            .body(Full::new(Bytes::from(body)))
            .expect("valid response")
    }

    /// Handle CORS preflight request by executing only the CORS middleware.
    ///
    /// This is called when an OPTIONS request with CORS headers is received
    /// for a path that has a CORS middleware configured on one of its operations.
    async fn handle_cors_preflight(
        &self,
        cors_middleware: &barbacane_compiler::MiddlewareConfig,
        headers: &HashMap<String, String>,
        request_id: &str,
        trace_id: &str,
    ) -> Response<Full<Bytes>> {
        // Build a minimal request for the CORS middleware
        let headers_btree: std::collections::BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let plugin_request = barbacane_wasm::Request {
            method: "OPTIONS".to_string(),
            path: String::new(),
            query: None,
            headers: headers_btree,
            body: None,
            client_ip: "0.0.0.0".to_string(),
            path_params: std::collections::BTreeMap::new(),
        };

        let request_json = match serde_json::to_vec(&plugin_request) {
            Ok(j) => j,
            Err(_) => {
                return Self::add_standard_headers(
                    self.internal_error_response(None),
                    request_id,
                    trace_id,
                );
            }
        };

        // Execute only the CORS middleware
        let middlewares = vec![cors_middleware.clone()];
        match self.execute_middleware_on_request(&middlewares, &request_json) {
            Ok((_, _, _)) => {
                // CORS middleware didn't short-circuit, return empty 204
                // (This shouldn't happen for valid preflights, but handle it gracefully)
                Self::add_standard_headers(
                    Response::builder()
                        .status(StatusCode::NO_CONTENT)
                        .body(Full::new(Bytes::new()))
                        .expect("valid response"),
                    request_id,
                    trace_id,
                )
            }
            Err(response) => {
                // CORS middleware short-circuited with a response (expected for preflights)
                Self::add_standard_headers(response, request_id, trace_id)
            }
        }
    }

    /// Build a 400 Bad Request response for generic errors.
    fn bad_request_response(&self, message: &str) -> Response<Full<Bytes>> {
        let body = serde_json::json!({
            "type": "urn:barbacane:error:bad-request",
            "title": "Bad Request",
            "status": 400,
            "detail": message,
        });

        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Build a 400 validation error response (RFC 9457).
    fn validation_error_response(&self, errors: &[ValidationError2]) -> Response<Full<Bytes>> {
        let problem = ProblemDetails::validation_error(errors, self.dev_mode);

        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(problem.to_json())))
            .expect("valid response")
    }

    /// Build a 500 Internal Server Error response (RFC 9457).
    fn internal_error_response(&self, detail: Option<&str>) -> Response<Full<Bytes>> {
        let body = if self.dev_mode {
            serde_json::json!({
                "type": "urn:barbacane:error:internal-error",
                "title": "Internal Server Error",
                "status": 500,
                "detail": detail.unwrap_or("An internal error occurred"),
            })
        } else {
            serde_json::json!({
                "type": "urn:barbacane:error:internal-error",
                "title": "Internal Server Error",
                "status": 500,
            })
        };

        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Build a 500 response with detail visible only in dev mode.
    fn dev_error_response(&self, msg: impl std::fmt::Display) -> Response<Full<Bytes>> {
        let detail = if self.dev_mode {
            Some(msg.to_string())
        } else {
            None
        };
        self.internal_error_response(detail.as_deref())
    }
}

/// Validation result for a single spec file.
#[derive(serde::Serialize)]
struct ValidationResult {
    file: String,
    valid: bool,
    errors: Vec<ValidationIssue>,
    warnings: Vec<ValidationIssue>,
}

#[derive(serde::Serialize)]
struct ValidationIssue {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
}

/// Run the validate command.
fn run_validate(specs: &[String], output_format: &str) -> ExitCode {
    let mut results = Vec::new();
    let mut has_errors = false;

    // Track all routes across specs for conflict detection (E1010)
    // Key: (path, method), Value: spec file where first defined
    let mut seen_routes: HashMap<(String, String), String> = HashMap::new();

    // Collect parsed specs for cross-spec validation
    let mut parsed_specs: Vec<(String, barbacane_compiler::ApiSpec)> = Vec::new();

    // Phase 1: Parse and validate each spec individually
    for spec_path in specs {
        let path = Path::new(spec_path);
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // Check file exists
        if !path.exists() {
            errors.push(ValidationIssue {
                code: "E1000".to_string(),
                message: format!("file not found: {}", spec_path),
                location: None,
            });
            has_errors = true;
            results.push(ValidationResult {
                file: spec_path.clone(),
                valid: false,
                errors,
                warnings,
            });
            continue;
        }

        // Try to parse the spec
        match barbacane_compiler::parse_spec_file(path) {
            Ok(spec) => {
                // Check for missing x-barbacane-dispatch on operations
                for op in &spec.operations {
                    if op.dispatch.is_none() {
                        warnings.push(ValidationIssue {
                            code: "E1020".to_string(),
                            message: format!(
                                "operation {} {} is missing x-barbacane-dispatch",
                                op.method, op.path
                            ),
                            location: Some(format!("{}:{} {}", spec_path, op.path, op.method)),
                        });
                    }

                    // Check middlewares have required 'name' field (E1011)
                    if let Some(middlewares) = &op.middlewares {
                        for (idx, mw) in middlewares.iter().enumerate() {
                            if mw.name.is_empty() {
                                errors.push(ValidationIssue {
                                    code: "E1011".to_string(),
                                    message: format!(
                                        "middleware #{} in {} {} is missing 'name'",
                                        idx + 1,
                                        op.method,
                                        op.path
                                    ),
                                    location: Some(format!(
                                        "{}:{} {}",
                                        spec_path, op.path, op.method
                                    )),
                                });
                                has_errors = true;
                            }
                        }
                    }
                }

                // Check global middlewares have required 'name' field (E1011)
                for (idx, mw) in spec.global_middlewares.iter().enumerate() {
                    if mw.name.is_empty() {
                        errors.push(ValidationIssue {
                            code: "E1011".to_string(),
                            message: format!("global middleware #{} is missing 'name'", idx + 1),
                            location: Some(format!("{}:x-barbacane-middlewares", spec_path)),
                        });
                        has_errors = true;
                    }
                }

                // Note: E1015 (unknown x-barbacane-* extension) checking is done during compile,
                // not validate, to avoid false positives on non-barbacane extensions.

                // Store for cross-spec validation
                parsed_specs.push((spec_path.clone(), spec));
                results.push(ValidationResult {
                    file: spec_path.clone(),
                    valid: errors.is_empty(),
                    errors,
                    warnings,
                });
            }
            Err(e) => {
                let (code, message) = match &e {
                    barbacane_compiler::ParseError::UnknownFormat => {
                        ("E1001".to_string(), e.to_string())
                    }
                    barbacane_compiler::ParseError::ParseError(_) => {
                        ("E1002".to_string(), e.to_string())
                    }
                    barbacane_compiler::ParseError::UnresolvedRef(_) => {
                        ("E1003".to_string(), e.to_string())
                    }
                    barbacane_compiler::ParseError::SchemaError(_) => {
                        ("E1004".to_string(), e.to_string())
                    }
                    barbacane_compiler::ParseError::Io(io_err) => {
                        ("E1000".to_string(), format!("I/O error: {}", io_err))
                    }
                };
                errors.push(ValidationIssue {
                    code,
                    message,
                    location: Some(spec_path.clone()),
                });
                has_errors = true;
                results.push(ValidationResult {
                    file: spec_path.clone(),
                    valid: false,
                    errors,
                    warnings,
                });
            }
        }
    }

    // Phase 2: Check for routing conflicts across specs (E1010)
    if parsed_specs.len() > 1 {
        for (spec_path, spec) in &parsed_specs {
            for op in &spec.operations {
                let key = (op.path.clone(), op.method.clone());
                if let Some(other_spec) = seen_routes.get(&key) {
                    // Find the result for this spec and add the conflict error
                    if let Some(result) = results.iter_mut().find(|r| &r.file == spec_path) {
                        result.errors.push(ValidationIssue {
                            code: "E1010".to_string(),
                            message: format!(
                                "routing conflict: {} {} is also declared in '{}'",
                                op.method, op.path, other_spec
                            ),
                            location: Some(format!("{}:{} {}", spec_path, op.path, op.method)),
                        });
                        result.valid = false;
                        has_errors = true;
                    }
                } else {
                    seen_routes.insert(key, spec_path.clone());
                }
            }
        }
    }

    // Output results
    if output_format == "json" {
        let output = serde_json::json!({
            "results": results,
            "summary": {
                "total": results.len(),
                "valid": results.iter().filter(|r| r.valid).count(),
                "invalid": results.iter().filter(|r| !r.valid).count(),
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("serializable json")
        );
    } else {
        // Text format
        for result in &results {
            if result.valid && result.warnings.is_empty() {
                eprintln!("✓ {} is valid", result.file);
            } else if result.valid {
                eprintln!(
                    "✓ {} is valid (with {} warning(s))",
                    result.file,
                    result.warnings.len()
                );
            } else {
                eprintln!("✗ {} has {} error(s)", result.file, result.errors.len());
            }

            for err in &result.errors {
                if let Some(loc) = &err.location {
                    eprintln!("  {} [{}]: {}", err.code, loc, err.message);
                } else {
                    eprintln!("  {}: {}", err.code, err.message);
                }
            }

            for warn in &result.warnings {
                if let Some(loc) = &warn.location {
                    eprintln!("  {} [{}]: {} (warning)", warn.code, loc, warn.message);
                } else {
                    eprintln!("  {}: {} (warning)", warn.code, warn.message);
                }
            }
        }

        let valid_count = results.iter().filter(|r| r.valid).count();
        let total = results.len();
        eprintln!();
        eprintln!(
            "validated {} spec(s): {} valid, {} invalid",
            total,
            valid_count,
            total - valid_count
        );
    }

    if has_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Official plugins available for download.
const OFFICIAL_PLUGINS: &[(&str, &str)] = &[
    ("mock", "mock.wasm"),
    ("http-upstream", "http-upstream.wasm"),
];

/// GitHub release URL base for official plugins.
const PLUGIN_RELEASE_BASE: &str =
    "https://github.com/barbacane-dev/barbacane/releases/latest/download";

/// Download a plugin from GitHub releases.
async fn download_plugin(name: &str, filename: &str, dest_dir: &Path) -> Result<(), String> {
    let url = format!("{}/{}", PLUGIN_RELEASE_BASE, filename);
    let dest_path = dest_dir.join(filename);

    eprint!("  Downloading {}...", name);

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch {}: {}", url, e))?;

    if !response.status().is_success() {
        return Err(format!(
            "failed to download {}: HTTP {}",
            filename,
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("failed to read response: {}", e))?;

    std::fs::write(&dest_path, &bytes)
        .map_err(|e| format!("failed to write {}: {}", dest_path.display(), e))?;

    eprintln!(" done ({} bytes)", bytes.len());
    Ok(())
}

/// Run the init command.
async fn run_init(name: &str, template: &str, fetch_plugins: bool) -> ExitCode {
    use std::fs;

    // Validate template
    if template != "basic" && template != "minimal" {
        eprintln!(
            "error: unknown template '{}'. Use 'basic' or 'minimal'.",
            template
        );
        return ExitCode::from(1);
    }

    // Determine project directory
    let project_dir = if name == "." {
        std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
    } else {
        Path::new(name).to_path_buf()
    };

    // Check if directory is empty (if not ".")
    if name != "." {
        if project_dir.exists() {
            if fs::read_dir(&project_dir)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
            {
                eprintln!("error: directory '{}' is not empty", name);
                return ExitCode::from(1);
            }
        } else if let Err(e) = fs::create_dir_all(&project_dir) {
            eprintln!("error: failed to create directory '{}': {}", name, e);
            return ExitCode::from(1);
        }
    }

    // Create plugins directory
    let plugins_dir = project_dir.join("plugins");
    if let Err(e) = fs::create_dir_all(&plugins_dir) {
        eprintln!("error: failed to create plugins directory: {}", e);
        return ExitCode::from(1);
    }

    // Download plugins if requested
    let mut downloaded_plugins = Vec::new();
    if fetch_plugins {
        eprintln!("Fetching official plugins...");
        for (plugin_name, filename) in OFFICIAL_PLUGINS {
            match download_plugin(plugin_name, filename, &plugins_dir).await {
                Ok(()) => downloaded_plugins.push((*plugin_name, *filename)),
                Err(e) => {
                    eprintln!(" failed");
                    eprintln!("warning: {}", e);
                }
            }
        }
        if downloaded_plugins.is_empty() {
            eprintln!("warning: no plugins were downloaded");
        }
        eprintln!();
    }

    // Create barbacane.yaml with downloaded plugins or empty template
    let manifest_content = if downloaded_plugins.is_empty() {
        r#"# Barbacane project manifest
# See https://barbacane.dev/docs/guide/spec-configuration for details

plugins: {}
  # Example plugin configuration:
  # my-plugin:
  #   path: ./plugins/my-plugin.wasm
"#
        .to_string()
    } else {
        let mut content = String::from(
            "# Barbacane project manifest\n\
             # See https://barbacane.dev/docs/guide/spec-configuration for details\n\n\
             plugins:\n",
        );
        for (plugin_name, filename) in &downloaded_plugins {
            content.push_str(&format!(
                "  {}:\n    path: ./plugins/{}\n",
                plugin_name, filename
            ));
        }
        content
    };

    if let Err(e) = fs::write(project_dir.join("barbacane.yaml"), &manifest_content) {
        eprintln!("error: failed to create barbacane.yaml: {}", e);
        return ExitCode::from(1);
    }

    // Create spec file based on template
    let spec_content = if template == "basic" {
        r#"openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"
  description: A Barbacane-powered API

servers:
  - url: http://localhost:8080
    description: Local development

paths:
  /health:
    get:
      summary: Health check
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status": "ok"}'
          headers:
            Content-Type: application/json
      responses:
        "200":
          description: Service is healthy
          content:
            application/json:
              schema:
                type: object
                properties:
                  status:
                    type: string
                    example: ok

  /users:
    get:
      summary: List users
      operationId: listUsers
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"users": []}'
          headers:
            Content-Type: application/json
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
            minimum: 1
            maximum: 100
            default: 10
      responses:
        "200":
          description: List of users
          content:
            application/json:
              schema:
                type: object
                properties:
                  users:
                    type: array
                    items:
                      type: object

    post:
      summary: Create user
      operationId: createUser
      x-barbacane-dispatch:
        name: mock
        config:
          status: 201
          body: '{"id": "user-123", "message": "Created"}'
          headers:
            Content-Type: application/json
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required:
                - name
                - email
              properties:
                name:
                  type: string
                  minLength: 1
                email:
                  type: string
                  format: email
      responses:
        "201":
          description: User created
        "400":
          description: Invalid request

  /users/{id}:
    get:
      summary: Get user by ID
      operationId: getUser
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"id": "{id}", "name": "John Doe"}'
          headers:
            Content-Type: application/json
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
            format: uuid
      responses:
        "200":
          description: User details
        "404":
          description: User not found
"#
    } else {
        // minimal template
        r#"openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

paths:
  /health:
    get:
      summary: Health check
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status": "ok"}'
      responses:
        "200":
          description: OK
"#
    };

    if let Err(e) = fs::write(project_dir.join("api.yaml"), spec_content) {
        eprintln!("error: failed to create api.yaml: {}", e);
        return ExitCode::from(1);
    }

    // Create .gitignore
    let gitignore_content = r#"# Build artifacts
*.bca
target/

# IDE
.idea/
.vscode/
*.swp
*.swo

# OS
.DS_Store
Thumbs.db
"#;

    if let Err(e) = fs::write(project_dir.join(".gitignore"), gitignore_content) {
        eprintln!("error: failed to create .gitignore: {}", e);
        return ExitCode::from(1);
    }

    // Success message
    let dir_name = if name == "." {
        "current directory"
    } else {
        name
    };
    eprintln!("✓ Initialized Barbacane project in {}", dir_name);
    eprintln!();
    eprintln!("Created:");
    eprintln!("  barbacane.yaml  - project manifest");
    eprintln!(
        "  api.yaml        - OpenAPI specification ({} template)",
        template
    );
    if !downloaded_plugins.is_empty() {
        for (plugin_name, filename) in &downloaded_plugins {
            eprintln!("  plugins/{}  - {} plugin", filename, plugin_name);
        }
    } else {
        eprintln!("  plugins/        - directory for WASM plugins");
    }
    eprintln!("  .gitignore      - Git ignore file");
    eprintln!();
    eprintln!("Next steps:");
    if downloaded_plugins.is_empty() && !fetch_plugins {
        eprintln!("  1. Download plugins: barbacane init . --fetch-plugins");
        eprintln!("     Or add them manually to plugins/");
        eprintln!("  2. Edit api.yaml to define your API");
        eprintln!(
            "  3. Run: barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca"
        );
        eprintln!("  4. Run: barbacane serve --artifact api.bca --dev");
    } else {
        eprintln!("  1. Edit api.yaml to define your API");
        eprintln!(
            "  2. Run: barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca"
        );
        eprintln!("  3. Run: barbacane serve --artifact api.bca --dev");
    }

    ExitCode::SUCCESS
}

// =============================================================================
// Hot-Reload Functions
// =============================================================================

/// Perform hot-reload: download, verify, load, and swap the gateway state.
async fn perform_hot_reload(
    notification: control_plane::ArtifactNotification,
    shared_gateway: &SharedGateway,
    artifact_dir: &Path,
    dev_mode: bool,
    limits: RequestLimits,
    allow_plaintext_upstream: bool,
    metrics: Arc<MetricsRegistry>,
) -> HotReloadResult {
    let artifact_id = notification.artifact_id;

    // Acquire lock to prevent concurrent hot-reloads
    let _guard = match hot_reload::HOT_RELOAD_LOCK.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            tracing::warn!(
                artifact_id = %artifact_id,
                "Hot-reload already in progress, skipping"
            );
            return HotReloadResult::Failed {
                artifact_id,
                error: "hot-reload already in progress".to_string(),
            };
        }
    };

    tracing::info!(
        artifact_id = %artifact_id,
        download_url = %notification.download_url,
        "Starting hot-reload"
    );

    // Create HTTP client for download
    let http_client = reqwest::Client::new();

    // Step 1: Download and verify artifact
    let artifact_path = match hot_reload::download_artifact(
        &http_client,
        &notification.download_url,
        &notification.sha256,
        artifact_dir,
    )
    .await
    {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(
                artifact_id = %artifact_id,
                error = %e,
                "Hot-reload download failed"
            );
            return HotReloadResult::Failed {
                artifact_id,
                error: format!("download failed: {}", e),
            };
        }
    };

    // Step 2: Load and compile new Gateway
    let new_gateway = match Gateway::load(
        &artifact_path,
        dev_mode,
        limits,
        allow_plaintext_upstream,
        metrics,
    ) {
        Ok(g) => g,
        Err(e) => {
            // Clean up downloaded artifact on failure
            let _ = tokio::fs::remove_file(&artifact_path).await;
            tracing::error!(
                artifact_id = %artifact_id,
                error = %e,
                "Hot-reload load failed"
            );
            return HotReloadResult::Failed {
                artifact_id,
                error: format!("load failed: {}", e),
            };
        }
    };

    // Step 3: Atomic swap
    let old_gateway = shared_gateway.swap(Arc::new(new_gateway));

    tracing::info!(
        artifact_id = %artifact_id,
        old_routes = old_gateway.manifest.routes_count,
        new_routes = shared_gateway.load().manifest.routes_count,
        "Hot-reload completed successfully"
    );

    // Drop old gateway on a blocking thread to avoid panic when wasmtime's
    // runtime is dropped inside an async context.
    tokio::task::spawn_blocking(move || drop(old_gateway));

    HotReloadResult::Success { artifact_id }
}

/// Run the compile command.
fn run_compile(
    specs: &[String],
    output: &str,
    manifest_file: &str,
    allow_plaintext: bool,
) -> ExitCode {
    let spec_paths: Vec<&Path> = specs.iter().map(Path::new).collect();
    let output_path = Path::new(output);

    // Check that all spec files exist
    for path in &spec_paths {
        if !path.exists() {
            eprintln!("error: spec file not found: {}", path.display());
            return ExitCode::from(1);
        }
    }

    let options = CompileOptions {
        allow_plaintext,
        ..Default::default()
    };

    let manifest_path = Path::new(manifest_file);
    if !manifest_path.exists() {
        eprintln!("error: manifest file not found: {}", manifest_file);
        return ExitCode::from(1);
    }

    // Load the project manifest
    let project_manifest = match ProjectManifest::load(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: failed to load manifest: {}", e);
            return ExitCode::from(1);
        }
    };

    // Get the base path for resolving plugin paths (directory containing the manifest)
    let base_path = manifest_path.parent().unwrap_or(Path::new("."));

    let result = compile_with_manifest(
        &spec_paths,
        &project_manifest,
        base_path,
        output_path,
        &options,
    );

    match result {
        Ok(compile_result) => {
            // Print warnings if any
            for warning in &compile_result.warnings {
                eprintln!(
                    "warning[{}]: {}{}",
                    warning.code,
                    warning.message,
                    warning
                        .location
                        .as_ref()
                        .map(|l| format!(" ({})", l))
                        .unwrap_or_default()
                );
            }

            let manifest = &compile_result.manifest;
            let plugin_info = if manifest.plugins.is_empty() {
                String::new()
            } else {
                format!(", {} plugin(s) bundled", manifest.plugins.len())
            };
            eprintln!(
                "compiled {} spec(s) to {} ({} routes{})",
                specs.len(),
                output,
                manifest.routes_count,
                plugin_info
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: compilation failed: {}", e);
            ExitCode::from(1)
        }
    }
}

/// TLS configuration for the server.
struct TlsConfig {
    cert_path: String,
    key_path: String,
    /// Minimum TLS version: "1.2" or "1.3"
    min_version: String,
}

/// Configuration for connected mode (optional control plane connection).
#[derive(Clone)]
struct ConnectedModeConfig {
    /// WebSocket URL for the control plane (e.g., ws://control:8080/ws/data-plane).
    control_plane_url: String,
    /// Project ID to register with.
    project_id: uuid::Uuid,
    /// API key for authentication.
    api_key: String,
    /// Optional name for this data plane.
    data_plane_name: Option<String>,
}

/// Convert a WebSocket URL to an HTTP base URL.
///
/// E.g., `ws://host:9090/ws/data-plane` → `http://host:9090`
///       `wss://host:9090/ws/data-plane` → `https://host:9090`
fn ws_url_to_http_base(ws_url: &str) -> String {
    let http_url = ws_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    // Strip the path portion, keeping only scheme + authority
    // Find the third '/' (after "http://") to locate where the path starts
    if let Some(authority_end) = http_url
        .find("://")
        .and_then(|i| http_url[i + 3..].find('/').map(|j| i + 3 + j))
    {
        http_url[..authority_end].to_string()
    } else {
        http_url
    }
}

/// Load TLS certificates and create a rustls ServerConfig.
///
/// Configuration:
/// - Minimum TLS version configurable (1.2 or 1.3)
/// - ALPN: h2, http/1.1
fn load_tls_config(config: &TlsConfig) -> Result<Arc<ServerConfig>, String> {
    // Load certificate chain
    let cert_file = File::open(&config.cert_path).map_err(|e| {
        format!(
            "failed to open certificate file '{}': {}",
            config.cert_path, e
        )
    })?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            format!(
                "failed to parse certificate file '{}': {}",
                config.cert_path, e
            )
        })?;

    if certs.is_empty() {
        return Err(format!("no certificates found in '{}'", config.cert_path));
    }

    // Load private key
    let key_file = File::open(&config.key_path)
        .map_err(|e| format!("failed to open key file '{}': {}", config.key_path, e))?;
    let mut key_reader = BufReader::new(key_file);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| format!("failed to parse key file '{}': {}", config.key_path, e))?
        .ok_or_else(|| format!("no private key found in '{}'", config.key_path))?;

    // Select TLS versions based on min_version setting
    // Note: min_version is validated at startup, so only "1.2" or "1.3" are possible
    let versions: Vec<&'static rustls::SupportedProtocolVersion> = match config.min_version.as_str()
    {
        "1.3" => vec![&rustls::version::TLS13],
        _ => vec![&rustls::version::TLS13, &rustls::version::TLS12], // "1.2" (default)
    };

    // Build TLS config with configured version
    let mut server_config = ServerConfig::builder_with_protocol_versions(&versions)
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("failed to build TLS config: {}", e))?;

    // Set ALPN protocols: prefer HTTP/2, fallback to HTTP/1.1
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(Arc::new(server_config))
}

/// Run the serve command.
#[allow(clippy::too_many_arguments)]
async fn run_serve(
    artifact: &str,
    listen: &str,
    dev: bool,
    limits: RequestLimits,
    allow_plaintext_upstream: bool,
    tls_config: Option<TlsConfig>,
    metrics: Arc<MetricsRegistry>,
    keepalive_timeout: u64,
    shutdown_timeout: u64,
    connected_mode: Option<ConnectedModeConfig>,
) -> ExitCode {
    let artifact_path = Path::new(artifact);
    if !artifact_path.exists() {
        eprintln!("error: artifact not found: {}", artifact);
        return ExitCode::from(1);
    }

    // Determine artifact directory for hot-reload downloads
    let artifact_dir = artifact_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    let gateway: SharedGateway = match Gateway::load(
        artifact_path,
        dev,
        limits.clone(),
        allow_plaintext_upstream,
        metrics.clone(),
    ) {
        Ok(g) => Arc::new(ArcSwap::new(Arc::new(g))),
        Err(e) => {
            eprintln!("error: {}", e);
            // Exit code 13 for secret resolution failures
            if e.contains("failed to resolve secrets") {
                return ExitCode::from(13);
            }
            return ExitCode::from(1);
        }
    };

    eprintln!(
        "barbacane: loaded {} route(s) from artifact",
        gateway.load().manifest.routes_count
    );

    // Parse listen address
    let addr: SocketAddr = match listen.parse() {
        Ok(a) => a,
        Err(_) => {
            eprintln!("error: invalid listen address: {}", listen);
            return ExitCode::from(1);
        }
    };

    // Bind the listener
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to bind to {}: {}", addr, e);
            return ExitCode::from(1);
        }
    };

    // Load TLS config if provided
    let tls_acceptor = match &tls_config {
        Some(config) => {
            let server_config = match load_tls_config(config) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(1);
                }
            };
            Some(TlsAcceptor::from(server_config))
        }
        None => None,
    };

    let protocol = if tls_acceptor.is_some() {
        "https"
    } else {
        "http"
    };
    eprintln!("barbacane: listening on {}://{}", protocol, addr);

    // Create shutdown signal channel
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // Spawn signal handler task
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = wait_for_shutdown_signal().await;
        eprintln!("barbacane: received shutdown signal, draining connections...");
        let _ = shutdown_tx_clone.send(true);
    });

    // Start control plane client if in connected mode
    let (mut artifact_rx, response_tx, control_plane_http_base) = if let Some(config) =
        connected_mode
    {
        eprintln!(
            "barbacane: connecting to control plane at {}",
            config.control_plane_url
        );
        let http_base = ws_url_to_http_base(&config.control_plane_url);
        let client = control_plane::ControlPlaneClient::new(control_plane::ControlPlaneConfig {
            control_plane_url: config.control_plane_url,
            project_id: config.project_id,
            api_key: config.api_key,
            data_plane_name: config.data_plane_name,
        });
        let (rx, tx) = client.start(shutdown_rx.clone());
        (Some(rx), Some(tx), Some(http_base))
    } else {
        (None, None, None)
    };

    // Keep-alive timeout (currently used for documentation; HTTP/1.1 uses internal defaults)
    let _keepalive_duration = Duration::from_secs(keepalive_timeout);
    let shutdown_duration = Duration::from_secs(shutdown_timeout);

    // Track active connections for graceful shutdown
    let active_connections = Arc::new(AtomicU64::new(0));

    // Accept connections
    loop {
        tokio::select! {
            // Check for shutdown signal
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            // Handle artifact notifications from control plane
            Some(mut notification) = async {
                match artifact_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Resolve relative download URLs against control plane base
                if notification.download_url.starts_with('/') {
                    if let Some(base) = &control_plane_http_base {
                        notification.download_url = format!("{}{}", base, notification.download_url);
                    }
                }

                eprintln!(
                    "barbacane: new artifact available: {}, initiating hot-reload",
                    notification.artifact_id
                );

                // Clone values for the spawned task
                let gateway_clone = gateway.clone();
                let artifact_dir_clone = artifact_dir.clone();
                let limits_clone = limits.clone();
                let metrics_clone = metrics.clone();
                let response_tx_clone = response_tx.clone();

                tokio::spawn(async move {
                    let result = perform_hot_reload(
                        notification,
                        &gateway_clone,
                        &artifact_dir_clone,
                        dev,
                        limits_clone,
                        allow_plaintext_upstream,
                        metrics_clone,
                    )
                    .await;

                    // Send response to control plane
                    let response = match &result {
                        HotReloadResult::Success { artifact_id } => {
                            eprintln!(
                                "barbacane: hot-reload successful for artifact {}",
                                artifact_id
                            );
                            control_plane::ArtifactDownloadedResponse {
                                artifact_id: *artifact_id,
                                success: true,
                                error: None,
                            }
                        }
                        HotReloadResult::Failed { artifact_id, error } => {
                            eprintln!(
                                "barbacane: hot-reload failed for artifact {}: {}",
                                artifact_id, error
                            );
                            control_plane::ArtifactDownloadedResponse {
                                artifact_id: *artifact_id,
                                success: false,
                                error: Some(error.clone()),
                            }
                        }
                    };

                    // Send response if we have a channel
                    if let Some(tx) = response_tx_clone {
                        if let Err(e) = tx.send(response).await {
                            tracing::warn!(error = %e, "Failed to send hot-reload response");
                        }
                    }
                });
            }
            // Accept new connections
            accept_result = listener.accept() => {
                let (stream, peer_addr) = match accept_result {
                    Ok(conn) => conn,
                    Err(e) => {
                        eprintln!("error: accept failed: {}", e);
                        continue;
                    }
                };

                // Track connection
                metrics.connection_opened();
                active_connections.fetch_add(1, Ordering::SeqCst);

                // Get a snapshot of the gateway for this connection.
                // All requests on this connection will use this version,
                // allowing in-flight requests to complete during hot-reload.
                let gateway_snapshot = gateway.load_full();
                let tls_acceptor = tls_acceptor.clone();
                let conn_metrics = metrics.clone();
                let conn_counter = active_connections.clone();
                let mut conn_shutdown_rx = shutdown_rx.clone();
                let client_addr = Some(peer_addr);

                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let gateway = Arc::clone(&gateway_snapshot);
                        let client_addr = client_addr;
                        async move { gateway.handle_request(req, client_addr).await }
                    });

                    if let Some(acceptor) = tls_acceptor {
                        // TLS connection - uses auto protocol detection (HTTP/1.1 or HTTP/2 via ALPN)
                        match acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                let io = TokioIo::new(tls_stream);
                                let mut builder = auto::Builder::new(TokioExecutor::new());
                                builder.http1().keep_alive(true);
                                builder
                                    .http2()
                                    .keep_alive_interval(Some(std::time::Duration::from_secs(20)));
                                let conn = builder.serve_connection_with_upgrades(io, service);

                                // Pin the connection for graceful shutdown
                                tokio::pin!(conn);

                                loop {
                                    tokio::select! {
                                        result = conn.as_mut() => {
                                            if let Err(e) = result {
                                                if !e.to_string().contains("connection closed") {
                                                    tracing::debug!(error = %e, "connection error");
                                                }
                                            }
                                            break;
                                        }
                                        _ = conn_shutdown_rx.changed() => {
                                            if *conn_shutdown_rx.borrow() {
                                                // Graceful shutdown - let current request complete
                                                conn.as_mut().graceful_shutdown();
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "TLS handshake failed");
                            }
                        }
                    } else {
                        // Plain TCP connection - uses auto protocol detection
                        // Supports both HTTP/1.1 and HTTP/2 prior knowledge (h2c)
                        let io = TokioIo::new(stream);
                        let mut builder = auto::Builder::new(TokioExecutor::new());
                        builder.http1().keep_alive(true);
                        builder
                            .http2()
                            .keep_alive_interval(Some(std::time::Duration::from_secs(20)));
                        let conn = builder.serve_connection_with_upgrades(io, service);

                        // Pin the connection for graceful shutdown
                        tokio::pin!(conn);

                        loop {
                            tokio::select! {
                                result = conn.as_mut() => {
                                    if let Err(e) = result {
                                        if !e.to_string().contains("connection closed") {
                                            tracing::debug!(error = %e, "connection error");
                                        }
                                    }
                                    break;
                                }
                                _ = conn_shutdown_rx.changed() => {
                                    if *conn_shutdown_rx.borrow() {
                                        // Graceful shutdown - let current request complete
                                        conn.as_mut().graceful_shutdown();
                                    }
                                }
                            }
                        }
                    }

                    // Connection closed
                    conn_metrics.connection_closed();
                    conn_counter.fetch_sub(1, Ordering::SeqCst);
                });
            }
        }
    }

    // Wait for active connections to drain (with timeout)
    let drain_start = Instant::now();
    loop {
        let active = active_connections.load(Ordering::SeqCst);
        if active == 0 {
            eprintln!("barbacane: all connections drained, shutting down");
            break;
        }

        if drain_start.elapsed() > shutdown_duration {
            eprintln!(
                "barbacane: shutdown timeout reached, {} connection(s) still active",
                active
            );
            break;
        }

        eprintln!(
            "barbacane: waiting for {} active connection(s) to complete...",
            active
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    ExitCode::SUCCESS
}

/// Wait for shutdown signal (SIGTERM or SIGINT).
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
        let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");

        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to register ctrl+c handler");
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Install the default crypto provider for rustls (required for TLS operations).
    // This must be done before any TLS operations. Ignore errors if already installed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();

    match cli.command {
        Commands::Compile {
            spec,
            output,
            manifest,
            allow_plaintext,
        } => run_compile(&spec, &output, &manifest, allow_plaintext),
        Commands::Validate { spec, format } => run_validate(&spec, &format),
        Commands::Init {
            name,
            template,
            fetch_plugins,
        } => run_init(&name, &template, fetch_plugins).await,
        Commands::Serve {
            artifact,
            listen,
            dev,
            log_level,
            log_format,
            otlp_endpoint,
            trace_sampling,
            max_body_size,
            max_headers,
            max_header_size,
            max_uri_length,
            allow_plaintext_upstream,
            tls_cert,
            tls_key,
            tls_min_version,
            keepalive_timeout,
            shutdown_timeout,
            control_plane,
            project_id,
            api_key,
            data_plane_name,
        } => {
            // Initialize telemetry
            let log_fmt = barbacane_telemetry::LogFormat::parse(&log_format)
                .unwrap_or(barbacane_telemetry::LogFormat::Json);

            let mut telemetry_config = barbacane_telemetry::TelemetryConfig::new()
                .with_log_level(&log_level)
                .with_log_format(log_fmt)
                .with_trace_sampling(trace_sampling);

            if let Some(endpoint) = otlp_endpoint {
                telemetry_config = telemetry_config.with_otlp_endpoint(endpoint);
            }

            let telemetry = match barbacane_telemetry::Telemetry::init(telemetry_config) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: failed to initialize telemetry: {}", e);
                    return ExitCode::from(1);
                }
            };

            let metrics = telemetry.metrics_clone();

            // Validate TLS min version
            if tls_min_version != "1.2" && tls_min_version != "1.3" {
                eprintln!(
                    "error: --tls-min-version must be '1.2' or '1.3', got '{}'",
                    tls_min_version
                );
                return ExitCode::from(1);
            }

            // Validate TLS arguments
            let tls_config = match (tls_cert, tls_key) {
                (Some(cert), Some(key)) => Some(TlsConfig {
                    cert_path: cert,
                    key_path: key,
                    min_version: tls_min_version,
                }),
                (None, None) => None,
                (Some(_), None) => {
                    eprintln!("error: --tls-cert requires --tls-key");
                    return ExitCode::from(1);
                }
                (None, Some(_)) => {
                    eprintln!("error: --tls-key requires --tls-cert");
                    return ExitCode::from(1);
                }
            };

            let limits = RequestLimits {
                max_body_size,
                max_headers,
                max_header_size,
                max_uri_length,
            };

            // Validate connected mode options
            let connected_mode = match (&control_plane, &project_id, &api_key) {
                (Some(cp), Some(pid), Some(key)) => {
                    // Parse project_id as UUID
                    let project_uuid = match uuid::Uuid::parse_str(pid) {
                        Ok(u) => u,
                        Err(_) => {
                            eprintln!("error: --project-id must be a valid UUID");
                            return ExitCode::from(1);
                        }
                    };
                    Some(ConnectedModeConfig {
                        control_plane_url: cp.clone(),
                        project_id: project_uuid,
                        api_key: key.clone(),
                        data_plane_name: data_plane_name.clone(),
                    })
                }
                (None, None, None) => None,
                _ => {
                    eprintln!(
                        "error: --control-plane, --project-id, and --api-key must all be specified together"
                    );
                    return ExitCode::from(1);
                }
            };

            run_serve(
                &artifact,
                &listen,
                dev,
                limits,
                allow_plaintext_upstream,
                tls_config,
                metrics,
                keepalive_timeout,
                shutdown_timeout,
                connected_mode,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_url_to_http_base() {
        assert_eq!(
            ws_url_to_http_base("ws://localhost:9090/ws/data-plane"),
            "http://localhost:9090"
        );
        assert_eq!(
            ws_url_to_http_base("wss://control.example.com/ws/data-plane"),
            "https://control.example.com"
        );
        assert_eq!(
            ws_url_to_http_base("ws://10.0.0.1:8080/ws/data-plane"),
            "http://10.0.0.1:8080"
        );
        // No path
        assert_eq!(
            ws_url_to_http_base("ws://localhost:9090"),
            "http://localhost:9090"
        );
    }
}
