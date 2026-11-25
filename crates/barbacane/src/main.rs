//! Barbacane API gateway.
//!
//! Compiles OpenAPI specs into artifacts and runs the data plane server.

mod waf;

use barbacane_lib::client_ip::{resolve_forwarded, Arrival, ClientIpHeader, TrustedProxies};
use barbacane_lib::lifecycle::background;
use barbacane_lib::metered_body::MeteredBody;
use barbacane_lib::tls::{self, TlsSettings};
use barbacane_lib::{admin, control_plane, hot_reload};

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use bytes::Bytes;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};

/// Boxed response body type that works for both buffered and streaming responses.
type AnyBody = BoxBody<Bytes, Infallible>;

/// Convert a buffered `Response<Full<Bytes>>` to the unified `Response<AnyBody>`.
fn box_full(r: Response<Full<Bytes>>) -> Response<AnyBody> {
    r.map(BoxBody::new)
}
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tracing::Instrument;
use uuid::Uuid;

/// The `Server` header: the product, no version, since a version on every
/// response tells a scanner what to try. The admin API carries the version.
const SERVER_HEADER: &str = "barbacane";

/// Metric `path` label used for requests that never matched a declared route
/// (unmatched, method-not-allowed, or rejected before routing). Using a fixed
/// sentinel instead of the raw request path keeps Prometheus series cardinality
/// bounded — a scanner hitting random URLs cannot create unbounded time-series.
const UNMATCHED_ROUTE_LABEL: &str = "<unmatched>";

use barbacane_telemetry::MetricsRegistry;
use std::collections::{HashMap, HashSet};

use barbacane_compiler::{
    compile_with_manifest, ArtifactArchive, CompileOptions, CompiledOperation, DeclaredLimits,
    Manifest, ProjectManifest, VerifiedArtifact,
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

/// A spec as the introspection endpoints serve it, in both formats.
struct ServedSpec {
    json: Bytes,
    yaml: Bytes,
}

impl ServedSpec {
    fn new(value: &serde_json::Value) -> Self {
        Self {
            json: Bytes::from(serde_json::to_string_pretty(value).unwrap_or_default()),
            yaml: Bytes::from(barbacane_compiler::yaml::to_string(value).unwrap_or_default()),
        }
    }
}

/// What the introspection endpoints serve, built from the artifact's specs
/// when it is loaded, so a request parses and serializes nothing.
struct Introspection {
    /// `/__barbacane/specs`.
    index: Bytes,
    /// `/__barbacane/specs/{file}`: each spec stripped of `x-barbacane-*`
    /// keys, `None` for one that does not parse.
    files: HashMap<String, Option<ServedSpec>>,
    /// All embedded OpenAPI specs merged; none when the artifact holds none.
    merged_openapi: Option<ServedSpec>,
    /// All embedded AsyncAPI specs merged.
    merged_asyncapi: Option<ServedSpec>,
}

impl Introspection {
    fn new(specs: &HashMap<String, String>) -> Self {
        let mut openapi_specs = Vec::new();
        let mut asyncapi_specs = Vec::new();
        let mut files = HashMap::with_capacity(specs.len());
        for (name, content) in specs {
            let entry = serde_json::json!({
                "name": name,
                "url": format!("/__barbacane/specs/{}", name),
            });
            match detect_spec_type(content) {
                SpecType::OpenApi => openapi_specs.push(entry),
                SpecType::AsyncApi => asyncapi_specs.push(entry),
                SpecType::Unknown => {}
            }
            let served = match barbacane_compiler::yaml::from_str::<serde_json::Value>(content) {
                Ok(mut value) => {
                    strip_barbacane_keys_recursive(&mut value);
                    Some(ServedSpec::new(&value))
                }
                Err(e) => {
                    tracing::error!(spec = %name, error = %e, "spec cannot be parsed for serving");
                    None
                }
            };
            files.insert(name.clone(), served);
        }
        let index = serde_json::json!({
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
        Self {
            index: Bytes::from(index.to_string()),
            files,
            merged_openapi: merged_specs(specs, SpecType::OpenApi, merge_openapi_specs)
                .map(|v| ServedSpec::new(&v)),
            merged_asyncapi: merged_specs(specs, SpecType::AsyncApi, merge_asyncapi_specs)
                .map(|v| ServedSpec::new(&v)),
        }
    }
}

/// The specs of `kind` in `specs` merged with `merge`, or `None` when there
/// is none.
fn merged_specs(
    specs: &HashMap<String, String>,
    kind: SpecType,
    merge: fn(&[(&String, &String)]) -> serde_json::Value,
) -> Option<serde_json::Value> {
    let of_kind: Vec<_> = specs
        .iter()
        .filter(|(_, content)| detect_spec_type(content) == kind)
        .collect();
    (!of_kind.is_empty()).then(|| merge(&of_kind))
}

/// Detect whether a spec is OpenAPI or AsyncAPI by checking root keys.
fn detect_spec_type(content: &str) -> SpecType {
    // Try to parse as YAML (also handles JSON)
    let parsed: Result<serde_json::Value, _> = barbacane_compiler::yaml::from_str(content);
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
        let parsed: Option<serde_json::Value> = barbacane_compiler::yaml::from_str(content).ok();
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
        let parsed: Option<serde_json::Value> = barbacane_compiler::yaml::from_str(content).ok();
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
use barbacane_wasm::balancer::Balancer;
use barbacane_wasm::{
    HttpClient, HttpClientConfig, InstancePool, PluginLimits, PoolSize, RateLimiter, ResponseCache,
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
    /// Manage the Ed25519 keys that sign artifacts.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },

    /// Compile OpenAPI spec(s) into a .bca artifact.
    Compile {
        /// Input spec file(s) (YAML or JSON).
        /// If omitted, discovers specs from the manifest's `specs` folder.
        #[arg(short, long, num_args = 1..)]
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

        /// Git commit SHA for build provenance tracking.
        #[arg(long)]
        provenance_commit: Option<String>,

        /// Build source identifier for provenance (e.g., "ci/github-actions").
        #[arg(long)]
        provenance_source: Option<String>,

        /// Bypass the plugin download cache entirely (no read, no write).
        #[arg(long)]
        no_cache: bool,
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

    /// Check that an artifact loads as `serve` would load it, without serving.
    ///
    /// Runs every load-time check: format version, signature, integrity, plugin
    /// ABI, imports and capabilities, limits, WAF rules, schemas and secrets,
    /// then each plugin's `init` with every configuration the routes give it.
    /// Run it with the environment `serve` gets: it reads
    /// BARBACANE_TRUSTED_PUBKEY and resolves secrets.
    Check {
        /// Path to the .bca artifact file.
        #[arg(long)]
        artifact: String,

        /// Accept an artifact no trusted key has signed, when
        /// BARBACANE_TRUSTED_PUBKEY is unset.
        #[arg(long)]
        allow_unsigned: bool,
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

    /// Start a local development server with auto-reload.
    ///
    /// Compiles specs from barbacane.yaml, starts the gateway, watches for
    /// file changes, and hot-reloads automatically. Equivalent to running
    /// compile + serve in a loop.
    Dev {
        /// Listen address.
        #[arg(long, default_value = "0.0.0.0:8080")]
        listen: String,

        /// Path to barbacane.yaml manifest.
        #[arg(short, long, default_value = "barbacane.yaml")]
        manifest: String,

        /// Override spec files (uses these instead of the manifest's specs folder).
        #[arg(short, long)]
        spec: Vec<String>,

        /// Log level (error, warn, info, debug, trace).
        #[arg(long, default_value = "info")]
        log_level: String,

        /// Admin API listen address. Set to "off" to disable.
        #[arg(long, default_value = "127.0.0.1:8081")]
        admin_bind: String,

        /// Debounce delay in milliseconds before recompiling after a file change.
        #[arg(long, default_value = "300")]
        debounce_ms: u64,

        /// Log format (json or pretty).
        #[arg(long, default_value = "pretty")]
        log_format: String,

        /// Maximum request body size in bytes (default: 1048576 = 1MB).
        #[arg(long, default_value = "1048576")]
        max_body_size: usize,

        /// Maximum number of request headers (default: 100).
        #[arg(long, default_value = "100")]
        max_headers: usize,

        /// Maximum size of a single header in bytes (default: 8192 = 8KB).
        #[arg(long, default_value = "8192")]
        max_header_size: usize,

        /// Maximum URI length in bytes (default: 8192 = 8KB).
        #[arg(long, default_value = "8192")]
        max_uri_length: usize,
    },

    /// Run the gateway server.
    Serve {
        /// Path to the .bca artifact file. Required without --control-plane;
        /// in connected mode, the artifact served until the control plane
        /// sends its deployment, and optional.
        #[arg(long)]
        artifact: Option<String>,

        /// Directory the artifacts the control plane deploys are kept in
        /// (connected mode). Defaults to the directory of --artifact.
        #[arg(long, env = "BARBACANE_ARTIFACT_DIR")]
        artifact_dir: Option<String>,

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

        /// Maximum URI length in bytes (default: 8192 = 8KB).
        #[arg(long, default_value = "8192")]
        max_uri_length: usize,

        /// Allow plaintext HTTP upstream connections (development only).
        /// In production, only HTTPS upstreams are allowed.
        #[arg(long)]
        allow_plaintext_upstream: bool,

        /// Allow a plaintext ws:// control plane link to a host other than
        /// loopback. The link carries the data plane's API key; use wss://.
        #[arg(long)]
        allow_plaintext_control_plane: bool,

        /// Load artifacts no trusted key has signed, when BARBACANE_TRUSTED_PUBKEY
        /// is unset (development and testing only; logged at every load).
        #[arg(long)]
        allow_unsigned: bool,

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

        /// CA certificates (PEM) that client certificates must chain to.
        /// Every client then presents one (mutual TLS). Requires --tls-cert.
        #[arg(long)]
        tls_client_ca: Option<String>,

        /// Seconds between checks of the certificate, key and client CA files;
        /// changed files are loaded without a restart. 0 turns the check off.
        /// SIGHUP reloads them at any time.
        #[arg(long, default_value = "60")]
        tls_reload_interval: u64,

        /// HTTP keep-alive idle timeout in seconds (default: 60).
        #[arg(long, default_value = "60")]
        keepalive_timeout: u64,

        /// Graceful shutdown timeout in seconds (default: 30).
        /// After SIGTERM, wait this long for in-flight requests to complete.
        #[arg(long, default_value = "30")]
        shutdown_timeout: u64,

        /// Maximum number of concurrently served connections; a connection
        /// past it is closed at once. Each takes at least one file descriptor,
        /// so keep it below the open-file limit (`ulimit -n`).
        #[arg(
            long,
            env = "BARBACANE_MAX_CONNECTIONS",
            default_value_t = DEFAULT_MAX_CONNECTIONS,
            value_parser = parse_max_connections,
        )]
        max_connections: usize,

        /// Plugin instances alive at once that the gateway keeps preallocated
        /// memory for. Each reserves about 4 GiB of virtual address space, not
        /// resident memory. Instances past it are allocated per request; 0
        /// turns the pool off.
        #[arg(
            long,
            env = "BARBACANE_PLUGIN_INSTANCE_POOL",
            default_value_t = DEFAULT_PLUGIN_INSTANCE_POOL,
            value_parser = parse_plugin_instance_pool,
        )]
        plugin_instance_pool: u32,

        // Connected mode options (optional)
        /// Control plane WebSocket URL (e.g., ws://control:9090/ws/data-plane).
        /// When provided, the data plane connects to the control plane for centralized management.
        #[arg(long)]
        control_plane: Option<String>,

        /// Project ID (UUID) for control plane registration.
        /// Required if --control-plane is specified.
        #[arg(long)]
        project_id: Option<String>,

        /// API key for control plane authentication.
        /// Required if --control-plane is specified.
        #[arg(long, env = "BARBACANE_CONTROL_API_KEY", hide_env_values = true)]
        api_key: Option<String>,

        /// Data plane name for identification in control plane.
        #[arg(long)]
        data_plane_name: Option<String>,

        /// Admin API listen address (health, metrics, provenance).
        /// Set to "off" to disable.
        #[arg(long, default_value = "127.0.0.1:8081")]
        admin_bind: String,

        /// Do not serve the embedded specs at /__barbacane/specs; the paths
        /// answer 404.
        #[arg(long)]
        no_introspection: bool,

        /// Proxies whose connections report the client address, as
        /// comma-separated addresses or CIDR ranges (10.0.0.0/8,192.0.2.7).
        /// Without it, every request is attributed to its TCP peer.
        #[arg(long, env = "BARBACANE_TRUSTED_PROXIES", default_value = "")]
        trusted_proxies: TrustedProxies,

        /// The header trusted proxies report the client in:
        /// x-forwarded-for or forwarded (RFC 7239).
        #[arg(
            long,
            env = "BARBACANE_CLIENT_IP_HEADER",
            default_value = "x-forwarded-for"
        )]
        client_ip_header: ClientIpHeader,
    },
}

// =============================================================================
// Hot-Reload Types
// =============================================================================

/// Shared gateway state that can be atomically swapped for hot-reload.
type SharedGateway = Arc<ArcSwap<Gateway>>;

/// Whether `/__barbacane/specs*` serves the embedded specs. Off with
/// `--no-introspection`, for the life of the process.
static INTROSPECTION_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

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
    /// WAF stage, when the artifact carries a rule set. Compiled once at
    /// startup; evaluation borrows it.
    waf: Option<waf::WafStage>,
    /// What the introspection endpoints serve, built once at load.
    introspection: Introspection,
    /// Request limits (body size, headers, URI length).
    limits: RequestLimits,
    dev_mode: bool,
    /// Whether plugin egress to internal/loopback/metadata targets is allowed
    /// (BARBACANE_ALLOW_INTERNAL_EGRESS). The HTTP/broker clients carry their own
    /// copy; this one guards WebSocket upstream connections.
    allow_internal_egress: bool,
    /// Whether `ws://` upstreams are allowed (`--allow-plaintext-upstream`).
    allow_plaintext_upstream: bool,
    /// WASM engine for plugin execution (kept alive for engine lifetime).
    _wasm_engine: Arc<WasmEngine>,
    /// Plugin instance pool.
    plugin_pool: Arc<InstancePool>,
    /// Size of the WASM engine's instance pool, handed to the next artifact's
    /// gateway.
    plugin_instance_pool: u32,
    /// Endpoint pool state, handed to the next artifact's gateway.
    balancer: Arc<Balancer>,
    /// Metrics registry for observability.
    metrics: Arc<MetricsRegistry>,
    /// API name from the first spec's title (for metrics labels).
    api_name: String,
}

impl Gateway {
    /// Load a gateway from a .bca artifact.
    #[allow(clippy::too_many_arguments)]
    fn load(
        artifact_path: &Path,
        dev_mode: bool,
        limits: RequestLimits,
        allow_plaintext_upstream: bool,
        metrics: Arc<MetricsRegistry>,
        trust: &ArtifactTrust,
        balancer: Option<Arc<Balancer>>,
        plugin_instance_pool: u32,
    ) -> Result<Self, String> {
        let archive = ArtifactArchive::read(artifact_path)
            .map_err(|e| format!("failed to read artifact: {}", e))?;

        check_artifact_version(archive.manifest().barbacane_artifact_version)?;

        // AR-1: authenticate the artifact before using any of it. With a pinned
        // key the Ed25519 signature is required; `artifact_hash` and every
        // archive member are always checked against the manifest.
        match trust {
            ArtifactTrust::Pinned(pubkey) => {
                barbacane_compiler::verify_artifact_signature(archive.manifest(), pubkey)
                    .map_err(|e| format!("artifact signature check failed: {e}"))?;
                tracing::info!("artifact Ed25519 signature verified");
            }
            ArtifactTrust::Unsigned => {
                tracing::warn!(
                    "artifact signature not checked: no key is pinned in \
                     BARBACANE_TRUSTED_PUBKEY, and --dev or --allow-unsigned lets unsigned \
                     artifacts load"
                );
            }
        }

        let VerifiedArtifact {
            manifest,
            routes,
            specs,
            plugins: bundled_plugins,
            waf_rules,
        } = archive
            .verify()
            .map_err(|e| format!("artifact integrity check failed: {}", e))?;

        // Initialize HTTP client for upstream requests and plugin outbound calls.
        // SSRF guard is on by default; operators opt out for trusted internal
        // upstreams via BARBACANE_ALLOW_INTERNAL_EGRESS.
        let allow_internal_egress = matches!(
            std::env::var("BARBACANE_ALLOW_INTERNAL_EGRESS")
                .ok()
                .as_deref(),
            Some("1" | "true" | "TRUE" | "yes")
        );
        // Cap the body the buffered plugin-egress path will read into host
        // memory. Operators can raise/lower it; default is 16 MiB.
        let max_response_bytes = std::env::var("BARBACANE_MAX_UPSTREAM_RESPONSE_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| HttpClientConfig::default().max_response_bytes);
        let http_client_config = HttpClientConfig {
            allow_plaintext: allow_plaintext_upstream,
            allow_internal_egress,
            max_response_bytes,
            ..Default::default()
        };
        // Endpoint pool state carries over from the previous artifact.
        let balancer = balancer.unwrap_or_default();
        let http_client = HttpClient::with_balancer(http_client_config, Arc::clone(&balancer))
            .map_err(|e| format!("failed to create HTTP client: {}", e))?;
        let http_client = Arc::new(http_client);

        // Each plugin runs with the limits its manifest declares, the defaults
        // for the rest, and room for one body if it receives bodies.
        let mut per_plugin_limits = HashMap::new();
        for plugin in &manifest.plugins {
            let declared = &plugin.capabilities.limits;
            declared
                .check()
                .map_err(|e| format!("plugin '{}' declares invalid limits: {}", plugin.name, e))?;
            let receives_body =
                plugin.plugin_type != "middleware" || plugin.capabilities.body_access;
            per_plugin_limits.insert(
                plugin.name.clone(),
                limits_for_plugin(declared, receives_body, limits.max_body_size),
            );
        }
        let plugin_limits =
            limits_for_plugin(&DeclaredLimits::default(), true, limits.max_body_size);
        let pool = PoolSize {
            instances: plugin_instance_pool,
            max_memory_bytes: per_plugin_limits
                .values()
                .chain([&plugin_limits])
                .map(|limits| limits.max_memory_bytes)
                .max()
                .unwrap_or(plugin_limits.max_memory_bytes),
        };
        let wasm_engine = WasmEngine::with_pool(plugin_limits.clone(), Some(pool))
            .map_err(|e| format!("failed to create WASM engine: {}", e))?;
        if wasm_engine.pool_instances() > 0 {
            tracing::info!(
                instances = wasm_engine.pool_instances(),
                max_memory_mib = pool.max_memory_bytes.div_ceil(1024 * 1024),
                "plugin instance pool ready"
            );
        }
        let wasm_engine = Arc::new(wasm_engine);

        if bundled_plugins.is_empty() {
            tracing::warn!("no plugins bundled in artifact - ensure barbacane.yaml manifest was used during compilation");
        }

        // Build the WAF stage only after the artifact has been authenticated.
        // The rule set is bound by artifact_hash, so compiling it before that
        // check would mean acting on rules an attacker could have swapped.
        // Refusing to start beats starting with a rule set that silently did
        // not load.
        let waf_stage = match waf::WafStage::from_rules(&manifest, waf_rules) {
            Ok(stage) => stage,
            Err(e) => return Err(format!("WAF: {e}")),
        };
        if let Some(stage) = &waf_stage {
            let (request_rules, response_rules) = stage.rules_by_direction();
            tracing::info!(
                rules = stage.rule_count(),
                request_phase_rules = request_rules,
                response_phase_rules = response_rules,
                blocking = stage.is_blocking(),
                paranoia_level = manifest.waf.paranoia_level,
                skipped_rules = ?manifest.waf.skipped_rules,
                "WAF enabled"
            );
            if !manifest.waf.skipped_rules.is_empty() {
                // Loud on every boot: an operator should not have to read the
                // manifest to discover the rule set is incomplete.
                tracing::warn!(
                    skipped_rules = ?manifest.waf.skipped_rules,
                    "WAF rule set is incomplete: these rules are not enforced by this build"
                );
            }
        }

        // WA-1: capability enforcement needs per-plugin capabilities that are
        // authoritative: those the CLI reads from plugin.toml and the control
        // plane from each plugin's embedded manifest. The host links every host
        // function, so the import check below is what keeps a plugin to its
        // declared capabilities; an artifact without them is not loaded.
        if !manifest.capabilities_enforced {
            return Err("the artifact carries no authoritative plugin capabilities (it was compiled without a manifest); compile it with `barbacane compile --manifest barbacane.yaml`".to_string());
        }

        // Compile all plugin modules first (we'll register them after creating the final pool)
        let mut compiled_modules = Vec::new();
        for (name, loaded) in bundled_plugins {
            // The plugin is built for this ABI and exports what the gateway calls.
            if let Some(mismatch) =
                barbacane_compiler::abi::check(&loaded.wasm_bytes, &loaded.plugin_type)
                    .map_err(|e| format!("plugin '{}' cannot be loaded: {}", name, e))?
            {
                return Err(format!(
                    "plugin '{}' cannot be loaded: {}; rebuild it with barbacane-plugin-sdk 1.x",
                    name, mismatch
                ));
            }
            for deprecated in barbacane_compiler::abi::deprecated_imports(&loaded.wasm_bytes)
                .map_err(|e| format!("plugin '{}' cannot be loaded: {}", name, e))?
            {
                tracing::warn!(
                    plugin = %name,
                    import = deprecated.name,
                    replacement = deprecated.replacement,
                    removed_in = deprecated.removal,
                    "plugin imports a deprecated host function"
                );
            }

            let module = wasm_engine
                .compile(
                    &loaded.wasm_bytes,
                    name.clone(),
                    loaded.version.clone(),
                    loaded.body_access,
                )
                .map_err(|e| format!("failed to compile plugin '{}': {}", name, e))?;

            // The exports the gateway calls have the signatures it calls them with.
            if let Some(plugin_type) = barbacane_wasm::PluginType::parse(&loaded.plugin_type) {
                barbacane_wasm::validate_exports(module.module(), plugin_type)
                    .map_err(|e| format!("plugin '{}' cannot be loaded: {}", name, e))?;
            }

            // A plugin imports only what the host provides: `barbacane` host
            // functions and the supported WASI functions.
            barbacane_wasm::validate_import_modules(module.module())
                .map_err(|e| format!("plugin '{}' cannot be loaded: {}", name, e))?;

            // WA-1: enforce the capability contract. The plugin may only import
            // host functions covered by the capabilities it declared in
            // plugin.toml (carried in the artifact manifest); anything else is a
            // hard load failure (default-deny).
            let declared = manifest
                .plugins
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.capabilities.host_functions.clone())
                .unwrap_or_default();
            barbacane_wasm::validate_imports(module.module(), &declared).map_err(|e| {
                format!(
                    "plugin '{}' violates its declared capabilities: {}",
                    name, e
                )
            })?;

            compiled_modules.push((name, loaded.version, module));
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

        // WA-3: map each plugin name to the secret references in its own config
        // (collected from the raw, pre-substitution configs), so the pool can
        // scope host_get_secret per plugin — a plugin can resolve the secrets it
        // declares, not another plugin's secrets held in the shared store.
        let mut secret_refs_by_plugin: HashMap<String, HashSet<String>> = HashMap::new();
        for op in &routes.operations {
            for mw in &op.middlewares {
                secret_refs_by_plugin
                    .entry(mw.name.clone())
                    .or_default()
                    .extend(barbacane_wasm::collect_secret_references(&mw.config));
            }
            secret_refs_by_plugin
                .entry(op.dispatch.name.clone())
                .or_default()
                .extend(barbacane_wasm::collect_secret_references(
                    &op.dispatch.config,
                ));
        }

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

        // Create NATS publisher for host_nats_publish calls. Broker egress
        // honors the same internal-egress policy as plugin HTTP calls.
        let nats_publisher = barbacane_wasm::NatsPublisher::new(allow_internal_egress)
            .map_err(|e| format!("failed to create NATS publisher: {}", e))?;

        // Create Kafka publisher for host_kafka_publish calls
        let kafka_publisher = barbacane_wasm::KafkaPublisher::new(allow_internal_egress)
            .map_err(|e| format!("failed to create Kafka publisher: {}", e))?;

        // Create LDAP client for host_ldap_bind / host_ldap_search calls; directory
        // egress honors the same internal-egress policy as plugin HTTP calls.
        let ldap_client = barbacane_wasm::LdapClient::new(allow_internal_egress)
            .map_err(|e| format!("failed to create LDAP client: {}", e))?;

        // Create pool with all options: HTTP client, secrets, rate limiter, cache, NATS, Kafka, and LDAP
        let plugin_pool = InstancePool::with_all_options(
            wasm_engine.clone(),
            plugin_limits,
            Some(http_client),
            Some(secrets_store),
            Some(rate_limiter),
            Some(response_cache),
            Some(Arc::new(nats_publisher)),
            Some(Arc::new(kafka_publisher)),
            Some(Arc::new(ldap_client)),
        )
        .with_secret_scopes(secret_refs_by_plugin)
        .with_plugin_limits(per_plugin_limits)
        .with_metrics(Arc::clone(&metrics));

        // Register all compiled modules in the pool
        for (name, version, module) in compiled_modules {
            plugin_pool.register_module(module);
            if dev_mode {
                tracing::debug!(plugin = %name, version = %version, "loaded plugin from artifact");
            }
        }

        // Register every (plugin, config) pair the routes use, once.
        for op in &resolved_operations {
            for (name, config) in plugin_configs(op) {
                let key = barbacane_wasm::InstanceKey::new(name, config);
                plugin_pool.register_config(key, init_config(config));
            }
        }

        let mut router = Router::new();
        let mut validators = Vec::new();

        for (position, op) in resolved_operations.iter().enumerate() {
            // An operation's index is its position: the request path indexes
            // the validators by it, so a mismatch is refused here rather than
            // met as an out-of-range index per request.
            if op.index != position {
                return Err(format!(
                    "artifact operation {} {} carries index {} at position {position}",
                    op.method, op.path, op.index
                ));
            }
            // Map AsyncAPI methods to HTTP methods for sync-to-async bridge pattern:
            // - SEND → POST (publish message via HTTP POST, get 202 Accepted)
            // - RECEIVE → GET (for SSE/WebSocket subscriptions, less common)
            let http_method = match op.method.as_str() {
                "SEND" => "POST",
                "RECEIVE" => "GET",
                other => other,
            };
            for path in barbacane_compiler::artifact::served_paths(&op.base_paths, &op.path) {
                router.insert(
                    &path,
                    http_method,
                    RouteEntry {
                        operation_index: op.index,
                    },
                );
            }

            // Pre-compile validator for this operation. Schemas point into the
            // artifact's shared definition pool rather than carrying their own.
            // An artifact holding a schema that cannot be enforced is refused.
            let validator = OperationValidator::with_defs(
                &op.parameters,
                op.request_body.as_ref(),
                routes.schema_defs.as_ref(),
            )
            .map_err(|e| format!("{} {}: {e}", op.method, op.path))?;
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

        // Keep pool state for the pools this artifact's configs list, drop the
        // rest with their metric series (ADR-0035). Done last, so a load that
        // fails leaves it as it was.
        for endpoint in balancer.retain(&plugin_pool.pools()) {
            metrics.forget_upstream_endpoint(&endpoint);
        }

        // Parsed and serialized here, not on each introspection request.
        let introspection = Introspection::new(&specs);

        Ok(Gateway {
            introspection,
            manifest,
            router,
            operations: resolved_operations,
            validators,
            waf: waf_stage,
            limits,
            dev_mode,
            allow_internal_egress,
            allow_plaintext_upstream,
            _wasm_engine: wasm_engine,
            plugin_pool: Arc::new(plugin_pool),
            plugin_instance_pool,
            balancer,
            metrics,
            api_name,
        })
    }

    /// Add standard headers to a response.
    ///
    /// Includes:
    /// - Server version
    /// - Request/trace IDs for observability
    /// - Security headers (X-Content-Type-Options, X-Frame-Options)
    fn add_standard_headers<B>(
        mut response: Response<B>,
        request_id: &str,
        trace_id: &str,
    ) -> Response<B> {
        let headers = response.headers_mut();

        // Observability headers. `request_id`/`trace_id` may originate from
        // client-supplied headers, so never panic on a value that isn't a legal
        // header value — fall back to a placeholder instead.
        headers.insert("server", HeaderValue::from_static(SERVER_HEADER));
        headers.insert(
            "x-request-id",
            HeaderValue::from_str(request_id)
                .unwrap_or_else(|_| HeaderValue::from_static("invalid")),
        );
        headers.insert(
            "x-trace-id",
            HeaderValue::from_str(trace_id).unwrap_or_else(|_| HeaderValue::from_static("invalid")),
        );

        // Security headers (enabled by default)
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        headers.insert("x-frame-options", HeaderValue::from_static("DENY"));

        response
    }

    /// Add the lifecycle headers of the operation: `Deprecation: true` when it
    /// is deprecated (draft-ietf-httpapi-deprecation-header), and `Sunset`
    /// when it has an `x-sunset` date (RFC 8594), deprecated or not.
    fn add_deprecation_headers<B>(
        mut response: Response<B>,
        operation: &CompiledOperation,
    ) -> Response<B> {
        let headers = response.headers_mut();
        if operation.deprecated {
            headers.insert("deprecation", HeaderValue::from_static("true"));
        }
        if let Some(sunset) = operation.sunset.as_deref().and_then(|s| s.parse().ok()) {
            headers.insert("sunset", sunset);
        }
        response
    }

    /// Handle an incoming HTTP request, inside a span carrying its request and
    /// trace IDs so every log line of the request can be matched to its response.
    /// The span is at ERROR level so it is enabled whenever any log line is.
    async fn handle_request(
        &self,
        req: Request<Incoming>,
        client_addr: Option<SocketAddr>,
        trusted_proxies: &TrustedProxies,
        tls: bool,
    ) -> Result<Response<AnyBody>, Infallible> {
        let start_time = Instant::now();
        let arrival = Arrival {
            tls,
            via_trusted_proxy: client_addr.is_some_and(|peer| trusted_proxies.trusts(peer.ip())),
        };
        // A value that is not text is an unreadable hop, which ends the walk.
        let client_ip = client_addr.map(|peer| {
            trusted_proxies.client_ip(
                peer.ip(),
                req.headers()
                    .get_all(trusted_proxies.header().name())
                    .iter()
                    .map(|value| value.to_str().unwrap_or("")),
            )
        });
        control_plane::REQUESTS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // The client's request id and trace id are kept when well-formed;
        // otherwise new ones are generated. Both go into every log line of the
        // request and into response headers, so their shape is bounded.
        let request_id = req
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .and_then(client_request_id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let trace_id = req
            .headers()
            .get("traceparent")
            .and_then(|v| v.to_str().ok())
            .and_then(trace_id_from_traceparent)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());

        let span = tracing::error_span!(
            "request",
            request_id = %request_id,
            trace_id = %trace_id,
            upstream_endpoint = tracing::field::Empty
        );
        self.handle_request_in_span(req, client_ip, arrival, start_time, request_id, trace_id)
            .instrument(span)
            .await
    }

    async fn handle_request_in_span(
        &self,
        req: Request<Incoming>,
        client_ip: Option<IpAddr>,
        arrival: Arrival,
        start_time: Instant,
        request_id: String,
        trace_id: String,
    ) -> Result<Response<AnyBody>, Infallible> {
        let uri_string = req.uri().to_string();
        let path = req.uri().path().to_string();
        let query_string = req.uri().query().map(|s| s.to_string());
        let method = req.method().clone();
        let method_str = method.as_str().to_string();

        // A `.` or `..` segment, in any spelling, would name another path to
        // a dispatcher or upstream that resolves it than the route matched.
        if barbacane_lib::validator::has_dot_segment(&path) {
            let response = self.bad_request_response("the path holds a `.` or `..` segment");
            self.record_request_metrics(
                &method_str,
                UNMATCHED_ROUTE_LABEL,
                response.status().as_u16(),
                0,
                0,
                start_time,
            );
            return Ok(box_full(Self::add_standard_headers(
                response,
                &request_id,
                &trace_id,
            )));
        }

        // Check URI length limit early
        if let Err(e) = self.limits.validate_uri(&uri_string) {
            let response = self.validation_error_response(&[e]);
            self.record_request_metrics(
                &method_str,
                // Bounded label: raw paths would let a scanner explode metric cardinality.
                UNMATCHED_ROUTE_LABEL,
                response.status().as_u16(),
                0,
                0,
                start_time,
            );
            return Ok(box_full(Self::add_standard_headers(
                response,
                &request_id,
                &trace_id,
            )));
        }

        // Reserved /__barbacane/* endpoints (skip other limits for internal endpoints)
        if path.starts_with("/__barbacane/") {
            let response = self.handle_barbacane_endpoint(&path, &method, query_string.as_deref());
            return Ok(box_full(Self::add_standard_headers(
                response,
                &request_id,
                &trace_id,
            )));
        }

        // Enforce header limits on the RAW header map first: `HeaderMap::len()`
        // counts every header line (including repeated names), so a flood of
        // duplicate-named headers can't slip under the count limit, and each
        // value is size-checked (not just the last one for a given name).
        let raw_headers = req.headers();
        let header_limit_error = self
            .limits
            .validate_header_count(raw_headers.len())
            .err()
            .or_else(|| {
                raw_headers.iter().find_map(|(name, value)| {
                    self.limits
                        .validate_header_size(name.as_str(), name.as_str().len() + value.len())
                        .err()
                })
            });

        // Extract headers for downstream validation and plugin forwarding. This
        // collapses duplicate names (last value wins, `cookie` joined), which is
        // fine now that the limit checks above ran against the raw map.
        // `x-forwarded-proto` and `x-forwarded-host` hold the scheme and host the
        // client used, not what it sent in them.
        let mut headers = request_header_map(req.headers());
        resolve_forwarded(
            &mut headers,
            arrival,
            req.uri().authority().map(|authority| authority.as_str()),
        );

        // The map above is what routing and validation use, and it is lossy in
        // two ways the WAF cannot afford: duplicate header names collapse, and
        // a value containing non-UTF-8 bytes is dropped entirely. A rule set
        // needs both. CRS 920620 exists to catch duplicate Content-Type
        // headers, and a payload hidden in a header with one invalid byte
        // would otherwise never be inspected at all.
        //
        // Values are converted lossily rather than dropped: the payload still
        // reaches the rules, and CRS's own encoding rules flag the mangling.
        // Passing raw bytes needs a byte-oriented header API in the engine.
        let waf_headers: Vec<(String, String)> = req
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();

        // The real protocol, not a constant: CRS 920430 checks REQUEST_PROTOCOL
        // against an allowed list and 920171 keys off HTTP/1.0 specifically, so
        // a hardcoded version makes both judge the wrong request.
        let waf_protocol = match req.version() {
            hyper::Version::HTTP_09 => "HTTP/0.9",
            hyper::Version::HTTP_10 => "HTTP/1.0",
            hyper::Version::HTTP_11 => "HTTP/1.1",
            hyper::Version::HTTP_2 => "HTTP/2.0",
            hyper::Version::HTTP_3 => "HTTP/3.0",
            _ => "HTTP/1.1",
        };

        // Check header limits
        if let Some(e) = header_limit_error {
            let response = self.validation_error_response(&[e]);
            self.record_request_metrics(
                &method_str,
                UNMATCHED_ROUTE_LABEL,
                response.status().as_u16(),
                0,
                0,
                start_time,
            );
            return Ok(box_full(Self::add_standard_headers(
                response,
                &request_id,
                &trace_id,
            )));
        }

        // Check content-length before reading body (if present)
        if let Some(content_length) = headers.get("content-length") {
            if let Ok(len) = content_length.parse::<usize>() {
                if self.limits.validate_body_size(len).is_err() {
                    let response = self.payload_too_large_response(self.limits.max_body_size);
                    self.record_request_metrics(
                        &method_str,
                        UNMATCHED_ROUTE_LABEL,
                        response.status().as_u16(),
                        0,
                        0,
                        start_time,
                    );
                    return Ok(box_full(Self::add_standard_headers(
                        response,
                        &request_id,
                        &trace_id,
                    )));
                }
            }
        }

        // Route lookup
        match self.router.lookup(&path, &method_str) {
            RouteMatch::Found { entry, mut params } => {
                // Path-parameter values are percent-decoded once, so validation
                // and the dispatcher see the same value (segments were matched
                // raw; `%2F` stays a literal slash within its segment, and `+`
                // stays `+`).
                for (_, value) in params.iter_mut() {
                    *value = barbacane_lib::validator::percent_decode_path(value);
                }
                let operation = &self.operations[entry.operation_index];
                let validator = &self.validators[entry.operation_index];
                let route_path = operation.path.clone();

                let content_type = headers.get("content-type").map(|s| s.as_str());

                // For WebSocket upgrade requests (ADR-0026), extract the upgrade
                // handle before consuming the body. Only a request shaped as a
                // handshake (RFC 6455 section 4.1) counts: anything else keeps
                // its body, which is read, validated and inspected as usual.
                let is_ws_upgrade = is_websocket_handshake(&method_str, &headers);

                let (body_bytes, upgrade_handle) = if is_ws_upgrade {
                    let permit = req.extensions().get::<ConnectionPermit>().cloned();
                    (
                        Bytes::new(),
                        Some(PendingUpgrade {
                            on_upgrade: hyper::upgrade::on(req),
                            permit,
                        }),
                    )
                } else {
                    // Cap the bytes read while collecting so a chunked body with
                    // no content-length can't be buffered past the limit (DoS).
                    // `Limited` errors as soon as the cap is exceeded.
                    // The whole body must arrive within `body_read_timeout`, so
                    // a client trickling bytes cannot hold the request open.
                    let limited =
                        http_body_util::Limited::new(req.into_body(), self.limits.max_body_size);
                    let collected =
                        tokio::time::timeout(self.limits.body_read_timeout, limited.collect())
                            .await;
                    match collected {
                        Ok(Ok(collected)) => (collected.to_bytes(), None),
                        Err(_elapsed) => {
                            self.metrics.record_validation_failure(
                                &method_str,
                                &route_path,
                                "body_timeout",
                            );
                            let response =
                                self.request_timeout_response(self.limits.body_read_timeout);
                            self.record_request_metrics(
                                &method_str,
                                &route_path,
                                response.status().as_u16(),
                                0,
                                0,
                                start_time,
                            );
                            return Ok(box_full(Self::add_standard_headers(
                                response,
                                &request_id,
                                &trace_id,
                            )));
                        }
                        Ok(Err(e)) => {
                            let response = if e
                                .downcast_ref::<http_body_util::LengthLimitError>()
                                .is_some()
                            {
                                self.metrics.record_validation_failure(
                                    &method_str,
                                    &route_path,
                                    "body_too_large",
                                );
                                self.payload_too_large_response(self.limits.max_body_size)
                            } else {
                                self.bad_request_response("failed to read request body")
                            };
                            self.record_request_metrics(
                                &method_str,
                                &route_path,
                                response.status().as_u16(),
                                0,
                                0,
                                start_time,
                            );
                            return Ok(box_full(Self::add_standard_headers(
                                response,
                                &request_id,
                                &trace_id,
                            )));
                        }
                    }
                };

                let request_size = body_bytes.len() as u64;

                // Validate actual body size (in case content-length was missing or wrong)
                if self.limits.validate_body_size(body_bytes.len()).is_err() {
                    self.metrics.record_validation_failure(
                        &method_str,
                        &route_path,
                        "body_too_large",
                    );
                    let response = self.payload_too_large_response(self.limits.max_body_size);
                    self.record_request_metrics(
                        &method_str,
                        &route_path,
                        response.status().as_u16(),
                        request_size,
                        0,
                        start_time,
                    );
                    return Ok(box_full(Self::add_standard_headers(
                        response,
                        &request_id,
                        &trace_id,
                    )));
                }

                // WAF inspection sits between the two models: the spec has
                // already said what the request may look like, and the rule
                // set now looks for attack shapes that are schema-valid.
                let mut waf_inspection: Option<waf::WafInspection> = None;
                // Rule ids counted in the request phase, so the response phase
                // counts only the rules it newly matched.
                let mut request_matched: Vec<u32> = Vec::new();
                if let Some(stage) = &self.waf {
                    let waf_start = std::time::Instant::now();
                    let (decision, mut inspection) = stage.inspect_request(
                        &method_str,
                        &uri_string,
                        waf_protocol,
                        &waf_headers,
                        &body_bytes,
                        content_type,
                        client_ip.map(|ip| ip.to_string()).as_deref(),
                    );
                    self.metrics.record_waf_inspection(
                        &method_str,
                        &route_path,
                        waf_start.elapsed().as_secs_f64(),
                        matches!(decision, waf::WafDecision::Allow),
                    );
                    // Count every rule that matched, not just the one that
                    // blocked. In detection-only mode nothing blocks, so this
                    // is the only signal available for tuning a rule set
                    // before switching it on.
                    request_matched = inspection.matched_rule_ids();
                    for id in &request_matched {
                        self.metrics.record_waf_match(&method_str, &route_path, *id);
                    }
                    if let waf::WafDecision::Block {
                        status,
                        rule_id,
                        message,
                    } = decision
                    {
                        tracing::warn!(
                            rule_id,
                            status,
                            method = %method_str,
                            path = %route_path,
                            inbound_score = inspection.inbound_score(),
                            message = %message,
                            "WAF blocked request"
                        );
                        self.metrics
                            .record_waf_blocked(&method_str, &route_path, rule_id);
                        self.metrics.record_validation_failure(
                            &method_str,
                            &route_path,
                            "waf_blocked",
                        );
                        // Run phase 5 so a request-phase block is logged and
                        // correlated the same as one that reached the response.
                        inspection.run_logging();
                        self.emit_waf_audit(
                            &inspection,
                            &method_str,
                            &route_path,
                            &request_id,
                            client_ip.map(|ip| ip.to_string()).as_deref(),
                            status,
                            Some(rule_id),
                        );
                        let response = self.waf_blocked_response(status, rule_id, &message);
                        self.record_request_metrics(
                            &method_str,
                            &route_path,
                            response.status().as_u16(),
                            request_size,
                            0,
                            start_time,
                        );
                        return Ok(box_full(Self::add_standard_headers(
                            response,
                            &request_id,
                            &trace_id,
                        )));
                    }
                    // Held across dispatch: response-phase rules continue the
                    // same transaction so outbound blocking rules read the
                    // scores the inbound rules accumulated.
                    waf_inspection = Some(inspection);
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
                    // The WAF allowed this request but spec validation rejected it
                    // before dispatch. Audit the WAF's verdict here too, so a
                    // transaction the WAF inspected is not lost from the audit log
                    // just because a later stage refused it.
                    if let Some(mut inspection) = waf_inspection.take() {
                        inspection.run_logging();
                        self.emit_waf_audit(
                            &inspection,
                            &method_str,
                            &route_path,
                            &request_id,
                            client_ip.map(|ip| ip.to_string()).as_deref(),
                            response.status().as_u16(),
                            None,
                        );
                    }
                    self.record_request_metrics(
                        &method_str,
                        &route_path,
                        response.status().as_u16(),
                        request_size,
                        0,
                        start_time,
                    );
                    return Ok(box_full(Self::add_standard_headers(
                        response,
                        &request_id,
                        &trace_id,
                    )));
                }

                let response: Response<AnyBody> = self
                    .dispatch(
                        operation,
                        &path,
                        params,
                        query_string,
                        &body_bytes,
                        &headers,
                        client_ip,
                        upgrade_handle,
                    )
                    .await?;

                // Add deprecation headers if the operation is deprecated
                let response = Self::add_deprecation_headers(response, operation);

                // Response-phase WAF inspection (phases 3, 4 and 5).
                let response = if let Some(inspection) = waf_inspection {
                    self.inspect_response_waf(
                        inspection,
                        response,
                        &method_str,
                        &route_path,
                        &request_matched,
                        &request_id,
                        client_ip.map(|ip| ip.to_string()).as_deref(),
                    )
                    .await
                } else {
                    response
                };

                // A buffered body's size is known now. A streamed one is
                // measured as it is sent, and the request recorded when it
                // ends: its size and duration cover the whole body.
                let response = match response.body().size_hint().exact() {
                    Some(response_size) => {
                        self.record_request_metrics(
                            &method_str,
                            &route_path,
                            response.status().as_u16(),
                            request_size,
                            response_size,
                            start_time,
                        );
                        response
                    }
                    None => {
                        let metrics = Arc::clone(&self.metrics);
                        let api = self.api_name.clone();
                        let status = response.status().as_u16();
                        let (method, route) = (method_str.clone(), route_path.clone());
                        response.map(|body| {
                            BoxBody::new(MeteredBody::new(body, move |response_size| {
                                metrics.record_request(
                                    &method,
                                    &route,
                                    status,
                                    &api,
                                    start_time.elapsed().as_secs_f64(),
                                    request_size,
                                    response_size,
                                );
                            }))
                        })
                    }
                };
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
                                    UNMATCHED_ROUTE_LABEL,
                                    response.status().as_u16(),
                                    0,
                                    0,
                                    start_time,
                                );
                                return Ok(box_full(response));
                            }
                        }
                    }
                }

                // Not a CORS preflight or no CORS middleware found - return 405
                let response = self.method_not_allowed_response(allowed, &method_str, &path);
                self.record_request_metrics(
                    &method_str,
                    UNMATCHED_ROUTE_LABEL,
                    response.status().as_u16(),
                    0,
                    0,
                    start_time,
                );
                Ok(box_full(Self::add_standard_headers(
                    response,
                    &request_id,
                    &trace_id,
                )))
            }
            RouteMatch::NotFound => {
                let response = self.not_found_response();
                self.record_request_metrics(
                    &method_str,
                    UNMATCHED_ROUTE_LABEL,
                    response.status().as_u16(),
                    0,
                    0,
                    start_time,
                );
                Ok(box_full(Self::add_standard_headers(
                    response,
                    &request_id,
                    &trace_id,
                )))
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
    #[allow(clippy::too_many_arguments)]
    async fn dispatch(
        &self,
        operation: &CompiledOperation,
        request_path: &str,
        params: Vec<(String, String)>,
        query_string: Option<String>,
        request_body: &[u8],
        headers: &HashMap<String, String>,
        client_ip: Option<IpAddr>,
        upgrade_handle: Option<PendingUpgrade>,
    ) -> Result<Response<AnyBody>, Infallible> {
        let dispatch = &operation.dispatch;

        // Build the Request object for plugins (using BTreeMap for WASM compatibility)
        let path_params: std::collections::BTreeMap<String, String> = params.into_iter().collect();
        let (headers_btree, dropped_headers) =
            filter_request_headers(&operation.allowed_request_headers, headers, self.dev_mode);
        self.metrics.record_request_headers_dropped(dropped_headers);
        // Extract body separately — it travels via side-channel, not in JSON.
        let raw_body = if request_body.is_empty() {
            None
        } else {
            Some(request_body.to_vec())
        };

        // The body travels via the side-channel, not in JSON.
        let mut plugin_request =
            barbacane_wasm::Request::new(operation.method.clone(), request_path).with_client_ip(
                client_ip
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "0.0.0.0".to_string()),
            );
        plugin_request.query = query_string;
        plugin_request.headers = headers_btree;
        plugin_request.path_params = path_params;

        let request_json = match serde_json::to_vec(&plugin_request) {
            Ok(j) => j,
            Err(e) => {
                return Ok(box_full(self.dev_error_response(format_args!(
                    "failed to serialize request: {}",
                    e
                ))));
            }
        };

        // Execute middleware on_request chain
        let (final_request_json, final_body, middleware_instances, middleware_context) =
            if !operation.middlewares.is_empty() {
                match self.run_middleware_on_request(
                    &operation.middlewares,
                    &request_json,
                    raw_body,
                ) {
                    Ok((req, body, instances, ctx)) => (req, body, instances, ctx),
                    Err(resp) => return Ok(box_full(resp)), // Short-circuit response
                }
            } else {
                (
                    request_json,
                    raw_body,
                    Vec::new(),
                    barbacane_wasm::RequestContext::default(),
                )
            };

        // All dispatchers must be WASM plugins loaded from the artifact
        if !self.plugin_pool.has_plugin(&dispatch.name) {
            return Ok(box_full(self.dev_error_response(format_args!(
                "unknown dispatcher '{}' - not found in artifact plugins",
                dispatch.name
            ))));
        }

        // Dispatch to the plugin.
        // Returns either a buffered response, streaming response (ADR-0023),
        // or WebSocket upgrade response (ADR-0026).
        let dispatch_outcome = match self
            .dispatch_wasm_plugin_inner(
                &dispatch.name,
                &dispatch.config,
                final_request_json,
                final_body,
                middleware_instances,
                middleware_context,
                upgrade_handle,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(box_full(e)),
        };

        Ok(dispatch_outcome)
    }

    /// Run the on_request chain on this thread as blocking work: WASM and its
    /// host calls (HTTP, LDAP, brokers) block, and the runtime hands the
    /// worker's other tasks to another thread meanwhile.
    #[allow(clippy::result_large_err, clippy::type_complexity)]
    fn run_middleware_on_request(
        &self,
        middlewares: &[barbacane_compiler::MiddlewareConfig],
        request_json: &[u8],
        raw_body: Option<Vec<u8>>,
    ) -> Result<
        (
            Vec<u8>,
            Option<Vec<u8>>,
            Vec<barbacane_wasm::PluginInstance>,
            barbacane_wasm::RequestContext,
        ),
        Response<Full<Bytes>>,
    > {
        tokio::task::block_in_place(|| {
            self.execute_middleware_on_request(middlewares, request_json, raw_body)
        })
    }

    /// Execute middleware on_request chain.
    /// Returns the final request JSON, body, middleware instances, and context
    /// (for on_response), or a short-circuit response.
    #[allow(clippy::result_large_err, clippy::type_complexity)]
    fn execute_middleware_on_request(
        &self,
        middlewares: &[barbacane_compiler::MiddlewareConfig],
        request_json: &[u8],
        raw_body: Option<Vec<u8>>,
    ) -> Result<
        (
            Vec<u8>,
            Option<Vec<u8>>,
            Vec<barbacane_wasm::PluginInstance>,
            barbacane_wasm::RequestContext,
        ),
        Response<Full<Bytes>>,
    > {
        use barbacane_wasm::RequestContext;

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
            return Ok((
                request_json.to_vec(),
                raw_body,
                instances,
                RequestContext::default(),
            ));
        }

        // Build per-middleware body_access flags
        let body_access_flags: Vec<bool> = middlewares
            .iter()
            .map(|mw| self.plugin_pool.body_access(&mw.name))
            .collect();

        // Side-channel body control (SPEC-008): body travels via host functions,
        // not embedded in JSON. Only body-access middleware receives the body.
        let mut body_ctrl = barbacane_wasm::BodyAccessControl::new(request_json.to_vec(), raw_body);
        let mut current_context = RequestContext::default();

        for (index, instance) in instances.iter_mut().enumerate() {
            let has_body_access = body_access_flags[index];
            let request_for_wasm = body_ctrl.prepare_instance(instance, has_body_access);

            instance.set_context(current_context.clone());
            let start = std::time::Instant::now();
            let middleware_name = instance.name().to_string();

            match instance.on_request(&request_for_wasm) {
                Ok(result_code) => {
                    let output = instance.take_output();
                    match barbacane_wasm::parse_middleware_output(&output, result_code) {
                        Ok(barbacane_wasm::OnRequestResult::Continue(new_request)) => {
                            self.metrics.record_middleware(
                                &middleware_name,
                                "request",
                                start.elapsed().as_secs_f64(),
                                false,
                            );
                            current_context = instance.get_context();
                            body_ctrl.collect_after(instance, new_request, has_body_access);
                        }
                        Ok(barbacane_wasm::OnRequestResult::ShortCircuit(response)) => {
                            self.metrics.record_middleware(
                                &middleware_name,
                                "request",
                                start.elapsed().as_secs_f64(),
                                true,
                            );
                            // Always collect short-circuit response body from side-channel.
                            // body_access only controls whether the plugin *receives* the
                            // request body — any middleware can set a response body on
                            // short-circuit (e.g. error responses with JSON problem details).
                            let sc_body = instance.take_output_body().unwrap_or(None);
                            let final_context = instance.get_context();
                            let final_response = match barbacane_wasm::execute_on_response_partial(
                                &mut instances,
                                &response,
                                index,
                                final_context,
                            ) {
                                Ok(r) => r,
                                Err(e) => return Err(self.dev_error_response(e)),
                            };
                            return match serde_json::from_slice::<barbacane_wasm::Response>(
                                &final_response,
                            ) {
                                Ok(mut plugin_response) => {
                                    plugin_response.body = sc_body;
                                    Err(Self::build_response_from_plugin(&plugin_response))
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "failed to parse middleware response");
                                    Err(self.dev_error_response(format_args!(
                                        "failed to parse middleware response: {}",
                                        e
                                    )))
                                }
                            };
                        }
                        Err(e) => {
                            self.metrics.record_middleware(
                                &middleware_name,
                                "request",
                                start.elapsed().as_secs_f64(),
                                false,
                            );
                            tracing::error!(error = %e, "middleware chain execution failed");
                            return Err(self.dev_error_response(format_args!(
                                "middleware chain error: {}",
                                e
                            )));
                        }
                    }
                }
                Err(e) => {
                    self.metrics.record_middleware(
                        &middleware_name,
                        "request",
                        start.elapsed().as_secs_f64(),
                        false,
                    );
                    tracing::error!(error = %e, "middleware chain execution failed");
                    return Err(
                        self.dev_error_response(format_args!("middleware chain error: {}", e))
                    );
                }
            }
        }

        let (metadata, body) = body_ctrl.finalize();
        Ok((metadata, body, instances, current_context))
    }

    /// Execute middleware on_response chain.
    ///
    /// Like the on_request path (SPEC-008), response body is stripped for
    /// middleware that don't declare `body_access = true`. Bodies travel via
    /// side-channel host functions, not embedded in JSON.
    ///
    /// A middleware that fails stops the chain and the request is answered
    /// with a 500 (SPEC-003 section 6.4).
    #[allow(clippy::result_large_err)]
    fn execute_middleware_on_response(
        &self,
        mut instances: Vec<barbacane_wasm::PluginInstance>,
        response: barbacane_wasm::Response,
        context: barbacane_wasm::RequestContext,
    ) -> Result<barbacane_wasm::Response, Response<Full<Bytes>>> {
        // Extract body — it travels via side-channel, not in JSON.
        let mut resp_for_json = response;
        let resp_body = resp_for_json.body.take();

        let response_json = serde_json::to_vec(&resp_for_json).map_err(|e| {
            self.dev_error_response(format_args!("failed to serialize response: {e}"))
        })?;

        // Side-channel body control: body only goes to body-access middleware.
        let mut body_ctrl = barbacane_wasm::BodyAccessControl::new(response_json, resp_body);

        let metrics = &self.metrics;

        // Process in reverse order (on_response runs last-to-first).
        for instance in instances.iter_mut().rev() {
            let has_body_access = self.plugin_pool.body_access(instance.name());
            let response_for_wasm = body_ctrl.prepare_instance(instance, has_body_access);

            instance.set_context(context.clone());
            let start = std::time::Instant::now();
            let middleware_name = instance.name().to_string();

            match instance.on_response(&response_for_wasm) {
                Ok(_result_code) => {
                    metrics.record_middleware(
                        &middleware_name,
                        "response",
                        start.elapsed().as_secs_f64(),
                        false,
                    );
                    let output = instance.take_output();
                    body_ctrl.collect_after(instance, output, has_body_access);
                }
                Err(e) => {
                    metrics.record_middleware(
                        &middleware_name,
                        "response",
                        start.elapsed().as_secs_f64(),
                        false,
                    );
                    return Err(self.dev_error_response(format_args!(
                        "plugin '{middleware_name}' on_response failed: {e}"
                    )));
                }
            }
        }

        // Reconstruct Response from metadata JSON + side-channel body.
        let (final_json, final_body) = body_ctrl.finalize();
        let mut resp =
            serde_json::from_slice::<barbacane_wasm::Response>(&final_json).map_err(|e| {
                self.dev_error_response(format_args!("failed to parse middleware response: {e}"))
            })?;
        resp.body = final_body;
        Ok(resp)
    }

    /// Build an HTTP response from a plugin Response.
    ///
    /// Does not use `self` (associated fn) so it can be unit-tested directly for
    /// the untrusted-header handling below.
    fn build_response_from_plugin(
        plugin_response: &barbacane_wasm::Response,
    ) -> Response<Full<Bytes>> {
        let status = StatusCode::from_u16(plugin_response.status).unwrap_or(StatusCode::OK);
        let mut builder = Response::builder().status(status);

        for (key, value) in &plugin_response.headers {
            // Skip framing headers that the plugin (or its upstream) may have
            // set for a different body. hyper recomputes `content-length` from
            // the actual `Full<Bytes>` payload; keeping a stale value would
            // cause the client to see a truncated response (`IncompleteMessage`)
            // when a middleware (for example `response-transformer`)
            // modifies the body length.
            let key_lc = key.to_ascii_lowercase();
            if matches!(
                key_lc.as_str(),
                "content-length" | "transfer-encoding" | "connection" | "keep-alive"
            ) {
                continue;
            }
            // Validate the plugin-supplied header before insertion. The `http`
            // crate defers validation to `.body()`, so an illegal name/value
            // (empty, whitespace, control byte, or a value with CR/LF) would
            // otherwise surface as a panic there. Plugins are untrusted, so skip
            // invalid headers instead of aborting the connection task (a plugin
            // reflecting an attacker field into a header could otherwise DoS the
            // route). Mirrors add_standard_headers' defensive handling.
            let name = match HeaderName::from_bytes(key.as_bytes()) {
                Ok(name) => name,
                Err(_) => {
                    tracing::debug!(header = %key, "skipping invalid plugin response header name");
                    continue;
                }
            };
            let header_value = match HeaderValue::from_str(value) {
                Ok(v) => v,
                Err(_) => {
                    tracing::debug!(header = %key, "skipping invalid plugin response header value");
                    continue;
                }
            };
            builder = builder.header(name, header_value);
        }

        let body = plugin_response.body.clone().unwrap_or_default();
        match builder.body(Full::new(Bytes::from(body))) {
            Ok(resp) => resp,
            Err(e) => {
                // Header validation above should make this unreachable, but never
                // panic on plugin-derived data: return a clean 502 instead.
                tracing::error!(error = %e, "failed to build response from plugin; returning 502");
                Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Full::new(Bytes::from_static(b"invalid upstream response")))
                    .expect("static 502 response is always valid")
            }
        }
    }

    /// Streamed body chunks in flight between `host_http_stream` and the client.
    const STREAM_CHANNEL_CAPACITY: usize = 16;

    /// Dispatch via a WASM plugin (inner function taking pre-serialized request).
    ///
    /// Handles both buffered and streaming (ADR-0023) dispatch paths:
    /// - **Buffered**: WASM returns a `Response` directly; on_response middleware runs synchronously.
    /// - **Streaming**: WASM calls `host_http_stream`; headers arrive via channel before WASM
    ///   returns, body streams via `StreamBody`. on_response runs in a background task for
    ///   observability only (modifications are discarded since the response is already sent).
    #[allow(clippy::too_many_arguments)]
    // The Err variant here *is* an HTTP response, which hyper sizes at 144
    // bytes. Boxing a response to satisfy a size lint would add an allocation
    // to the error path for no benefit, and the type is not ours to shrink.
    #[allow(clippy::result_large_err)]
    async fn dispatch_wasm_plugin_inner(
        &self,
        plugin_name: &str,
        config: &serde_json::Value,
        request_json: Vec<u8>,
        request_body: Option<Vec<u8>>,
        middleware_instances: Vec<barbacane_wasm::PluginInstance>,
        middleware_context: barbacane_wasm::RequestContext,
        upgrade_handle: Option<PendingUpgrade>,
    ) -> Result<Response<AnyBody>, Response<Full<Bytes>>> {
        use barbacane_wasm::StreamEvent;
        use futures_util::stream;

        // The (plugin, config) pair was registered when the artifact loaded.
        let instance_key = barbacane_wasm::InstanceKey::new(plugin_name, config);

        // Get a plugin instance
        let mut instance = match self.plugin_pool.get_instance(&instance_key) {
            Ok(i) => i,
            Err(e) => {
                return Err(
                    self.dev_error_response(format_args!("failed to get plugin instance: {}", e))
                );
            }
        };

        // Pre-compute Sec-WebSocket-Accept for WebSocket upgrades (RFC 6455 §4.2.2).
        // Extract the key from request_json before it's moved into the dispatch closure.
        let ws_accept = serde_json::from_slice::<serde_json::Value>(&request_json)
            .ok()
            .and_then(|v| v["headers"]["sec-websocket-key"].as_str().map(String::from))
            .map(|key| {
                tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes())
            });

        // Set up streaming channel (ADR-0023). The sender is injected into the instance so that
        // `host_http_stream` can push `StreamEvent::Headers` then `StreamEvent::Chunk` events
        // before `dispatch()` returns. The channel is bounded: when the client reads slower
        // than the upstream sends, it fills and the upstream read waits, so a streamed
        // response holds at most `STREAM_CHANNEL_CAPACITY` chunks in host memory.
        let (stream_tx, mut stream_rx) =
            tokio::sync::mpsc::channel::<StreamEvent>(Self::STREAM_CHANNEL_CAPACITY);
        instance.set_stream_sender(Arc::new(stream_tx));

        // Inject request body via side-channel before dispatch.
        instance.set_request_body(request_body);

        // Carry the middleware chain's accumulated context into the
        // dispatcher so it can read keys written upstream (e.g. `ai.target`
        // set by a `cel` routing instance). The dispatcher may also write
        // new keys (e.g. `ai.prompt_tokens`); we capture those below and
        // thread them through to `on_response`.
        instance.set_context(middleware_context.clone());

        // Run WASM dispatch on a blocking thread (WASM execution is synchronous),
        // inside the request's span so host calls can record on it.
        let span = tracing::Span::current();
        let mut wasm_handle = tokio::task::spawn_blocking(move || {
            let _entered = span.enter();
            let result = instance.dispatch(&request_json);
            let output = instance.take_output();
            let output_body = instance.take_output_body();
            let last_http = instance.take_last_http_result();
            let ws_upgrade_request = instance.take_ws_upgrade_request();
            let post_dispatch_context = instance.get_context();
            (
                result,
                output,
                output_body,
                last_http,
                ws_upgrade_request,
                post_dispatch_context,
            )
        });

        // Race: first stream event vs. WASM completion.
        // In the streaming path `host_http_stream` sends `StreamEvent::Headers` before
        // `dispatch()` returns, so `stream_rx.recv()` always wins the race in that case.
        let mut maybe_wasm_result = None;
        let first_event: Option<StreamEvent>;
        tokio::select! {
            biased;
            e = stream_rx.recv() => first_event = e,
            r = &mut wasm_handle => {
                maybe_wasm_result = Some(r);
                first_event = None;
            }
        }

        match first_event {
            // ── Streaming path ──────────────────────────────────────────────────────────
            Some(StreamEvent::Headers { status, headers }) => {
                let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
                let mut builder = Response::builder().status(status_code);
                for (k, v) in &headers {
                    // Same defensive validation as build_response_from_plugin:
                    // skip framing headers and any name/value the `http` crate
                    // would reject (which would otherwise panic at `.body()`).
                    let k_lc = k.to_ascii_lowercase();
                    if matches!(
                        k_lc.as_str(),
                        "content-length" | "transfer-encoding" | "connection" | "keep-alive"
                    ) {
                        continue;
                    }
                    let name = match HeaderName::from_bytes(k.as_bytes()) {
                        Ok(name) => name,
                        Err(_) => {
                            tracing::debug!(header = %k, "skipping invalid streamed response header name");
                            continue;
                        }
                    };
                    let value = match HeaderValue::from_str(v) {
                        Ok(value) => value,
                        Err(_) => {
                            tracing::debug!(header = %k, "skipping invalid streamed response header value");
                            continue;
                        }
                    };
                    builder = builder.header(name, value);
                }

                // Convert remaining Chunk events into HTTP body frames.
                let chunk_stream = stream::unfold(stream_rx, |mut rx| async move {
                    loop {
                        match rx.recv().await {
                            Some(StreamEvent::Chunk(bytes)) => {
                                return Some((
                                    Ok::<Frame<Bytes>, Infallible>(Frame::data(bytes)),
                                    rx,
                                ));
                            }
                            // Skip unexpected extra Headers events.
                            Some(StreamEvent::Headers { .. }) => continue,
                            // Sender dropped — stream complete.
                            None => return None,
                        }
                    }
                });

                let response = match builder.body(BoxBody::new(StreamBody::new(chunk_stream))) {
                    Ok(r) => r,
                    Err(e) => {
                        // Validated headers make this unreachable, but never panic
                        // on plugin-derived data: return a clean 502 instead.
                        tracing::error!(error = %e, "failed to build streaming response; returning 502");
                        Response::builder()
                            .status(StatusCode::BAD_GATEWAY)
                            .body(BoxBody::new(Full::new(Bytes::from_static(
                                b"invalid upstream response",
                            ))))
                            .expect("static 502 response is always valid")
                    }
                };

                // Background task: wait for WASM to finish, then run on_response for
                // observability. Modifications are discarded (response already sent).
                // Strip the response body to avoid WASM OOM with large streamed payloads.
                let wh = wasm_handle;
                let metrics = Arc::clone(&self.metrics);
                background().spawn(async move {
                    match wh.await {
                        Ok((Ok(_), _, _, Some(last_http), _, post_ctx))
                            if !middleware_instances.is_empty() =>
                        {
                            if let Ok(plugin_resp) =
                                serde_json::from_slice::<barbacane_wasm::Response>(&last_http)
                            {
                                // Strip body: observability-only, modifications discarded.
                                // Body is already None (side-channel), just pass metadata.
                                let mut instances = middleware_instances;
                                let resp_json =
                                    serde_json::to_vec(&plugin_resp).unwrap_or_default();
                                let cb = |name: &str, phase: &str, dur: f64, sc: bool| {
                                    metrics.record_middleware(name, phase, dur, sc);
                                };
                                if let Err(e) = barbacane_wasm::execute_on_response_with_metrics(
                                    &mut instances,
                                    &resp_json,
                                    post_ctx,
                                    Some(&cb),
                                ) {
                                    tracing::error!(
                                        error = %e,
                                        "middleware failed after a streamed response was sent"
                                    );
                                }
                            }
                        }
                        Ok((Err(e), _, _, _, _, _)) => {
                            tracing::warn!(
                                error = %e,
                                "streaming dispatch error (response already sent)"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "streaming WASM task panicked (response already sent)"
                            );
                        }
                        _ => {}
                    }
                });

                Ok(response)
            }

            // ── Buffered path ────────────────────────────────────────────────────────────
            _ => {
                // Retrieve the WASM result. In the normal buffered case `maybe_wasm_result` is
                // already set. The fallback await handles the rare case where recv() returned
                // `None` (channel closed) and won the race just as WASM completed.
                let wasm_result = match maybe_wasm_result {
                    Some(r) => r,
                    None => wasm_handle.await,
                };

                let (
                    dispatch_result,
                    output,
                    output_body,
                    _,
                    ws_upgrade_request,
                    post_dispatch_context,
                ) = match wasm_result {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(
                            self.dev_error_response(format_args!("plugin task panicked: {}", e))
                        );
                    }
                };

                if let Err(e) = dispatch_result {
                    return Err(self.dev_error_response(format_args!(
                        "plugin '{plugin_name}' dispatch failed: {e}"
                    )));
                }

                if output.is_empty() {
                    return Err(self.dev_error_response("plugin returned empty output"));
                }

                let mut plugin_response: barbacane_wasm::Response =
                    match serde_json::from_slice(&output) {
                        Ok(r) => r,
                        Err(e) => {
                            return Err(self.dev_error_response(format_args!(
                                "failed to parse plugin response: {}",
                                e
                            )));
                        }
                    };

                // Inject body from side-channel (output_body from host_body_set/clear).
                if let Some(body) = output_body {
                    plugin_response.body = body;
                }

                // status=0 is the streamed_response() sentinel. If WASM returned it without
                // sending stream events, that is a plugin bug.
                if plugin_response.status == 0 {
                    return Err(self.dev_error_response(
                        "plugin returned streaming sentinel without stream events",
                    ));
                }

                // ── WebSocket upgrade path (ADR-0026) ──────────────────────────────────
                // status=101 with a ws_upgrade_request means the dispatcher called
                // host_ws_upgrade successfully. Connect to the upstream on the main
                // runtime (so the TcpStream's I/O driver stays alive), complete the
                // client-side upgrade, and spawn bidirectional frame relay.
                if plugin_response.status == 101 {
                    let ws_request = match ws_upgrade_request {
                        Some(req) => req,
                        None => {
                            return Err(self.dev_error_response(
                                "plugin returned 101 without calling host_ws_upgrade",
                            ));
                        }
                    };

                    let upgrade_handle = match upgrade_handle {
                        Some(h) => h,
                        None => {
                            return Err(self.dev_error_response(
                                "plugin returned 101 but request is not an HTTP upgrade",
                            ));
                        }
                    };

                    // Connect to the upstream WebSocket on the main runtime.
                    // This MUST happen here (not in a temporary runtime) so the
                    // TcpStream is registered with the I/O driver that will drive
                    // the relay task.
                    let ws_upstream = match barbacane_wasm::ws_client::connect_upstream(
                        ws_request,
                        self.allow_internal_egress,
                        self.allow_plaintext_upstream,
                    )
                    .await
                    {
                        Ok(stream) => stream,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "WebSocket upstream connection failed"
                            );
                            return Err(self.upstream_unavailable_response(format_args!(
                                "WebSocket upstream connection failed: {}",
                                err
                            )));
                        }
                    };

                    // Run on_response for observability (modifications
                    // discarded). A middleware that fails refuses the upgrade.
                    if !middleware_instances.is_empty() {
                        let sentinel_response = barbacane_wasm::Response::new(101);
                        tokio::task::block_in_place(|| {
                            self.execute_middleware_on_response(
                                middleware_instances,
                                sentinel_response,
                                post_dispatch_context.clone(),
                            )
                        })?;
                    }

                    // Spawn the WebSocket relay as a background task, which
                    // the drain waits for. The client-side upgrade completes
                    // when we return the 101 response.
                    let shutdown = background().shutdown();
                    let PendingUpgrade { on_upgrade, permit } = upgrade_handle;
                    background().spawn(async move {
                        // The relay counts against --max-connections.
                        let _permit = permit;
                        // Wait for hyper to complete the HTTP upgrade.
                        let upgraded = match on_upgrade.await {
                            Ok(u) => u,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "WebSocket client upgrade failed"
                                );
                                return;
                            }
                        };

                        // Wrap the upgraded client connection as a WebSocket.
                        let client_ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                            TokioIo::new(upgraded),
                            tokio_tungstenite::tungstenite::protocol::Role::Server,
                            None,
                        )
                        .await;

                        // Relay frames bidirectionally.
                        Self::relay_websocket(
                            client_ws,
                            ws_upstream,
                            shutdown,
                            WEBSOCKET_IDLE_TIMEOUT,
                        )
                        .await;
                    });

                    // Return 101 Switching Protocols to the client.
                    // hyper will handle the actual protocol switch.
                    let mut builder = Response::builder()
                        .status(StatusCode::SWITCHING_PROTOCOLS)
                        .header("upgrade", "websocket")
                        .header("connection", "Upgrade");
                    if let Some(accept) = &ws_accept {
                        builder = builder.header("sec-websocket-accept", accept.as_str());
                    }
                    let response = builder
                        .body(BoxBody::new(Full::new(Bytes::new())))
                        .expect("valid 101 response");

                    return Ok(response);
                }

                // Run on_response middleware chain with the post-dispatch
                // context so middlewares can observe keys written by the
                // dispatcher.
                let final_response = if !middleware_instances.is_empty() {
                    tokio::task::block_in_place(|| {
                        self.execute_middleware_on_response(
                            middleware_instances,
                            plugin_response,
                            post_dispatch_context,
                        )
                    })?
                } else {
                    plugin_response
                };

                Ok(box_full(Self::build_response_from_plugin(&final_response)))
            }
        }
    }

    /// Handle reserved /__barbacane/* endpoints.
    ///
    /// These endpoints always allow cross-origin requests (`Access-Control-Allow-Origin: *`)
    /// so that Swagger UI, Redoc, or similar tools hosted on different domains can fetch specs.
    fn handle_barbacane_endpoint(
        &self,
        path: &str,
        method: &Method,
        query: Option<&str>,
    ) -> Response<Full<Bytes>> {
        // Handle CORS preflight for internal endpoints
        if method == Method::OPTIONS {
            return Response::builder()
                .status(StatusCode::NO_CONTENT)
                .header("access-control-allow-origin", "*")
                .header("access-control-allow-methods", "GET, OPTIONS")
                .header("access-control-allow-headers", "content-type, accept")
                .header("access-control-max-age", "86400")
                .body(Full::new(Bytes::new()))
                .expect("valid response");
        }

        if method != Method::GET {
            return self.method_not_allowed_response(
                vec!["GET".to_string()],
                method.as_str(),
                path,
            );
        }

        // Parse format from query string (default: yaml for specs, json for index)
        let format = query
            .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("format=")))
            .unwrap_or("yaml");

        let mut response = match path {
            "/__barbacane/health" => self.health_response(),
            _ if !INTROSPECTION_ENABLED.load(std::sync::atomic::Ordering::Relaxed) => {
                self.not_found_response()
            }
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
        };

        response
            .headers_mut()
            .insert("access-control-allow-origin", HeaderValue::from_static("*"));

        response
    }

    /// The specs index (always JSON).
    fn specs_index_response(&self) -> Response<Full<Bytes>> {
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(self.introspection.index.clone()))
            .expect("valid response")
    }

    /// Serve the merged OpenAPI spec.
    fn merged_openapi_response(&self, format: &str) -> Response<Full<Bytes>> {
        match &self.introspection.merged_openapi {
            Some(merged) => self.serve_spec_content(merged, format),
            None => self.not_found_response(),
        }
    }

    /// Serve the merged AsyncAPI spec.
    fn merged_asyncapi_response(&self, format: &str) -> Response<Full<Bytes>> {
        match &self.introspection.merged_asyncapi {
            Some(merged) => self.serve_spec_content(merged, format),
            None => self.not_found_response(),
        }
    }

    /// Serve spec content in requested format.
    fn serve_spec_content(&self, spec: &ServedSpec, format: &str) -> Response<Full<Bytes>> {
        let (content, content_type) = if format == "json" {
            (spec.json.clone(), "application/json")
        } else {
            (spec.yaml.clone(), "text/yaml")
        };

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", content_type)
            .body(Full::new(content))
            .expect("valid response")
    }

    /// Serve a specific spec file, stripped of `x-barbacane-*` extensions. A
    /// spec that does not parse is not served: its source carries the gateway
    /// configuration.
    fn spec_file_response(&self, filename: &str, format: &str) -> Response<Full<Bytes>> {
        match self.introspection.files.get(filename) {
            None => self.not_found_response(),
            Some(Some(spec)) => self.serve_spec_content(spec, format),
            Some(None) => self.internal_error_response(Some("the spec cannot be parsed")),
        }
    }

    /// Build the health response.
    /// The public liveness answer. It says the gateway is up and nothing
    /// else; the artifact's version, compiler and route count are on the
    /// admin API's `/health`, which is not public.
    fn health_response(&self) -> Response<Full<Bytes>> {
        if admin::DRAINING.load(Ordering::Relaxed) {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header("content-type", "application/json")
                .header("connection", "close")
                .body(Full::new(Bytes::from_static(br#"{"status":"draining"}"#)))
                .expect("valid response");
        }
        let body = serde_json::json!({ "status": "healthy" });

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Build a 404 Not Found response (RFC 9457).
    fn not_found_response(&self) -> Response<Full<Bytes>> {
        let body = serde_json::json!({
            "type": "urn:barbacane:error:not-found",
            "title": "Not Found",
            "status": 404,
        });

        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Build a 405 Method Not Allowed response (RFC 9457).
    fn method_not_allowed_response(
        &self,
        allowed: Vec<String>,
        method: &str,
        path: &str,
    ) -> Response<Full<Bytes>> {
        let allow_header = allowed.join(", ");
        let body = serde_json::json!({
            "type": "urn:barbacane:error:method-not-allowed",
            "title": "Method Not Allowed",
            "status": 405,
            "detail": format!("{method} is not allowed on {path}. Allowed methods: {allow_header}"),
        });

        Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header("content-type", "application/problem+json")
            .header("allow", allow_header)
            .body(Full::new(Bytes::from(body.to_string())))
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
        // Build a minimal request for the CORS middleware. A preflight is not
        // bound to one operation, and everything it carries (`origin` and the
        // `access-control-request-*` pair) is in the baseline, so nothing
        // operation-specific is admitted here.
        let (headers_btree, dropped_headers) = filter_request_headers(&[], headers, self.dev_mode);
        self.metrics.record_request_headers_dropped(dropped_headers);

        let mut plugin_request =
            barbacane_wasm::Request::new("OPTIONS", "").with_client_ip("0.0.0.0");
        plugin_request.headers = headers_btree;

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
        match self.run_middleware_on_request(&middlewares, &request_json, None) {
            Ok((_, _, _, _)) => {
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

    /// Build a 403 response for a request the WAF blocked (RFC 9457).
    ///
    /// The rule id and message appear only in dev mode. In production they
    /// tell an attacker exactly which rule to shape the next payload around,
    /// so they go to the log instead.
    fn waf_blocked_response(
        &self,
        status: u16,
        rule_id: u32,
        message: &str,
    ) -> Response<Full<Bytes>> {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
        // Derive the title from the resolved status: a rule can specify
        // `status:406`, and answering 406 with a title of "Forbidden" is a
        // contradiction in an RFC 9457 body.
        let mut body = serde_json::json!({
            "type": "urn:barbacane:error:waf-blocked",
            "title": status.canonical_reason().unwrap_or("Blocked"),
            "status": status.as_u16(),
            "detail": "The request was blocked by the web application firewall.",
        });
        if self.dev_mode {
            body["rule_id"] = serde_json::json!(rule_id);
            if !message.is_empty() {
                body["rule_message"] = serde_json::json!(message);
            }
        }
        Response::builder()
            .status(status)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Inspect a dispatched response through WAF phases 3, 4 and 5, continuing
    /// the transaction the request phase started.
    ///
    /// A buffered body at or under the configured cap is collected and inspected
    /// (phase 4); a streamed or oversized body has only its headers inspected
    /// (phase 3) and the skip counted, because it has already reached the client
    /// by the time a phase-4 rule could block. A WebSocket upgrade has no
    /// response phases. Returns the response to send: the upstream response
    /// (rebuilt when its body was collected), or a block response.
    #[allow(clippy::too_many_arguments)]
    async fn inspect_response_waf(
        &self,
        mut inspection: waf::WafInspection<'_>,
        response: Response<AnyBody>,
        method: &str,
        path: &str,
        request_matched: &[u32],
        request_id: &str,
        client_ip: Option<&str>,
    ) -> Response<AnyBody> {
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            // No response phases; audit the request-phase result.
            self.emit_waf_audit(&inspection, method, path, request_id, client_ip, 101, None);
            return response;
        }

        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(n, v)| {
                (
                    n.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();

        // A buffered body reports an exact upper size hint; a streamed one
        // reports none. Only a buffered body within the cap is collected.
        let cap = self.waf.as_ref().map_or(0, |s| s.response_body_cap());
        let within_cap = response
            .body()
            .size_hint()
            .upper()
            .is_some_and(|n| n as usize <= cap);

        let (decision, response) = if within_cap {
            let (parts, body) = response.into_parts();
            let bytes = body
                .collect()
                .await
                .expect("AnyBody error type is Infallible")
                .to_bytes();
            let decision = inspection.inspect_response(status, &headers, Some(&bytes));
            let rebuilt = Response::from_parts(parts, BoxBody::new(Full::new(bytes)));
            (decision, rebuilt)
        } else {
            self.metrics.record_waf_response_body_skipped(method, path);
            let decision = inspection.inspect_response(status, &headers, None);
            (decision, response)
        };

        // Count only the rules the response phase newly matched; the request
        // phase already counted its own.
        for id in inspection.matched_rule_ids() {
            if !request_matched.contains(&id) {
                self.metrics.record_waf_match(method, path, id);
            }
        }

        let (audit_status, blocked_rule_id) = match &decision {
            waf::WafDecision::Block {
                status, rule_id, ..
            } => (*status, Some(*rule_id)),
            waf::WafDecision::Allow => (status, None),
        };
        self.emit_waf_audit(
            &inspection,
            method,
            path,
            request_id,
            client_ip,
            audit_status,
            blocked_rule_id,
        );

        if let waf::WafDecision::Block {
            status,
            rule_id,
            message,
        } = decision
        {
            tracing::warn!(
                rule_id,
                status,
                method = %method,
                path = %path,
                message = %message,
                "WAF blocked response"
            );
            self.metrics.record_waf_blocked(method, path, rule_id);
            self.metrics
                .record_validation_failure(method, path, "waf_blocked");
            return box_full(self.waf_blocked_response(status, rule_id, &message));
        }

        response
    }

    /// Write a per-transaction WAF audit record, governed by the audit engine
    /// policy. The record is a structured line on the `waf.audit` tracing
    /// target so it can be routed to its own sink.
    #[allow(clippy::too_many_arguments)]
    fn emit_waf_audit(
        &self,
        inspection: &waf::WafInspection,
        method: &str,
        path: &str,
        request_id: &str,
        client_ip: Option<&str>,
        response_status: u16,
        blocked_rule_id: Option<u32>,
    ) {
        let Some(stage) = &self.waf else {
            return;
        };
        let matches = inspection.audit_matches();
        if !waf::should_audit(
            stage.audit_engine(),
            blocked_rule_id.is_some(),
            !matches.is_empty(),
        ) {
            return;
        }
        self.metrics.record_waf_audit(method, path);
        let record = serde_json::json!({
            "request_id": request_id,
            "client_ip": client_ip,
            "method": method,
            "path": path,
            "response_status": response_status,
            "blocked": blocked_rule_id.is_some(),
            "blocking_rule_id": blocked_rule_id,
            "inbound_score": inspection.inbound_score(),
            "outbound_score": inspection.outbound_score(),
            "matches": matches,
        });
        tracing::info!(target: "waf.audit", audit = %record, "WAF audit record");
    }

    /// Build a 413 Payload Too Large response (RFC 9457). Used when the request
    /// body exceeds the configured size limit, whether detected via
    /// `Content-Length` or while streaming a chunked body.
    fn payload_too_large_response(&self, limit: usize) -> Response<Full<Bytes>> {
        let body = serde_json::json!({
            "type": "urn:barbacane:error:payload-too-large",
            "title": "Payload Too Large",
            "status": 413,
            "detail": format!("Request body exceeds the maximum allowed size of {limit} bytes."),
        });

        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// Build a 408 Request Timeout response (RFC 9457): the body did not
    /// arrive within `deadline`. The connection is closed, since the rest of
    /// the body is not read.
    fn request_timeout_response(&self, deadline: Duration) -> Response<Full<Bytes>> {
        let body = serde_json::json!({
            "type": "urn:barbacane:error:request-timeout",
            "title": "Request Timeout",
            "status": 408,
            "detail": format!("The request body did not arrive within {} seconds.", deadline.as_secs()),
        });

        Response::builder()
            .status(StatusCode::REQUEST_TIMEOUT)
            .header("content-type", "application/problem+json")
            .header("connection", "close")
            .body(Full::new(Bytes::from(body.to_string())))
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

    /// A 502 for an upstream the gateway could not reach, as the dispatchers
    /// answer it. The cause reaches the caller only in dev mode.
    fn upstream_unavailable_response(&self, msg: impl std::fmt::Display) -> Response<Full<Bytes>> {
        let mut body = serde_json::json!({
            "type": "urn:barbacane:error:upstream-unavailable",
            "title": "Bad Gateway",
            "status": 502,
        });
        if self.dev_mode {
            body["detail"] = serde_json::Value::String(msg.to_string());
        }
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("valid response")
    }

    /// A 500 for a failure inside the gateway. The cause is always logged; it
    /// reaches the caller only in dev mode.
    fn dev_error_response(&self, msg: impl std::fmt::Display) -> Response<Full<Bytes>> {
        let msg = msg.to_string();
        tracing::error!(error = %msg, "request failed inside the gateway");
        let detail = self.dev_mode.then_some(msg);
        self.internal_error_response(detail.as_deref())
    }

    /// Relay WebSocket frames bidirectionally between client and upstream (ADR-0026).
    ///
    /// Runs until either side closes, an error occurs, or no frame (control
    /// frames included) crosses in either direction for `idle`; then both peers
    /// are sent a Close 1001, as at shutdown. Frames are forwarded as-is with
    /// no inspection or transformation.
    async fn relay_websocket<C, U>(
        client_ws: C,
        upstream_ws: U,
        shutdown: tokio_util::sync::CancellationToken,
        idle: Duration,
    ) where
        C: futures_util::Sink<
                tokio_tungstenite::tungstenite::Message,
                Error = tokio_tungstenite::tungstenite::Error,
            > + futures_util::Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Send
            + 'static,
        U: futures_util::Sink<
                tokio_tungstenite::tungstenite::Message,
                Error = tokio_tungstenite::tungstenite::Error,
            > + futures_util::Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Send
            + 'static,
    {
        use futures_util::{SinkExt, StreamExt};

        let (mut client_tx, mut client_rx) = client_ws.split();
        let (mut upstream_tx, mut upstream_rx) = upstream_ws.split();

        // Shutdown or an idle relay stops both directions.
        let stop = shutdown.child_token();
        let started = tokio::time::Instant::now();
        let last_frame = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mark = |last: &std::sync::atomic::AtomicU64| {
            last.store(
                started.elapsed().as_millis() as u64,
                std::sync::atomic::Ordering::Relaxed,
            )
        };
        let watchdog = {
            let stop = stop.clone();
            let last_frame = Arc::clone(&last_frame);
            async move {
                loop {
                    let last = Duration::from_millis(
                        last_frame.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    let quiet_until = started + last + idle;
                    if tokio::time::Instant::now() >= quiet_until {
                        tracing::debug!("WebSocket relay idle; closing");
                        stop.cancel();
                        return;
                    }
                    tokio::select! {
                        () = stop.cancelled() => return,
                        () = tokio::time::sleep_until(quiet_until) => {}
                    }
                }
            }
        };
        let shutdown = stop;
        let end_watchdog = shutdown.clone();

        // Each direction forwards frames until its peer closes, and at
        // shutdown sends its sink a Close 1001 (going away). Each returns
        // whether shutdown ended it.
        let upstream_shutdown = shutdown.clone();
        let client_last = Arc::clone(&last_frame);
        let client_to_upstream = async move {
            loop {
                tokio::select! {
                    biased;
                    () = upstream_shutdown.cancelled() => {
                        let _ = upstream_tx.send(going_away()).await;
                        let _ = upstream_tx.close().await;
                        return true;
                    }
                    msg = client_rx.next() => match msg {
                        Some(Ok(m)) if m.is_close() => {
                            let _ = upstream_tx.close().await;
                            return false;
                        }
                        Some(Ok(m)) => {
                            mark(&client_last);
                            if upstream_tx.send(m).await.is_err() {
                                return false;
                            }
                        }
                        _ => return false,
                    },
                }
            }
        };

        let upstream_to_client = async move {
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => {
                        let _ = client_tx.send(going_away()).await;
                        let _ = client_tx.close().await;
                        return true;
                    }
                    msg = upstream_rx.next() => match msg {
                        Some(Ok(m)) if m.is_close() => {
                            let _ = client_tx.close().await;
                            return false;
                        }
                        Some(Ok(m)) => {
                            mark(&last_frame);
                            if client_tx.send(m).await.is_err() {
                                return false;
                            }
                        }
                        _ => return false,
                    },
                }
            }
        };

        // When a peer ends its direction, the other is dropped. When shutdown
        // ends one, the other has seen it too and sends its own Close.
        tokio::spawn(watchdog);
        let mut client_to_upstream = std::pin::pin!(client_to_upstream);
        let mut upstream_to_client = std::pin::pin!(upstream_to_client);
        let (by_shutdown, client_side_done) = tokio::select! {
            by_shutdown = &mut client_to_upstream => (by_shutdown, true),
            by_shutdown = &mut upstream_to_client => (by_shutdown, false),
        };
        if by_shutdown {
            let rest = async {
                if client_side_done {
                    (&mut upstream_to_client).await;
                } else {
                    (&mut client_to_upstream).await;
                }
            };
            let _ = tokio::time::timeout(RELAY_CLOSE_GRACE, rest).await;
        }
        end_watchdog.cancel();

        tracing::debug!("WebSocket relay closed");
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

                // Entries that disable a global middleware (E1012, E1013, E1014)
                for err in barbacane_compiler::check_disabled_middlewares(&spec) {
                    let text = err.to_string();
                    let (code, message) = text.split_once(": ").unwrap_or(("E1000", &text));
                    errors.push(ValidationIssue {
                        code: code.to_string(),
                        message: message.to_string(),
                        location: Some(spec_path.clone()),
                    });
                    has_errors = true;
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
                    barbacane_compiler::ParseError::InvalidMiddleware(_) => {
                        ("E1011".to_string(), e.to_string())
                    }
                    barbacane_compiler::ParseError::MisplacedExtension(_) => {
                        ("E1017".to_string(), e.to_string())
                    }
                    barbacane_compiler::ParseError::InvalidSunset(_) => {
                        ("E1018".to_string(), e.to_string())
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

    // Phase 2: Check for routing conflicts (E1010): operations the router
    // would file as one route, in one spec or across specs.
    for (spec_path, spec) in &parsed_specs {
        for op in &spec.operations {
            for served in barbacane_compiler::artifact::served_paths(&op.base_paths, &op.path) {
                let key = barbacane_compiler::artifact::route_key(&served, &op.method);
                let Some(other) = seen_routes.get(&key) else {
                    seen_routes.insert(key, format!("{} {served} in '{spec_path}'", op.method));
                    continue;
                };
                if let Some(result) = results.iter_mut().find(|r| &r.file == spec_path) {
                    result.errors.push(ValidationIssue {
                        code: "E1010".to_string(),
                        message: format!(
                            "routing conflict: {} {served} and {other} are both the route {} {}",
                            op.method, key.0, key.1
                        ),
                        location: Some(format!("{}:{} {}", spec_path, op.path, op.method)),
                    });
                    result.valid = false;
                    has_errors = true;
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

/// Where the official plugins of this release are published: the GitHub
/// release of the CLI's own version, so a release candidate gets its own
/// plugins and not those of the last stable release.
const PLUGIN_RELEASE_BASE: &str = concat!(
    "https://github.com/barbacane-dev/barbacane/releases/download/v",
    env!("CARGO_PKG_VERSION")
);

/// The download URL of an official plugin's `filename`.
fn plugin_url(filename: &str) -> String {
    format!("{PLUGIN_RELEASE_BASE}/{filename}")
}

/// Download a plugin from GitHub releases.
async fn download_plugin(name: &str, filename: &str, dest_dir: &Path) -> Result<(), String> {
    let url = plugin_url(filename);
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

    // Create plugins and specs directories
    let plugins_dir = project_dir.join("plugins");
    if let Err(e) = fs::create_dir_all(&plugins_dir) {
        eprintln!("error: failed to create plugins directory: {}", e);
        return ExitCode::from(1);
    }

    let specs_dir = project_dir.join("specs");
    if let Err(e) = fs::create_dir_all(&specs_dir) {
        eprintln!("error: failed to create specs directory: {}", e);
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

specs: ./specs/

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
             specs: ./specs/\n\n\
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
          body: '{"id": "{{path_params.id}}", "name": "John Doe"}'
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

    if let Err(e) = fs::write(specs_dir.join("api.yaml"), spec_content) {
        eprintln!("error: failed to create specs/api.yaml: {}", e);
        return ExitCode::from(1);
    }

    // Create .gitignore
    let gitignore_content = r#"# Build artifacts
*.bca
target/

# Artifact signing keys
*.pk8

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
    eprintln!("  barbacane.yaml    - project manifest");
    eprintln!(
        "  specs/api.yaml    - OpenAPI specification ({} template)",
        template
    );
    if !downloaded_plugins.is_empty() {
        for (plugin_name, filename) in &downloaded_plugins {
            eprintln!("  plugins/{}  - {} plugin", filename, plugin_name);
        }
    } else {
        eprintln!("  plugins/          - directory for WASM plugins");
    }
    eprintln!("  .gitignore        - Git ignore file");
    eprintln!();
    eprintln!("Next steps:");
    if downloaded_plugins.is_empty() && !fetch_plugins {
        eprintln!("  1. Download plugins: barbacane init . --fetch-plugins");
        eprintln!("     Or add them manually to plugins/");
        eprintln!("  2. Edit specs/api.yaml to define your API");
        eprintln!("  3. Run: barbacane dev");
    } else {
        eprintln!("  1. Edit specs/api.yaml to define your API");
        eprintln!("  2. Run: barbacane dev");
    }

    ExitCode::SUCCESS
}

// =============================================================================
// Hot-Reload Functions
// =============================================================================

/// Perform hot-reload: download, verify, load, and swap the gateway state.
#[allow(clippy::too_many_arguments)]
async fn perform_hot_reload(
    notification: control_plane::ArtifactNotification,
    download_token: Option<String>,
    shared_gateway: &SharedGateway,
    artifact_dir: &Path,
    dev_mode: bool,
    limits: RequestLimits,
    allow_plaintext_upstream: bool,
    metrics: Arc<MetricsRegistry>,
    trust: ArtifactTrust,
) -> HotReloadResult {
    let artifact_id = notification.artifact_id;

    // One reload at a time; a deployment that arrives during a reload waits
    // for it and is applied next.
    let _guard = hot_reload::HOT_RELOAD_LOCK.lock().await;

    tracing::info!(
        artifact_id = %artifact_id,
        download_url = %notification.download_url,
        "Starting hot-reload"
    );

    // Step 1: Download and verify artifact
    let download = match hot_reload::download_client() {
        Ok(client) => {
            hot_reload::download_artifact(
                &client,
                &notification.download_url,
                download_token.as_deref(),
                &notification.sha256,
                artifact_dir,
                barbacane_compiler::MAX_ARTIFACT_BYTES,
            )
            .await
        }
        Err(e) => Err(e),
    };
    let artifact_path = match download {
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

    // Step 2: Load and compile new Gateway. Compiling every plugin takes
    // seconds; the runtime hands this worker's other tasks elsewhere.
    let balancer = Arc::clone(&shared_gateway.load().balancer);
    let new_gateway = match tokio::task::block_in_place(|| {
        Gateway::load(
            &artifact_path,
            dev_mode,
            limits,
            allow_plaintext_upstream,
            metrics,
            &trust,
            Some(balancer),
            shared_gateway.load().plugin_instance_pool,
        )
    }) {
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

    retire(old_gateway);

    // The gateway runs the new artifact: it becomes the one a restart loads.
    if let Err(e) = hot_reload::promote(artifact_dir, artifact_id).await {
        tracing::warn!(artifact_id = %artifact_id, error = %e, "the deployed artifact could not be kept for a restart");
    }

    HotReloadResult::Success { artifact_id }
}

/// The project's deployment, fetched from the control plane with the data
/// plane's key and kept in `artifact_dir` as the current artifact: how a data
/// plane started without an artifact gets its first one. Returns its path and
/// id.
async fn bootstrap_deployment(
    config: &ConnectedModeConfig,
    artifact_dir: &Path,
) -> Result<(std::path::PathBuf, uuid::Uuid), String> {
    #[derive(serde::Deserialize)]
    struct Deployment {
        artifact_id: uuid::Uuid,
        sha256: String,
        download_url: String,
    }

    tokio::fs::create_dir_all(artifact_dir)
        .await
        .map_err(|e| format!("cannot create {}: {e}", artifact_dir.display()))?;
    let base = ws_url_to_http_base(&config.control_plane_url);
    let url = format!("{base}/projects/{}/deployment", config.project_id);
    let client = hot_reload::download_client()?;
    let response = client
        .get(&url)
        .bearer_auth(&config.api_key)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("cannot reach the control plane at {url}: {e}"))?;
    match response.status() {
        StatusCode::NOT_FOUND => {
            return Err(format!(
                "project {} has no deployment yet: deploy an artifact, or start with --artifact",
                config.project_id
            ))
        }
        status if !status.is_success() => return Err(format!("{url} answered {status}")),
        _ => {}
    }
    let deployment: Deployment = response
        .json()
        .await
        .map_err(|e| format!("{url} answered an unreadable deployment: {e}"))?;
    let download_url = if deployment.download_url.starts_with('/') {
        format!("{base}{}", deployment.download_url)
    } else {
        deployment.download_url
    };
    hot_reload::download_artifact(
        &client,
        &download_url,
        Some(&config.api_key),
        &deployment.sha256,
        artifact_dir,
        barbacane_compiler::MAX_ARTIFACT_BYTES,
    )
    .await?;
    let current = hot_reload::promote(artifact_dir, deployment.artifact_id).await?;
    Ok((current, deployment.artifact_id))
}

/// Run the compile command.
fn run_compile(
    specs: &[String],
    output: &str,
    manifest_file: &str,
    allow_plaintext: bool,
    provenance_commit: Option<String>,
    provenance_source: Option<String>,
    no_cache: bool,
) -> ExitCode {
    let output_path = Path::new(output);

    let options = CompileOptions {
        allow_plaintext,
        provenance_commit,
        provenance_source,
        no_cache,
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

    // Determine spec paths: from --spec args, or discover from manifest's specs folder.
    let discovered_specs: Vec<std::path::PathBuf>;
    let spec_paths: Vec<&Path> = if !specs.is_empty() {
        specs.iter().map(Path::new).collect()
    } else {
        match project_manifest.discover_spec_files(base_path) {
            Ok(paths) if paths.is_empty() => {
                eprintln!(
                    "error: no spec files provided. Use --spec or add 'specs: ./specs/' to {}",
                    manifest_file
                );
                return ExitCode::from(1);
            }
            Ok(paths) => {
                discovered_specs = paths;
                discovered_specs.iter().map(|p| p.as_path()).collect()
            }
            Err(e) => {
                eprintln!("error: {}", e);
                return ExitCode::from(1);
            }
        }
    };

    // Check that all spec files exist
    for path in &spec_paths {
        if !path.exists() {
            eprintln!("error: spec file not found: {}", path.display());
            return ExitCode::from(1);
        }
    }

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
                spec_paths.len(),
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

/// Run the dev server: compile, serve, watch, and hot-reload on changes.
#[allow(clippy::too_many_arguments)]
async fn run_dev(
    manifest_file: &str,
    spec_overrides: &[String],
    listen: &str,
    metrics: Arc<MetricsRegistry>,
    admin_bind: &str,
    debounce_ms: u64,
    limits: RequestLimits,
) -> ExitCode {
    use barbacane_lib::dev::DevWatcher;

    let manifest_path = Path::new(manifest_file);
    if !manifest_path.exists() {
        eprintln!("barbacane dev: manifest not found: {}", manifest_file);
        return ExitCode::from(1);
    }

    let pinned = std::env::var("BARBACANE_TRUSTED_PUBKEY").ok();
    let trust = match artifact_trust(pinned.as_deref(), true, false) {
        Ok(trust) => trust,
        Err(e) => {
            eprintln!("barbacane dev: {e}");
            return ExitCode::from(1);
        }
    };

    let manifest_dir = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    // Load manifest.
    let mut project_manifest = match ProjectManifest::load(manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("barbacane dev: {}", e);
            return ExitCode::from(1);
        }
    };

    // Determine spec files.
    let mut spec_paths = if !spec_overrides.is_empty() {
        spec_overrides
            .iter()
            .map(std::path::PathBuf::from)
            .collect()
    } else {
        match project_manifest.discover_spec_files(&manifest_dir) {
            Ok(paths) if paths.is_empty() => {
                eprintln!(
                    "barbacane dev: no specs found. Add 'specs: ./specs/' to {} or pass --spec",
                    manifest_file
                );
                return ExitCode::from(1);
            }
            Ok(paths) => paths,
            Err(e) => {
                eprintln!("barbacane dev: {}", e);
                return ExitCode::from(1);
            }
        }
    };

    let spec_count = spec_paths.len();
    let plugin_count = project_manifest.plugins.len();
    eprintln!(
        "barbacane dev: loaded {} ({} spec(s), {} plugin(s))",
        manifest_file, spec_count, plugin_count,
    );

    // Initial compile.
    // no_cache is false: local path: plugins are always read fresh from disk,
    // and url: plugins should use the download cache to avoid re-fetching on every reload.
    let compile_options = CompileOptions {
        allow_plaintext: true,
        ..Default::default()
    };

    let mut temp_artifact = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("barbacane dev: failed to create temp file: {}", e);
            return ExitCode::from(1);
        }
    };

    let temp_path = temp_artifact.path().to_path_buf();

    let spec_refs: Vec<&Path> = spec_paths.iter().map(|p| p.as_path()).collect();
    let compile_start = Instant::now();
    if let Err(e) = compile_with_manifest(
        &spec_refs,
        &project_manifest,
        &manifest_dir,
        &temp_path,
        &compile_options,
    ) {
        eprintln!("barbacane dev: compile error: {}", e);
        return ExitCode::from(1);
    }
    let compile_ms = compile_start.elapsed().as_millis();

    // Load gateway.
    let gateway: SharedGateway = match Gateway::load(
        &temp_path,
        true,
        limits.clone(),
        true,
        metrics.clone(),
        &trust,
        None,
        DEFAULT_PLUGIN_INSTANCE_POOL,
    ) {
        Ok(g) => {
            eprintln!(
                "barbacane dev: compiled {} route(s) in {}ms",
                g.manifest.routes_count, compile_ms
            );
            Arc::new(ArcSwap::new(Arc::new(g)))
        }
        Err(e) => {
            eprintln!("barbacane dev: {}", e);
            return ExitCode::from(1);
        }
    };

    // Parse listen address.
    let addr: SocketAddr = match listen.parse() {
        Ok(a) => a,
        Err(_) => {
            eprintln!("barbacane dev: invalid listen address: {}", listen);
            return ExitCode::from(1);
        }
    };

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("barbacane dev: failed to bind to {}: {}", addr, e);
            return ExitCode::from(1);
        }
    };

    // The bound address, not the requested one: with port 0 the OS picks, and
    // this line is how the caller learns what it picked.
    let bound = listener.local_addr().unwrap_or(addr);
    eprintln!("barbacane dev: listening on http://{}", bound);

    // Shutdown signal.
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = wait_for_shutdown_signal().await;
        eprintln!("\nbarbacane dev: shutting down...");
        let _ = shutdown_tx_clone.send(true);
    });

    // Admin API.
    let admin_manifest: Arc<ArcSwap<barbacane_compiler::Manifest>> = {
        let gw = gateway.load();
        Arc::new(ArcSwap::new(Arc::new(gw.manifest.clone())))
    };
    let drift_detected = Arc::new(std::sync::atomic::AtomicBool::new(false));

    if admin_bind != "off" {
        let admin_addr: SocketAddr = match admin_bind.parse() {
            Ok(a) => a,
            Err(e) => {
                eprintln!(
                    "barbacane dev: invalid --admin-bind address '{}': {}",
                    admin_bind, e
                );
                return ExitCode::from(1);
            }
        };

        let admin_state = Arc::new(admin::AdminState {
            manifest: admin_manifest.clone(),
            metrics: metrics.clone(),
            drift_detected: drift_detected.clone(),
            started_at: Instant::now(),
        });

        // Bind before spawning, so the chosen port is known and can be logged.
        // An admin API that cannot bind is reported and skipped; it must not
        // stop the gateway serving traffic.
        match TcpListener::bind(admin_addr).await {
            Ok(admin_listener) => {
                let admin_bound = admin_listener.local_addr().unwrap_or(admin_addr);
                tokio::spawn(async move {
                    if let Err(e) = admin::start_admin_server(admin_listener, admin_state).await {
                        tracing::error!(error = %e, "Admin server failed");
                    }
                });
                eprintln!("barbacane dev: admin API on http://{}", admin_bound);
            }
            Err(e) => eprintln!(
                "barbacane dev: admin API unavailable, could not bind {}: {}",
                admin_addr, e
            ),
        }
    }

    // File watcher.
    let mut watch_paths: Vec<std::path::PathBuf> = vec![manifest_path
        .canonicalize()
        .unwrap_or(manifest_path.to_path_buf())];

    // Watch spec sources: specs folder (if using manifest discovery) or individual files (if --spec overrides).
    if spec_overrides.is_empty() {
        if let Some(ref specs_dir) = project_manifest.specs {
            let resolved = if Path::new(specs_dir).is_absolute() {
                std::path::PathBuf::from(specs_dir)
            } else {
                manifest_dir.join(specs_dir)
            };
            if resolved.is_dir() {
                watch_paths.push(resolved);
            }
        } else {
            for p in &spec_paths {
                watch_paths.push(p.clone());
            }
        }
    } else {
        for p in &spec_paths {
            watch_paths.push(p.clone());
        }
    }

    // Watch local plugin .wasm files.
    for p in project_manifest.local_plugin_paths(&manifest_dir) {
        if p.exists() {
            watch_paths.push(p);
        }
    }

    let watch_display: Vec<String> = watch_paths
        .iter()
        .filter_map(|p| p.strip_prefix(&manifest_dir).ok().or(Some(p.as_path())))
        .map(|p| p.display().to_string())
        .collect();
    eprintln!("barbacane dev: watching {}", watch_display.join(", "));

    let mut watcher = match DevWatcher::new(&watch_paths) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("barbacane dev: {}", e);
            return ExitCode::from(1);
        }
    };

    let debounce = Duration::from_millis(debounce_ms);

    // Main loop: accept connections + watch for file changes.
    loop {
        tokio::select! {
            // Shutdown.
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            // File change detected.
            changed = watcher.next_change(debounce) => {
                if changed.is_empty() {
                    continue;
                }

                let changed_display: Vec<String> = changed
                    .iter()
                    .filter_map(|p| p.strip_prefix(&manifest_dir).ok().or(Some(p.as_path())))
                    .map(|p| p.display().to_string())
                    .collect();
                eprint!("barbacane dev: [{}] recompiling...", changed_display.join(", "));

                // Re-read manifest if it changed.
                let manifest_canonical = manifest_path
                    .canonicalize()
                    .unwrap_or(manifest_path.to_path_buf());
                let manifest_changed = changed.iter().any(|p| {
                    p.canonicalize().unwrap_or(p.clone()) == manifest_canonical
                });

                if manifest_changed {
                    match ProjectManifest::load(manifest_path) {
                        Ok(m) => project_manifest = m,
                        Err(e) => {
                            eprintln!(" manifest error: {}", e);
                            eprintln!("barbacane dev: serving previous version");
                            continue;
                        }
                    }
                }

                // Re-discover specs if using manifest folder (picks up additions/removals).
                if spec_overrides.is_empty() {
                    match project_manifest.discover_spec_files(&manifest_dir) {
                        Ok(paths) if !paths.is_empty() => spec_paths = paths,
                        Ok(_) => {
                            eprintln!(" no spec files found");
                            eprintln!("barbacane dev: serving previous version");
                            continue;
                        }
                        Err(e) => {
                            eprintln!(" {}", e);
                            eprintln!("barbacane dev: serving previous version");
                            continue;
                        }
                    }
                }

                // Compile to new temp file.
                let new_temp = match tempfile::NamedTempFile::new() {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!(" temp file error: {}", e);
                        continue;
                    }
                };
                let new_path = new_temp.path().to_path_buf();

                let spec_refs: Vec<&Path> = spec_paths.iter().map(|p| p.as_path()).collect();
                let compile_start = Instant::now();
                if let Err(e) = compile_with_manifest(
                    &spec_refs,
                    &project_manifest,
                    &manifest_dir,
                    &new_path,
                    &compile_options,
                ) {
                    eprintln!(" compile error: {}", e);
                    eprintln!("barbacane dev: serving previous version");
                    continue;
                }

                // Load new gateway, off the async workers.
                let balancer = Arc::clone(&gateway.load().balancer);
                match tokio::task::block_in_place(|| {
                    Gateway::load(
                        &new_path,
                        true,
                        limits.clone(),
                        true,
                        metrics.clone(),
                        &trust,
                        Some(balancer),
                        gateway.load().plugin_instance_pool,
                    )
                }) {
                    Ok(new_gw) => {
                        let routes = new_gw.manifest.routes_count;
                        let ms = compile_start.elapsed().as_millis();

                        // Atomic swap.
                        let old = gateway.swap(Arc::new(new_gw));
                        retire(old);

                        // Update admin manifest.
                        let gw = gateway.load();
                        admin_manifest.store(Arc::new(gw.manifest.clone()));

                        // Replace temp artifact.
                        temp_artifact = new_temp;

                        eprintln!(" reloaded {} route(s) in {}ms", routes, ms);
                    }
                    Err(e) => {
                        eprintln!(" load error: {}", e);
                        eprintln!("barbacane dev: serving previous version");
                    }
                }

                // Update watches if manifest changed (new plugins or specs folder).
                if manifest_changed {
                    let mut new_watch_paths: Vec<std::path::PathBuf> = vec![
                        manifest_path.canonicalize().unwrap_or(manifest_path.to_path_buf()),
                    ];
                    if let Some(ref specs_dir) = project_manifest.specs {
                        let resolved = if Path::new(specs_dir).is_absolute() {
                            std::path::PathBuf::from(specs_dir)
                        } else {
                            manifest_dir.join(specs_dir)
                        };
                        if resolved.is_dir() {
                            new_watch_paths.push(resolved);
                        }
                    }
                    for p in project_manifest.local_plugin_paths(&manifest_dir) {
                        if p.exists() {
                            new_watch_paths.push(p);
                        }
                    }
                    if let Err(e) = watcher.update_watches(&new_watch_paths) {
                        tracing::warn!(error = %e, "failed to update file watches");
                    }
                }
            }
            // Accept new connection.
            accept_result = listener.accept() => {
                let (stream, peer_addr) = match accept_result {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                        continue;
                    }
                };

                metrics.connection_opened();

                let gateway_swap = Arc::clone(&gateway);
                let conn_metrics = metrics.clone();
                let mut conn_shutdown_rx = shutdown_rx.clone();
                let client_addr = Some(canonical_peer(peer_addr));

                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let gateway = gateway_swap.load_full();
                        let client_addr = client_addr;
                        async move {
                            gateway
                                .handle_request(req, client_addr, &TrustedProxies::none(), false)
                                .await
                        }
                    });

                    let io = TokioIo::new(stream);
                    let mut builder = auto::Builder::new(TokioExecutor::new());
                    admin::configure_conn_builder(&mut builder, DEFAULT_KEEPALIVE);
                    let conn = builder.serve_connection_with_upgrades(io, service);

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
                                    conn.as_mut().graceful_shutdown();
                                }
                            }
                        }
                    }

                    conn_metrics.connection_closed();
                });
            }
        }
    }

    // Clean up temp artifact.
    drop(temp_artifact);

    ExitCode::SUCCESS
}

/// Default ceiling on concurrently served connections, bounding file-descriptor
/// and task growth under a connection flood. Override with `--max-connections`
/// or `BARBACANE_MAX_CONNECTIONS`.
const DEFAULT_MAX_CONNECTIONS: usize = 10_000;

/// `--max-connections`: a whole number from 1 to the largest a semaphore holds.
/// An empty value, as an empty `BARBACANE_MAX_CONNECTIONS` gives, is the default.
fn parse_max_connections(value: &str) -> Result<usize, String> {
    if value.is_empty() {
        return Ok(DEFAULT_MAX_CONNECTIONS);
    }
    let max = tokio::sync::Semaphore::MAX_PERMITS;
    value
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=max).contains(n))
        .ok_or_else(|| format!("expected a whole number from 1 to {max}"))
}

/// Default size of the plugin instance pool, in instances alive at once.
/// Override with `--plugin-instance-pool` or `BARBACANE_PLUGIN_INSTANCE_POOL`.
const DEFAULT_PLUGIN_INSTANCE_POOL: u32 = 1000;

/// `--plugin-instance-pool`: a whole number, 0 for no pool. An empty value is
/// the default.
fn parse_plugin_instance_pool(value: &str) -> Result<u32, String> {
    if value.is_empty() {
        return Ok(DEFAULT_PLUGIN_INSTANCE_POOL);
    }
    value
        .parse::<u32>()
        .map_err(|_| format!("expected a whole number from 0 to {}", u32::MAX))
}

/// Whether a listener on `addr` serves plaintext to other hosts.
fn plaintext_off_loopback(addr: SocketAddr, tls: bool) -> bool {
    !tls && !addr.ip().to_canonical().is_loopback()
}

/// The process's soft open-file limit; `None` when unlimited.
#[cfg(unix)]
fn open_file_limit() -> Option<u64> {
    rustix::process::getrlimit(rustix::process::Resource::Nofile).current
}

#[cfg(not(unix))]
fn open_file_limit() -> Option<u64> {
    None
}

/// Whether the open-file limit runs out before `max_connections` connections
/// are open. Every connection holds a descriptor, and the process holds others.
fn open_file_limit_too_low(max_connections: usize, limit: Option<u64>) -> bool {
    limit.is_some_and(|limit| limit <= max_connections as u64)
}

/// Pause after a failed `accept`, so a listener whose accepts keep failing
/// (file descriptors exhausted) does not spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// How long a WebSocket relay ended by shutdown gives its other direction to
/// send its Close frame.
const RELAY_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// How long a WebSocket relay may pass no frame, in either direction, before
/// the gateway closes it.
const WEBSOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// A connection's slot under `--max-connections`, shared by the connection and
/// a WebSocket relay it becomes.
#[derive(Clone)]
struct ConnectionPermit {
    _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
}

/// A WebSocket handshake hyper completes after the `101`, with the slot of the
/// connection it upgrades.
struct PendingUpgrade {
    on_upgrade: hyper::upgrade::OnUpgrade,
    permit: Option<ConnectionPermit>,
}

/// The Close frame a relay sends both peers at shutdown.
fn going_away() -> tokio_tungstenite::tungstenite::Message {
    use tokio_tungstenite::tungstenite::protocol::frame::{coding::CloseCode, CloseFrame};
    tokio_tungstenite::tungstenite::Message::Close(Some(CloseFrame {
        code: CloseCode::Away,
        reason: "gateway shutting down".into(),
    }))
}

/// Maximum time allowed to complete a TLS handshake before the connection is
/// dropped. Bounds slowloris-style handshake stalls.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default idle keep-alive timeout used where no operator value is supplied
/// (e.g. the dev server). Also the per-request header-read deadline.
const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(60);

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
    /// Artifact ID currently loaded (set after loading).
    initial_artifact_id: Option<uuid::Uuid>,
}

/// Whether a request is a WebSocket handshake (RFC 6455 section 4.1): a GET
/// with `Upgrade: websocket`, `Connection` naming `upgrade`, and a
/// `Sec-WebSocket-Key`. `headers` holds lower-case names.
fn is_websocket_handshake(method: &str, headers: &HashMap<String, String>) -> bool {
    method.eq_ignore_ascii_case("GET")
        && headers
            .get("upgrade")
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && headers.get("connection").is_some_and(|v| {
            v.split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        })
        && headers.contains_key("sec-websocket-key")
}

/// Whether the control plane link may be used: a `wss://` link, a `ws://`
/// link to loopback, or any link when `allow_plaintext` is set. The link
/// carries the data plane's API key in its first frame.
fn control_plane_link_allowed(url: &str, allow_plaintext: bool) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("ws://") else {
        return Ok(());
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _port)| host)
        .trim_matches(['[', ']']);
    let loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if loopback || allow_plaintext {
        Ok(())
    } else {
        Err(format!(
            "the control plane link {url} is plaintext to a host other than loopback and would carry the API key in clear; use wss://, or --allow-plaintext-control-plane"
        ))
    }
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

/// The peer as the gateway reports it. A dual-stack listener sees an IPv4
/// client as an IPv4-mapped IPv6 address, which is reported as the IPv4 address.
fn canonical_peer(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Run the serve command.
#[allow(clippy::too_many_arguments)]
async fn run_serve(
    artifact: Option<&str>,
    artifact_dir: std::path::PathBuf,
    listen: &str,
    dev: bool,
    limits: RequestLimits,
    allow_plaintext_upstream: bool,
    tls_config: Option<TlsSettings>,
    tls_reload_interval: u64,
    metrics: Arc<MetricsRegistry>,
    keepalive_timeout: u64,
    shutdown_timeout: u64,
    max_connections: usize,
    plugin_instance_pool: u32,
    connected_mode: Option<ConnectedModeConfig>,
    admin_bind: &str,
    allow_unsigned: bool,
    trusted_proxies: TrustedProxies,
) -> ExitCode {
    let pinned = std::env::var("BARBACANE_TRUSTED_PUBKEY").ok();
    let trust = match artifact_trust(pinned.as_deref(), dev, allow_unsigned) {
        Ok(trust) => trust,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    // The artifact to start with. In connected mode: the last one deployed,
    // kept in the artifact directory; else --artifact; with neither, the
    // project's deployment, fetched from the control plane.
    let mut connected_mode = connected_mode;
    let current = connected_mode
        .as_ref()
        .and_then(|_| hot_reload::current_artifact(&artifact_dir));
    let (initial_artifact, initial_artifact_id) = match (current, artifact, &connected_mode) {
        (Some(current), _, _) => {
            if let Some(artifact) = artifact {
                eprintln!(
                    "barbacane: loading the last deployed artifact {} in place of {artifact}",
                    current.display(),
                );
            }
            (current, hot_reload::current_artifact_id(&artifact_dir))
        }
        (None, Some(artifact), _) => {
            let path = Path::new(artifact);
            if !path.exists() {
                eprintln!("error: artifact not found: {artifact}");
                return ExitCode::from(1);
            }
            (path.to_path_buf(), None)
        }
        (None, None, Some(config)) => {
            eprintln!(
                "barbacane: no artifact yet; fetching project {}'s deployment from the control plane",
                config.project_id
            );
            match bootstrap_deployment(config, &artifact_dir).await {
                Ok((path, id)) => (path, Some(id)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        (None, None, None) => {
            eprintln!("error: --artifact is required without --control-plane");
            return ExitCode::from(1);
        }
    };
    if let Some(config) = connected_mode.as_mut() {
        config.initial_artifact_id = initial_artifact_id;
    }
    let gateway: SharedGateway = match Gateway::load(
        &initial_artifact,
        dev,
        limits.clone(),
        allow_plaintext_upstream,
        metrics.clone(),
        &trust,
        None,
        plugin_instance_pool,
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
    let tls_server_config: Option<tls::SharedServerConfig> = match &tls_config {
        Some(settings) => match tls::server_config(settings) {
            Ok(config) => Some(Arc::new(ArcSwap::new(config))),
            Err(e) => {
                eprintln!("error: {}", e);
                return ExitCode::from(1);
            }
        },
        None => None,
    };

    let protocol = if tls_server_config.is_some() {
        "https"
    } else {
        "http"
    };
    // The bound address, not the requested one: with port 0 the OS picks, and
    // this line is how the caller learns what it picked.
    let bound = listener.local_addr().unwrap_or(addr);
    eprintln!("barbacane: listening on {}://{}", protocol, bound);
    if plaintext_off_loopback(bound, tls_server_config.is_some()) {
        tracing::warn!(
            listen = %bound,
            "listening in plaintext on {bound}, an address other than loopback: \
             terminate TLS in front of the gateway, or pass --tls-cert and --tls-key"
        );
    }
    let open_files = open_file_limit();
    let open_files_text = open_files.map_or_else(|| "unlimited".to_string(), |n| n.to_string());
    tracing::info!(
        max_connections,
        open_file_limit = %open_files_text,
        "serving at most {max_connections} concurrent connections; open-file limit {open_files_text}"
    );
    if open_file_limit_too_low(max_connections, open_files) {
        tracing::warn!(
            max_connections,
            open_file_limit = %open_files_text,
            "the open-file limit {open_files_text} is not above --max-connections \
             {max_connections}: connections fail at accept before the cap sheds them; \
             raise the limit (ulimit -n, LimitNOFILE) or lower --max-connections"
        );
    }
    if !trusted_proxies.is_empty() {
        tracing::info!(
            trusted_proxies = %trusted_proxies.networks(),
            header = trusted_proxies.header().name(),
            "client address read from {} behind {}",
            trusted_proxies.header().name(),
            trusted_proxies.networks()
        );
    }
    let trusted_proxies = Arc::new(trusted_proxies);
    if let (Some(settings), Some(shared)) = (tls_config, &tls_server_config) {
        tokio::spawn(tls::reload(
            settings,
            Arc::clone(shared),
            Duration::from_secs(tls_reload_interval),
        ));
    }

    // Create shutdown signal channel
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // Spawn signal handler task
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = wait_for_shutdown_signal().await;
        eprintln!("barbacane: received shutdown signal, draining connections...");
        let _ = shutdown_tx_clone.send(true);
    });

    // Artifact hash watch channel for drift detection
    let initial_hash = gateway.load().manifest.artifact_hash.clone();
    let (artifact_hash_tx, artifact_hash_rx) = watch::channel(Some(initial_hash));
    let (artifact_id_tx, artifact_id_rx) = watch::channel(initial_artifact_id);

    // Shared manifest for admin API (updated on hot-reload)
    let admin_manifest: Arc<ArcSwap<barbacane_compiler::Manifest>> = {
        let gw = gateway.load();
        Arc::new(ArcSwap::new(Arc::new(gw.manifest.clone())))
    };
    let drift_detected = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Start control plane client if in connected mode. Artifacts are downloaded
    // with the same key the data plane registers with.
    let download_token = connected_mode.as_ref().map(|config| config.api_key.clone());
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
            initial_artifact_id: config.initial_artifact_id,
        });
        let (rx, tx) = client.start(
            shutdown_rx.clone(),
            artifact_hash_rx,
            artifact_id_rx,
            drift_detected.clone(),
        );
        (Some(rx), Some(tx), Some(http_base))
    } else {
        (None, None, None)
    };

    // Start admin API server if enabled
    if admin_bind != "off" {
        let admin_addr: SocketAddr = match admin_bind.parse() {
            Ok(addr) => addr,
            Err(e) => {
                eprintln!(
                    "error: invalid --admin-bind address '{}': {}",
                    admin_bind, e
                );
                return ExitCode::from(1);
            }
        };

        let admin_state = Arc::new(admin::AdminState {
            manifest: admin_manifest.clone(),
            metrics: metrics.clone(),
            drift_detected: drift_detected.clone(),
            started_at: Instant::now(),
        });

        // Bind before spawning, so the chosen port is known and can be logged.
        // An admin API that cannot bind is reported and skipped; metrics and
        // health going missing must not take the data plane down with them.
        match TcpListener::bind(admin_addr).await {
            Ok(admin_listener) => {
                let admin_bound = admin_listener.local_addr().unwrap_or(admin_addr);
                tokio::spawn(async move {
                    if let Err(e) = admin::start_admin_server(admin_listener, admin_state).await {
                        tracing::error!(error = %e, "Admin server failed");
                    }
                });
                eprintln!("barbacane: admin API on http://{}", admin_bound);
            }
            Err(e) => eprintln!(
                "error: admin API unavailable, could not bind {}: {}",
                admin_addr, e
            ),
        }
    }

    // Keep-alive / idle timeout: bounds how long an idle connection may wait for
    // the next request's headers (slowloris defense + HTTP keep-alive idle).
    let keepalive_duration = Duration::from_secs(keepalive_timeout.max(1));
    let shutdown_duration = Duration::from_secs(shutdown_timeout);

    // Cap concurrently served connections to bound FD/task growth under a flood.
    let conn_semaphore = Arc::new(tokio::sync::Semaphore::new(max_connections));

    // Track active connections for graceful shutdown
    let active_connections = Arc::new(AtomicU64::new(0));

    // Accept connections
    loop {
        tokio::select! {
            // Check for shutdown signal
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    admin::DRAINING.store(true, Ordering::Relaxed);
                    background().begin_shutdown();
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
                let artifact_hash_tx_clone = artifact_hash_tx.clone();
                let artifact_id_tx_clone = artifact_id_tx.clone();
                let admin_manifest_clone = admin_manifest.clone();
                let trust_clone = trust.clone();
                let download_token = download_token.clone();

                tokio::spawn(async move {
                    let result = perform_hot_reload(
                        notification,
                        download_token,
                        &gateway_clone,
                        &artifact_dir_clone,
                        dev,
                        limits_clone,
                        allow_plaintext_upstream,
                        metrics_clone,
                        trust_clone,
                    )
                    .await;

                    // Send response to control plane
                    let response = match &result {
                        HotReloadResult::Success { artifact_id } => {
                            eprintln!(
                                "barbacane: hot-reload successful for artifact {}",
                                artifact_id
                            );
                            // Update artifact hash for drift detection
                            let gw = gateway_clone.load();
                            let _ =
                                artifact_hash_tx_clone.send(Some(gw.manifest.artifact_hash.clone()));
                            let _ = artifact_id_tx_clone.send(Some(*artifact_id));
                            // Update admin API manifest
                            admin_manifest_clone.store(Arc::new(gw.manifest.clone()));

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
                        // Accept errors (file descriptors exhausted, a reset
                        // in the backlog) repeat at once; the pause keeps the
                        // loop from spinning on them.
                        eprintln!("error: accept failed: {}", e);
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                        continue;
                    }
                };

                // Connection cap: shed load when at capacity rather than letting
                // FDs/tasks grow without bound.
                let permit = match conn_semaphore.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::warn!(%peer_addr, "connection limit reached; dropping connection");
                        drop(stream);
                        continue;
                    }
                };

                // Track connection
                metrics.connection_opened();
                active_connections.fetch_add(1, Ordering::SeqCst);

                // Each request looks the gateway up, so a hot-reload is
                // observed by the next request on every connection; a request
                // in flight keeps the gateway it started with.
                let gateway_swap = Arc::clone(&gateway);
                // The configuration current at accept time: a reload applies
                // to the next handshake.
                let tls_acceptor = tls_server_config
                    .as_ref()
                    .map(|config| TlsAcceptor::from(config.load_full()));
                let conn_tls = tls_acceptor.is_some();
                let conn_metrics = metrics.clone();
                let conn_counter = active_connections.clone();
                let mut conn_shutdown_rx = shutdown_rx.clone();
                let client_addr = Some(canonical_peer(peer_addr));
                let conn_trusted_proxies = Arc::clone(&trusted_proxies);
                let conn_keepalive = keepalive_duration;

                tokio::spawn(async move {
                    // Held for the connection's lifetime, and by a WebSocket
                    // relay the connection becomes; released when both end.
                    let permit = ConnectionPermit {
                        _permit: Arc::new(permit),
                    };
                    let service = service_fn(move |mut req: Request<hyper::body::Incoming>| {
                        req.extensions_mut().insert(permit.clone());
                        let gateway = gateway_swap.load_full();
                        let client_addr = client_addr;
                        let trusted_proxies = Arc::clone(&conn_trusted_proxies);
                        async move {
                            gateway
                                .handle_request(req, client_addr, &trusted_proxies, conn_tls)
                                .await
                        }
                    });

                    if let Some(acceptor) = tls_acceptor {
                        // TLS connection - uses auto protocol detection (HTTP/1.1 or HTTP/2 via ALPN).
                        // Bound the handshake so a stalled client can't pin the task.
                        let accepted =
                            match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                                .await
                            {
                                Ok(Ok(tls_stream)) => Some(tls_stream),
                                Ok(Err(e)) => {
                                    tracing::debug!(error = %e, "TLS handshake failed");
                                    None
                                }
                                Err(_) => {
                                    tracing::debug!("TLS handshake timed out");
                                    None
                                }
                            };
                        if let Some(tls_stream) = accepted {
                                let io = TokioIo::new(tls_stream);
                                let mut builder = auto::Builder::new(TokioExecutor::new());
                                admin::configure_conn_builder(&mut builder, conn_keepalive);
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
                    } else {
                        // Plain TCP connection - uses auto protocol detection
                        // Supports both HTTP/1.1 and HTTP/2 prior knowledge (h2c)
                        let io = TokioIo::new(stream);
                        let mut builder = auto::Builder::new(TokioExecutor::new());
                        admin::configure_conn_builder(&mut builder, conn_keepalive);
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

    // No new connection is accepted while the ones in flight finish: a
    // client or a probe that connects now is refused, not queued and reset.
    drop(listener);

    // Wait for active connections, WebSocket relays and post-response
    // middleware to finish (with timeout).
    let drain_start = Instant::now();
    // Progress is reported when it changes, not on every check.
    let mut reported = None;
    loop {
        let active = active_connections.load(Ordering::SeqCst);
        let tasks = background().running();
        if active == 0 && tasks == 0 {
            eprintln!("barbacane: all connections drained, shutting down");
            break;
        }

        if drain_start.elapsed() > shutdown_duration {
            eprintln!(
                "barbacane: shutdown timeout reached, {active} connection(s) and {tasks} background task(s) still active"
            );
            break;
        }

        if reported != Some((active, tasks)) {
            eprintln!(
                "barbacane: waiting for {active} active connection(s) and {tasks} background task(s) to complete..."
            );
            reported = Some((active, tasks));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    ExitCode::SUCCESS
}

/// The (plugin, config) pairs an operation runs: its middlewares, then its
/// dispatcher.
fn plugin_configs(
    op: &barbacane_compiler::CompiledOperation,
) -> impl Iterator<Item = (&String, &serde_json::Value)> {
    op.middlewares
        .iter()
        .map(|mw| (&mw.name, &mw.config))
        .chain(std::iter::once((&op.dispatch.name, &op.dispatch.config)))
}

/// The JSON a plugin's `init` receives for `config`. An omitted config is an
/// empty object: the SDK deserializes it into the plugin's config struct, which
/// `null` cannot be.
fn init_config(config: &serde_json::Value) -> Vec<u8> {
    if config.is_null() {
        return b"{}".to_vec();
    }
    serde_json::to_vec(config).unwrap_or_default()
}

/// Run the check command: load the artifact as `serve` does, then run each
/// plugin's `init` with every configuration the routes give it.
///
/// Exit codes are those `serve` uses for the same failures: 1, and 13 when a
/// secret does not resolve.
fn run_check(artifact: &str, allow_unsigned: bool) -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_max_level(tracing::Level::WARN)
        .with_target(false)
        .without_time()
        .try_init();

    let path = Path::new(artifact);
    if !path.exists() {
        eprintln!("error: artifact not found: {artifact}");
        return ExitCode::from(1);
    }
    let pinned = std::env::var("BARBACANE_TRUSTED_PUBKEY").ok();
    let trust = match artifact_trust(pinned.as_deref(), false, allow_unsigned) {
        Ok(trust) => trust,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    let gateway = match Gateway::load(
        path,
        false,
        RequestLimits::default(),
        false,
        Arc::new(MetricsRegistry::new()),
        &trust,
        None,
        0,
    ) {
        Ok(gateway) => gateway,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(if e.contains("failed to resolve secrets") {
                13
            } else {
                1
            });
        }
    };

    let mut checked = HashSet::new();
    let mut failures = Vec::new();
    for op in &gateway.operations {
        for (name, config) in plugin_configs(op) {
            let key = barbacane_wasm::InstanceKey::new(name, config);
            if !checked.insert(key.clone()) {
                continue;
            }
            if let Err(e) = gateway.plugin_pool.get_instance(&key) {
                failures.push(format!(
                    "plugin '{name}' does not initialise with its configuration on {} {}: {e}",
                    op.method, op.path
                ));
            }
        }
    }

    let manifest = &gateway.manifest;
    let summary = format!(
        "{artifact}: artifact format {}, compiled by barbacane {}, signature {}; \
         {} route(s), {} plugin(s), {} plugin configuration(s)",
        manifest.barbacane_artifact_version,
        manifest.compiler_version,
        match trust {
            ArtifactTrust::Pinned(_) => "verified",
            ArtifactTrust::Unsigned => "not checked",
        },
        manifest.routes_count,
        manifest.plugins.len(),
        checked.len(),
    );
    drop(gateway);

    if failures.is_empty() {
        println!("\u{2713} {summary}");
        ExitCode::SUCCESS
    } else {
        for failure in &failures {
            eprintln!("error: {failure}");
        }
        eprintln!("\u{2717} {summary}");
        ExitCode::from(1)
    }
}

/// Release, on the blocking pool, the gateway a reload replaced. When that is
/// the last reference, freeing its compiled plugins and stopping its engine's
/// epoch thread happens there rather than on an async worker; a request still
/// holding the gateway frees it where the request ends.
fn retire(gateway: Arc<Gateway>) {
    tokio::task::spawn_blocking(move || drop(gateway));
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

    // `control` is built from the control plane's spec rather than derived.
    let matches = Cli::command()
        .subcommand(barbacane_lib::control::command())
        .get_matches();
    if let Some(("control", control)) = matches.subcommand() {
        return barbacane_lib::control::run(control).await;
    }
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    match cli.command {
        Commands::Compile {
            spec,
            output,
            manifest,
            allow_plaintext,
            provenance_commit,
            provenance_source,
            no_cache,
        } => run_compile(
            &spec,
            &output,
            &manifest,
            allow_plaintext,
            provenance_commit,
            provenance_source,
            no_cache,
        ),
        Commands::Validate { spec, format } => run_validate(&spec, &format),
        Commands::Check {
            artifact,
            allow_unsigned,
        } => run_check(&artifact, allow_unsigned),
        Commands::Key { action } => run_key(action),
        Commands::Dev {
            listen,
            manifest,
            spec,
            log_level,
            admin_bind,
            debounce_ms,
            log_format,
            max_body_size,
            max_headers,
            max_header_size,
            max_uri_length,
        } => {
            let log_fmt = barbacane_telemetry::LogFormat::parse(&log_format)
                .unwrap_or(barbacane_telemetry::LogFormat::Pretty);
            let telemetry_config = barbacane_telemetry::TelemetryConfig::new()
                .with_log_level(&log_level)
                .with_log_format(log_fmt);

            let telemetry = match barbacane_telemetry::Telemetry::init(telemetry_config) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: failed to initialize telemetry: {}", e);
                    return ExitCode::from(1);
                }
            };

            let limits = RequestLimits {
                max_body_size,
                max_headers,
                max_header_size,
                max_uri_length,
                ..RequestLimits::default()
            };

            run_dev(
                &manifest,
                &spec,
                &listen,
                telemetry.metrics_clone(),
                &admin_bind,
                debounce_ms,
                limits,
            )
            .await
        }
        Commands::Init {
            name,
            template,
            fetch_plugins,
        } => run_init(&name, &template, fetch_plugins).await,
        Commands::Serve {
            artifact,
            artifact_dir,
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
            allow_plaintext_control_plane,
            allow_unsigned,
            tls_cert,
            tls_key,
            tls_min_version,
            tls_client_ca,
            tls_reload_interval,
            keepalive_timeout,
            shutdown_timeout,
            max_connections,
            plugin_instance_pool,
            control_plane,
            project_id,
            api_key,
            data_plane_name,
            admin_bind,
            no_introspection,
            trusted_proxies,
            client_ip_header,
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
                (Some(cert), Some(key)) => Some(TlsSettings {
                    cert_path: cert.into(),
                    key_path: key.into(),
                    min_version: tls_min_version,
                    client_ca_path: tls_client_ca.map(Into::into),
                }),
                (None, None) if tls_client_ca.is_some() => {
                    eprintln!("error: --tls-client-ca requires --tls-cert and --tls-key");
                    return ExitCode::from(1);
                }
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
                // A body gets the deadline headers get.
                body_read_timeout: Duration::from_secs(keepalive_timeout.max(1)),
            };

            if no_introspection {
                INTROSPECTION_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
            }

            // Validate connected mode options
            let connected_mode = match (&control_plane, &project_id, &api_key) {
                (Some(cp), Some(pid), Some(key)) => {
                    if let Err(e) = control_plane_link_allowed(cp, allow_plaintext_control_plane) {
                        eprintln!("error: {e}");
                        return ExitCode::from(1);
                    }
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
                        initial_artifact_id: None,
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

            // Deployed artifacts are kept in --artifact-dir, else next to
            // --artifact. Without a control plane, --artifact is the artifact.
            let artifact_dir = match (&artifact_dir, &artifact, &connected_mode) {
                (Some(dir), _, _) => std::path::PathBuf::from(dir),
                (None, Some(path), _) => Path::new(path)
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
                (None, None, Some(_)) => {
                    eprintln!(
                        "error: connected mode needs --artifact-dir (BARBACANE_ARTIFACT_DIR) or --artifact"
                    );
                    return ExitCode::from(1);
                }
                (None, None, None) => {
                    eprintln!("error: --artifact is required without --control-plane");
                    return ExitCode::from(1);
                }
            };

            run_serve(
                artifact.as_deref(),
                artifact_dir,
                &listen,
                dev,
                limits,
                allow_plaintext_upstream,
                tls_config,
                tls_reload_interval,
                metrics,
                keepalive_timeout,
                shutdown_timeout,
                max_connections,
                plugin_instance_pool,
                connected_mode,
                &admin_bind,
                allow_unsigned,
                trusted_proxies.with_header(client_ip_header),
            )
            .await
        }
    }
}

/// `barbacane key` actions.
#[derive(Subcommand, Debug)]
enum KeyAction {
    /// Generate a signing key (PKCS#8) and print its public key.
    ///
    /// Compile with BARBACANE_SIGNING_KEY set to the key file, and pin the
    /// printed public key on data planes with BARBACANE_TRUSTED_PUBKEY.
    Generate {
        /// Where to write the private key. An existing file is not overwritten.
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Print the public key of a signing key, as BARBACANE_TRUSTED_PUBKEY takes it.
    Public {
        /// The signing key: PKCS#8, DER or PEM.
        #[arg(long)]
        key: std::path::PathBuf,
    },
}

fn run_key(action: KeyAction) -> ExitCode {
    match action {
        KeyAction::Generate { out } => {
            let result = barbacane_compiler::signing::generate_signing_key().and_then(|key| {
                write_private_key(&out, &key)
                    .map_err(|e| format!("writing {}: {e}", out.display()))?;
                barbacane_compiler::signing::signing_public_key(&key)
            });
            match result {
                Ok(public) => {
                    eprintln!(
                        "wrote {}: keep it secret, and compile with BARBACANE_SIGNING_KEY={}",
                        out.display(),
                        out.display()
                    );
                    eprintln!("pin this public key on data planes as BARBACANE_TRUSTED_PUBKEY:");
                    println!("{public}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        KeyAction::Public { key } => {
            let result = std::fs::read(&key)
                .map_err(|e| format!("reading {}: {e}", key.display()))
                .and_then(|bytes| barbacane_compiler::signing::signing_public_key(&bytes));
            match result {
                Ok(public) => {
                    println!("{public}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// Write a private key to a new file that only its owner can read.
fn write_private_key(path: &Path, key: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, key)
}

/// Whose signature an artifact must carry to be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ArtifactTrust {
    /// Signed by the key whose hex Ed25519 public key this is.
    Pinned(String),
    /// Loaded unsigned: `--dev` or `--allow-unsigned`, with no key pinned.
    Unsigned,
}

/// The trust a gateway applies to every artifact it loads, hot reloads
/// included. A pinned key always requires its signature; with none, only
/// `--dev` or `--allow-unsigned` lets an unsigned artifact load.
fn artifact_trust(
    pinned: Option<&str>,
    dev: bool,
    allow_unsigned: bool,
) -> Result<ArtifactTrust, String> {
    match pinned.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => match hex::decode(key) {
            Ok(bytes) if bytes.len() == 32 => Ok(ArtifactTrust::Pinned(key.to_string())),
            _ => Err(
                "BARBACANE_TRUSTED_PUBKEY is not an Ed25519 public key (64 hex characters, \
                 as `barbacane key public` prints it)"
                    .to_string(),
            ),
        },
        None if dev || allow_unsigned => Ok(ArtifactTrust::Unsigned),
        None => Err(
            "no trusted signing key: set BARBACANE_TRUSTED_PUBKEY to the public key \
             that signs your artifacts (`barbacane key public --key <file>` prints it), or \
             pass --allow-unsigned to load unsigned artifacts"
                .to_string(),
        ),
    }
}

/// Longest client-supplied request id kept.
const MAX_REQUEST_ID_BYTES: usize = 128;

/// A client-supplied `x-request-id`, when it is at most
/// [`MAX_REQUEST_ID_BYTES`] of visible ASCII: a request id is written into
/// log lines and response headers as is.
fn client_request_id(value: &str) -> Option<String> {
    let well_formed = !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value.bytes().all(|b| (0x21..=0x7e).contains(&b));
    well_formed.then(|| value.to_string())
}

/// The trace id of a W3C `traceparent` (`00-<trace-id>-<parent-id>-<flags>`,
/// version 00 and nothing after the flags): 32 lowercase hex digits, not all
/// zero, followed by a 16-digit parent id and 2-digit flags.
fn trace_id_from_traceparent(value: &str) -> Option<String> {
    let mut parts = value.trim().split('-');
    let (version, trace_id, parent_id, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let lower_hex = |s: &str, len: usize| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let well_formed = version == "00"
        && parts.next().is_none()
        && lower_hex(trace_id, 32)
        && trace_id.bytes().any(|b| b != b'0')
        && lower_hex(parent_id, 16)
        && lower_hex(flags, 2);
    well_formed.then(|| trace_id.to_string())
}

/// Refuse an artifact format this binary does not read: one older than
/// [`barbacane_compiler::OLDEST_ARTIFACT_VERSION`], or newer than the format
/// this binary writes.
///
/// Without this the mismatch surfaces as an integrity failure, because the
/// hash covers the files whose shape changed. That names the wrong cause and
/// sends the operator looking for tampering.
fn check_artifact_version(found: u32) -> Result<(), String> {
    let oldest = barbacane_compiler::OLDEST_ARTIFACT_VERSION;
    let newest = barbacane_compiler::ARTIFACT_VERSION;
    if found > newest {
        return Err(format!(
            "artifact format version {found} is newer than this build reads (up to version \
             {newest}): it was compiled by a newer barbacane. Upgrade the data plane."
        ));
    }
    if found < oldest {
        return Err(format!(
            "artifact format version {found} is older than this build reads (from version \
             {oldest}). Recompile the spec with this version of barbacane."
        ));
    }
    Ok(())
}

/// The limits one plugin runs with: those it declares, the defaults for the
/// rest. A plugin that receives bodies (a dispatcher, or a middleware with
/// `body_access`) gets at least `max_body_size` plus 4 MiB of memory, so one
/// body fits beside its working memory.
fn limits_for_plugin(
    declared: &DeclaredLimits,
    receives_body: bool,
    max_body_size: usize,
) -> PluginLimits {
    const MIB: usize = 1024 * 1024;
    let defaults = PluginLimits::default();
    let declared_memory = declared
        .memory_mib
        .map_or(defaults.max_memory_bytes, |mib| mib as usize * MIB);
    let max_memory_bytes = if receives_body {
        declared_memory.max(max_body_size.saturating_add(4 * MIB))
    } else {
        declared_memory
    };
    PluginLimits {
        max_memory_bytes,
        max_fuel: declared.fuel.unwrap_or(defaults.max_fuel),
        max_execution_ms: declared.timeout_ms.unwrap_or(defaults.max_execution_ms),
        ..defaults
    }
}

#[cfg(test)]
mod plugin_limits_tests {
    use super::{limits_for_plugin, DeclaredLimits, PluginLimits};

    const MIB: usize = 1024 * 1024;

    #[test]
    fn a_plugin_declaring_nothing_gets_the_defaults() {
        let limits = limits_for_plugin(&DeclaredLimits::default(), false, MIB);
        let defaults = PluginLimits::default();
        assert_eq!(limits.max_memory_bytes, defaults.max_memory_bytes);
        assert_eq!(limits.max_fuel, defaults.max_fuel);
        assert_eq!(limits.max_execution_ms, defaults.max_execution_ms);
    }

    #[test]
    fn declared_limits_replace_the_defaults() {
        let declared = DeclaredLimits {
            memory_mib: Some(4),
            fuel: Some(5_000),
            timeout_ms: Some(20),
        };
        let limits = limits_for_plugin(&declared, false, MIB);
        assert_eq!(limits.max_memory_bytes, 4 * MIB);
        assert_eq!(limits.max_fuel, 5_000);
        assert_eq!(limits.max_execution_ms, 20);
    }

    #[test]
    fn a_large_body_limit_raises_memory_only_for_plugins_receiving_bodies() {
        let declared = DeclaredLimits {
            memory_mib: Some(4),
            ..Default::default()
        };
        assert_eq!(
            limits_for_plugin(&declared, false, 100 * MIB).max_memory_bytes,
            4 * MIB
        );
        assert_eq!(
            limits_for_plugin(&declared, true, 100 * MIB).max_memory_bytes,
            104 * MIB
        );
        assert_eq!(
            limits_for_plugin(&DeclaredLimits::default(), false, 100 * MIB).max_memory_bytes,
            PluginLimits::default().max_memory_bytes
        );
        // A declared limit above the body floor stands.
        let roomy = DeclaredLimits {
            memory_mib: Some(256),
            ..Default::default()
        };
        assert_eq!(
            limits_for_plugin(&roomy, true, MIB).max_memory_bytes,
            256 * MIB
        );
    }
}

#[cfg(test)]
mod init_tests {
    #[test]
    fn plugins_come_from_the_release_of_this_version() {
        assert_eq!(
            super::plugin_url("mock.wasm"),
            format!(
                "https://github.com/barbacane-dev/barbacane/releases/download/v{}/mock.wasm",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}

#[cfg(test)]
mod artifact_trust_tests {
    use super::{artifact_trust, ArtifactTrust};

    const KEY: &str = "19bf44096984cdfe8541bac167dc3b96c85086aa30b6b6cb0c5c38ad703166e1";

    #[test]
    fn a_pinned_key_is_required_whatever_the_flags() {
        for (dev, allow) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                artifact_trust(Some(KEY), dev, allow),
                Ok(ArtifactTrust::Pinned(KEY.to_string()))
            );
        }
        assert_eq!(
            artifact_trust(Some(&format!("  {KEY}\n")), false, false),
            Ok(ArtifactTrust::Pinned(KEY.to_string()))
        );
    }

    #[test]
    fn without_a_key_only_dev_or_allow_unsigned_loads_unsigned_artifacts() {
        assert_eq!(
            artifact_trust(None, true, false),
            Ok(ArtifactTrust::Unsigned)
        );
        assert_eq!(
            artifact_trust(None, false, true),
            Ok(ArtifactTrust::Unsigned)
        );
        assert_eq!(
            artifact_trust(Some("  "), false, true),
            Ok(ArtifactTrust::Unsigned)
        );
        let error = artifact_trust(None, false, false).unwrap_err();
        assert!(error.contains("BARBACANE_TRUSTED_PUBKEY") && error.contains("--allow-unsigned"));
        assert!(artifact_trust(Some(""), false, false).is_err());
    }

    #[test]
    fn a_pinned_value_that_is_not_a_public_key_is_refused() {
        for bad in ["not-hex", "abcd", &KEY[..62], &format!("{KEY}00")] {
            assert!(artifact_trust(Some(bad), true, true).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod wasi_tests {
    /// The compiler refuses what the data plane cannot link, and only that.
    #[test]
    fn the_compiler_and_the_runtime_agree_on_the_wasi_set() {
        assert_eq!(
            barbacane_compiler::wasi::WASI_IMPORTS,
            barbacane_wasm::WASI_IMPORTS
        );
        assert_eq!(
            barbacane_compiler::wasi::WASI_MODULE,
            barbacane_wasm::WASI_MODULE
        );
    }
}

#[cfg(test)]
mod relay_tests {
    use super::Gateway;
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;
    use tokio_util::sync::CancellationToken;

    type Ws = WebSocketStream<tokio::io::DuplexStream>;

    /// A relay between a client and an upstream, with the peers' ends.
    async fn relay(shutdown: CancellationToken) -> (Ws, Ws, tokio::task::JoinHandle<()>) {
        relay_idle_after(shutdown, Duration::from_secs(60)).await
    }

    async fn relay_idle_after(
        shutdown: CancellationToken,
        idle: Duration,
    ) -> (Ws, Ws, tokio::task::JoinHandle<()>) {
        let (client, gateway_client) = tokio::io::duplex(4096);
        let (gateway_upstream, upstream) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let gateway_client =
            WebSocketStream::from_raw_socket(gateway_client, Role::Server, None).await;
        let gateway_upstream =
            WebSocketStream::from_raw_socket(gateway_upstream, Role::Client, None).await;
        let upstream = WebSocketStream::from_raw_socket(upstream, Role::Server, None).await;
        let task = tokio::spawn(Gateway::relay_websocket(
            gateway_client,
            gateway_upstream,
            shutdown,
            idle,
        ));
        (client, upstream, task)
    }

    async fn next(ws: &mut Ws) -> Message {
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("a frame in time")
            .expect("open")
            .expect("valid")
    }

    #[tokio::test]
    async fn frames_cross_both_ways_until_a_peer_closes() {
        let (mut client, mut upstream, task) = relay(CancellationToken::new()).await;
        client.send(Message::text("ping")).await.unwrap();
        assert_eq!(next(&mut upstream).await, Message::text("ping"));
        upstream.send(Message::binary(vec![1, 2, 3])).await.unwrap();
        assert_eq!(next(&mut client).await, Message::binary(vec![1, 2, 3]));

        client.close(None).await.unwrap();
        assert!(next(&mut upstream).await.is_close());
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the relay ends")
            .unwrap();
    }

    /// The next frame other than a pong: the upstream answers a relayed ping.
    async fn next_not_pong(ws: &mut Ws) -> Message {
        loop {
            let frame = next(ws).await;
            if !matches!(frame, Message::Pong(_)) {
                return frame;
            }
        }
    }

    #[tokio::test]
    async fn an_idle_relay_is_closed_and_traffic_keeps_it_open() {
        let idle = Duration::from_millis(300);
        let (mut client, mut upstream, task) =
            relay_idle_after(CancellationToken::new(), idle).await;

        // Frames in either direction, more often than the idle time.
        for i in 0..8 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if i % 2 == 0 {
                client.send(Message::Ping(vec![].into())).await.unwrap();
                assert!(matches!(next(&mut upstream).await, Message::Ping(_)));
            } else {
                upstream.send(Message::text("tick")).await.unwrap();
                assert_eq!(next_not_pong(&mut client).await, Message::text("tick"));
            }
        }
        assert!(!task.is_finished(), "traffic kept the relay open");

        for peer in [&mut client, &mut upstream] {
            match next_not_pong(peer).await {
                Message::Close(Some(frame)) => assert_eq!(frame.code, CloseCode::Away),
                other => panic!("expected Close 1001, got {other:?}"),
            }
        }
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the relay ends")
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_sends_going_away_to_both_peers() {
        let shutdown = CancellationToken::new();
        let (mut client, mut upstream, task) = relay(shutdown.clone()).await;
        client.send(Message::text("before")).await.unwrap();
        assert_eq!(next(&mut upstream).await, Message::text("before"));

        shutdown.cancel();
        for peer in [&mut client, &mut upstream] {
            match next(peer).await {
                Message::Close(Some(frame)) => assert_eq!(frame.code, CloseCode::Away),
                other => panic!("expected Close 1001, got {other:?}"),
            }
        }
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the relay ends")
            .unwrap();
    }
}

#[cfg(test)]
mod peer_tests {
    use super::canonical_peer;
    use std::net::SocketAddr;

    #[test]
    fn an_ipv4_mapped_peer_is_reported_as_ipv4() {
        let mapped: SocketAddr = "[::ffff:10.0.0.1]:4242".parse().unwrap();
        assert_eq!(canonical_peer(mapped), "10.0.0.1:4242".parse().unwrap());
        let v6: SocketAddr = "[2001:db8::1]:4242".parse().unwrap();
        assert_eq!(canonical_peer(v6), v6);
    }
}

#[cfg(test)]
mod listener_checks_tests {
    use super::{
        open_file_limit_too_low, parse_max_connections, plaintext_off_loopback,
        DEFAULT_MAX_CONNECTIONS,
    };
    use std::net::SocketAddr;

    #[test]
    fn the_connection_cap_is_a_count_a_semaphore_holds() {
        assert_eq!(parse_max_connections("1"), Ok(1));
        assert_eq!(parse_max_connections("250000"), Ok(250_000));
        assert_eq!(parse_max_connections(""), Ok(DEFAULT_MAX_CONNECTIONS));
        let max = tokio::sync::Semaphore::MAX_PERMITS;
        assert_eq!(parse_max_connections(&max.to_string()), Ok(max));
        for refused in [
            "0",
            "-1",
            "ten",
            "1.5",
            &(max + 1).to_string(),
            "18446744073709551616",
        ] {
            assert!(parse_max_connections(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn plaintext_is_flagged_off_loopback_only() {
        for addr in [
            "0.0.0.0:8080",
            "[::]:8080",
            "10.1.2.3:8080",
            "[2001:db8::1]:443",
        ] {
            let addr: SocketAddr = addr.parse().unwrap();
            assert!(plaintext_off_loopback(addr, false), "{addr}");
            assert!(!plaintext_off_loopback(addr, true), "{addr} with TLS");
        }
        for addr in [
            "127.0.0.1:8080",
            "127.10.0.1:8080",
            "[::1]:8080",
            "[::ffff:127.0.0.1]:8080",
        ] {
            let addr: SocketAddr = addr.parse().unwrap();
            assert!(!plaintext_off_loopback(addr, false), "{addr}");
        }
    }

    #[test]
    fn the_open_file_limit_must_exceed_the_connection_cap() {
        assert!(open_file_limit_too_low(10_000, Some(1024)));
        assert!(open_file_limit_too_low(1024, Some(1024)));
        assert!(!open_file_limit_too_low(1024, Some(1025)));
        assert!(!open_file_limit_too_low(10_000, None));
    }
}

#[cfg(test)]
mod artifact_version_tests {
    use super::check_artifact_version;

    #[test]
    fn accepts_the_version_this_build_writes() {
        assert!(check_artifact_version(barbacane_compiler::ARTIFACT_VERSION).is_ok());
    }

    #[test]
    fn accepts_every_version_from_the_oldest_this_major_reads() {
        for version in
            barbacane_compiler::OLDEST_ARTIFACT_VERSION..=barbacane_compiler::ARTIFACT_VERSION
        {
            assert!(check_artifact_version(version).is_ok(), "{version}");
        }
    }

    #[test]
    fn refuses_an_older_version_and_says_to_recompile() {
        let err = check_artifact_version(barbacane_compiler::OLDEST_ARTIFACT_VERSION - 1)
            .expect_err("an older format must be refused");
        assert!(
            err.contains("Recompile"),
            "the message must say what to do: {err}"
        );
        assert!(
            err.contains(&barbacane_compiler::OLDEST_ARTIFACT_VERSION.to_string()),
            "and which versions this build reads: {err}"
        );
    }

    #[test]
    fn refuses_a_newer_version_and_says_to_upgrade() {
        let err = check_artifact_version(barbacane_compiler::ARTIFACT_VERSION + 1)
            .expect_err("a newer format must be refused");
        assert!(err.contains("Upgrade the data plane"), "{err}");
    }
}

/// A request's headers by name, for validation and plugins. A repeated header
/// keeps its last value, except `cookie`, whose values are joined with `; ` as
/// an HTTP/2 client may split them (RFC 9113 §8.2.3). Values that are not
/// visible ASCII are left out.
fn request_header_map(headers: &hyper::HeaderMap) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::with_capacity(headers.len());
    for (name, value) in headers {
        let Ok(value) = value.to_str() else { continue };
        match map.get_mut(name.as_str()) {
            Some(existing) if name == hyper::header::COOKIE => {
                existing.push_str("; ");
                existing.push_str(value);
            }
            _ => {
                map.insert(name.as_str().to_string(), value.to_string());
            }
        }
    }
    map
}

/// Keep only the request headers an operation admits.
///
/// The baseline, plus what the spec declares for this operation and what its
/// middleware configuration tells a plugin to read. Everything else is dropped
/// before any plugin sees it, so an upstream receives what the document
/// describes and nothing a caller added on top.
///
/// `x-auth-*` is the identity contract written by auth plugins and read by
/// acl and upstreams, so a client-supplied value is always dropped; otherwise a
/// caller could forge groups an auth plugin leaves unset. Every path that
/// builds a plugin request (dispatch and CORS preflight) goes through here.
///
/// The WAF is deliberately not filtered this way: it inspects what the client
/// actually sent, which is the whole point of it.
fn filter_request_headers(
    allowed: &[String],
    headers: &HashMap<String, String>,
    dev_mode: bool,
) -> (std::collections::BTreeMap<String, String>, u64) {
    let mut kept = std::collections::BTreeMap::new();
    let mut dropped = 0u64;

    for (name, value) in headers {
        let lowered = name.to_ascii_lowercase();
        // The identity namespace belongs to the auth plugins' output. A spec
        // cannot declare one (E1056), so this only restates what the allowlist
        // could never contain.
        if lowered.starts_with("x-auth-") {
            dropped += 1;
            let reason = "request header dropped: the x-auth-* namespace carries the identity \
                          the auth plugins establish and is never accepted from a client";
            if dev_mode {
                tracing::warn!(header = %lowered, "{reason}");
            } else {
                tracing::debug!(header = %lowered, "{reason}");
            }
            continue;
        }
        if is_baseline_request_header(&lowered) || allowed.iter().any(|a| a == &lowered) {
            kept.insert(name.clone(), value.clone());
            continue;
        }
        dropped += 1;
        if dev_mode {
            tracing::warn!(
                header = %lowered,
                "request header dropped: the operation does not declare it. Declare it as a \
                 parameter, or as the security scheme that carries it, for it to reach the upstream"
            );
        } else {
            tracing::debug!(header = %lowered, "request header dropped");
        }
    }

    (kept, dropped)
}

/// Request headers every operation accepts, whatever its spec declares.
///
/// Message framing and negotiation, CORS, tracing, the WebSocket handshake, and
/// the proxy chain as it arrives. `authorization` is deliberately absent: it is
/// forwarded because an operation's security scheme says the operation is
/// authenticated, and for no other reason.
const BASELINE_REQUEST_HEADERS: &[&str] = &[
    // Message framing and content negotiation
    "host",
    "content-type",
    "content-length",
    "content-encoding",
    "transfer-encoding",
    "accept",
    "accept-encoding",
    "accept-language",
    "accept-charset",
    "user-agent",
    "range",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    "if-range",
    "cache-control",
    "pragma",
    "expect",
    // CORS
    "origin",
    "access-control-request-method",
    "access-control-request-headers",
    // Browser request metadata, sent automatically and describing the request
    // rather than the caller. `user-agent`, `accept-language` and `origin`
    // above are the same class. The fetch-metadata set matters most: an
    // upstream uses it to reject cross-site requests, so dropping it silently
    // removes a defence the upstream believes it has.
    "referer",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-dest",
    "sec-fetch-user",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "dnt",
    "sec-gpc",
    "priority",
    // Tracing and correlation
    "traceparent",
    "tracestate",
    "x-request-id",
    // WebSocket handshake
    "upgrade",
    "connection",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
    // Proxy chain: `x-forwarded-for`, `x-real-ip` and `forwarded` as received,
    // their trust settled by `--trusted-proxies`, not by this list;
    // `x-forwarded-proto` and `x-forwarded-host` as `resolve_forwarded` sets them.
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-real-ip",
    "forwarded",
];

/// `true` when the baseline admits this header.
fn is_baseline_request_header(name: &str) -> bool {
    BASELINE_REQUEST_HEADERS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod request_header_map_tests {
    use super::request_header_map;

    #[test]
    fn split_cookie_headers_are_joined() {
        let mut headers = hyper::HeaderMap::new();
        headers.append("cookie", "a=1".parse().unwrap());
        headers.append("cookie", "b=2".parse().unwrap());
        headers.append("x-tag", "first".parse().unwrap());
        headers.append("x-tag", "last".parse().unwrap());

        let map = request_header_map(&headers);

        assert_eq!(map["cookie"], "a=1; b=2");
        assert_eq!(map["x-tag"], "last");
    }
}

#[cfg(test)]
mod introspection_tests {
    use super::Introspection;
    use std::collections::HashMap;

    #[test]
    fn the_served_specs_are_built_at_load() {
        let specs = HashMap::from([
            (
                "api.yaml".to_string(),
                "openapi: 3.1.0\ninfo: {title: A, version: '1'}\nx-barbacane-middlewares: []\npaths: {}\n"
                    .to_string(),
            ),
            ("broken.yaml".to_string(), "openapi: [unclosed".to_string()),
        ]);
        let built = Introspection::new(&specs);

        let index: serde_json::Value = serde_json::from_slice(&built.index).unwrap();
        assert_eq!(index["openapi"]["count"], 1);
        let api = built.files["api.yaml"].as_ref().expect("parsed");
        let json: serde_json::Value = serde_json::from_slice(&api.json).unwrap();
        assert_eq!(json["info"]["title"], "A");
        assert!(json.get("x-barbacane-middlewares").is_none(), "stripped");
        assert!(String::from_utf8_lossy(&api.yaml).contains("title: A"));
        assert!(built.files["broken.yaml"].is_none());
        assert!(built.merged_openapi.is_some());
        assert!(built.merged_asyncapi.is_none());
    }
}

#[cfg(test)]
mod request_header_filter_tests {
    use super::filter_request_headers;
    use std::collections::HashMap;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn allow(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The baseline travels on every operation, and a header the operation
    /// admits travels with it. Anything else does not reach a plugin.
    #[test]
    fn keeps_the_baseline_and_what_the_operation_admits() {
        let (kept, dropped) = filter_request_headers(
            &allow(&["x-tenant-id"]),
            &headers(&[
                ("content-type", "application/json"),
                ("accept", "*/*"),
                ("x-request-id", "r1"),
                ("x-tenant-id", "acme"),
                ("x-debug-mode", "on"),
                ("x-internal-flag", "1"),
            ]),
            false,
        );
        assert!(kept.contains_key("content-type"));
        assert!(kept.contains_key("accept"));
        assert!(kept.contains_key("x-request-id"));
        assert!(
            kept.contains_key("x-tenant-id"),
            "the operation declares it"
        );
        assert!(!kept.contains_key("x-debug-mode"), "undeclared");
        assert!(!kept.contains_key("x-internal-flag"), "undeclared");
        assert_eq!(dropped, 2);
    }

    /// `authorization` is not in the baseline. It reaches the upstream because
    /// a security scheme put it in the operation's list, and not otherwise.
    #[test]
    fn authorization_travels_only_when_the_operation_admits_it() {
        let sent = headers(&[("authorization", "Bearer t")]);

        let (anonymous, dropped) = filter_request_headers(&[], &sent, false);
        assert!(
            !anonymous.contains_key("authorization"),
            "an operation declaring no security takes no credential"
        );
        assert_eq!(dropped, 1);

        let (authenticated, dropped) =
            filter_request_headers(&allow(&["authorization"]), &sent, false);
        assert_eq!(
            authenticated.get("authorization").map(String::as_str),
            Some("Bearer t")
        );
        assert_eq!(dropped, 0);
    }

    /// The identity namespace is the auth plugins' output. A client may not
    /// supply it, whatever the case, and no allowlist can admit it.
    #[test]
    fn client_supplied_identity_headers_are_always_dropped() {
        let (kept, dropped) = filter_request_headers(
            // Even were one to appear in the list, which E1056 prevents.
            &allow(&["x-auth-consumer"]),
            &headers(&[
                ("x-auth-consumer", "admin"),
                ("X-Auth-Consumer-Groups", "admin"),
                ("x-auth-claims", "{}"),
                ("X-AUTH-DN", "cn=admin"),
                ("content-type", "application/json"),
            ]),
            false,
        );
        assert!(kept
            .keys()
            .all(|k| !k.to_ascii_lowercase().starts_with("x-auth-")));
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped, 4);
    }

    /// Only that exact prefix is reserved; a header that merely starts with
    /// similar text is an ordinary one, admitted if the operation declares it.
    #[test]
    fn only_the_x_auth_dash_prefix_is_reserved() {
        let (kept, _) = filter_request_headers(
            &allow(&["x-authorization", "x-auth", "x-authz-mode"]),
            &headers(&[
                ("x-authorization", "custom"),
                ("x-auth", "bare"),
                ("x-authz-mode", "strict"),
            ]),
            false,
        );
        assert_eq!(kept.len(), 3);
    }

    /// A browser sends these on an ordinary navigation without being asked, so
    /// an operation cannot reasonably be expected to declare them. Before they
    /// were in the baseline the dev log named eight of them on every request,
    /// which buried the headers an author actually had to act on.
    #[test]
    fn browser_metadata_travels_without_being_declared() {
        let sent = headers(&[
            ("Referer", "https://example.test/app"),
            ("Sec-Fetch-Site", "same-origin"),
            ("Sec-Fetch-Mode", "cors"),
            ("Sec-Fetch-Dest", "empty"),
            ("Sec-CH-UA", "\"Chromium\";v=\"140\""),
            ("Sec-CH-UA-Mobile", "?0"),
            ("Sec-CH-UA-Platform", "\"macOS\""),
            ("DNT", "1"),
        ]);
        let (kept, dropped) = filter_request_headers(&[], &sent, false);
        assert_eq!(dropped, 0, "kept: {:?}", kept.keys().collect::<Vec<_>>());
        assert_eq!(kept.len(), sent.len());
    }

    /// HTTP matches header names without regard to case, so the list must too,
    /// or a caller could evade it by changing the spelling.
    #[test]
    fn matching_ignores_case_in_both_directions() {
        let (kept, dropped) = filter_request_headers(
            &allow(&["x-tenant-id"]),
            &headers(&[("X-Tenant-Id", "acme"), ("Content-Type", "text/plain")]),
            false,
        );
        assert_eq!(kept.len(), 2, "kept: {:?}", kept.keys().collect::<Vec<_>>());
        assert_eq!(dropped, 0);
        // The value travels under the name the client used.
        assert_eq!(kept.get("X-Tenant-Id").map(String::as_str), Some("acme"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_response_from_plugin_skips_invalid_headers_without_panicking() {
        use std::collections::BTreeMap;
        // An untrusted plugin returns headers with an illegal name, an illegal
        // value (embedded newline), and framing headers — none may panic the
        // task, and the invalid/framing ones must be dropped.
        let mut headers = BTreeMap::new();
        headers.insert("x-ok".to_string(), "fine".to_string());
        headers.insert("bad name".to_string(), "1".to_string()); // space in name
        headers.insert("".to_string(), "empty-name".to_string()); // empty name
        headers.insert("x-inject".to_string(), "a\r\nb".to_string()); // CRLF in value
        headers.insert("content-length".to_string(), "999".to_string()); // framing
        let plugin_response = barbacane_wasm::Response::new(200)
            .with_headers(headers)
            .with_body(b"hello".to_vec());

        let resp = Gateway::build_response_from_plugin(&plugin_response);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("x-ok").unwrap(), "fine");
        assert!(resp.headers().get("bad name").is_none());
        assert!(resp.headers().get("x-inject").is_none());
        // Framing header is dropped (hyper recomputes content-length).
        assert!(resp.headers().get("content-length").is_none());
    }

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

#[cfg(test)]
mod request_id_tests {
    use super::*;

    #[test]
    fn a_short_visible_ascii_request_id_is_kept() {
        assert_eq!(client_request_id("req-42"), Some("req-42".into()));
        assert_eq!(
            client_request_id(&"a".repeat(MAX_REQUEST_ID_BYTES)),
            Some("a".repeat(MAX_REQUEST_ID_BYTES))
        );
    }

    #[test]
    fn a_long_empty_or_non_printable_request_id_is_replaced() {
        assert_eq!(client_request_id(""), None);
        assert_eq!(
            client_request_id(&"a".repeat(MAX_REQUEST_ID_BYTES + 1)),
            None
        );
        assert_eq!(client_request_id("with space"), None);
        assert_eq!(client_request_id("tab\there"), None);
        assert_eq!(client_request_id("caf\u{e9}"), None);
    }

    #[test]
    fn a_well_formed_traceparent_gives_its_trace_id() {
        assert_eq!(
            trace_id_from_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".into())
        );
    }

    #[test]
    fn a_malformed_traceparent_gives_no_trace_id() {
        for value in [
            "",
            "00-zzzz-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-and-more-than-fits",
        ] {
            assert_eq!(trace_id_from_traceparent(value), None, "{value:?}");
        }
    }
}

#[cfg(test)]
mod trust_boundary_tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_handshake_is_a_get_with_upgrade_connection_and_key() {
        let handshake = headers(&[
            ("upgrade", "websocket"),
            ("connection", "keep-alive, Upgrade"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        assert!(is_websocket_handshake("GET", &handshake));
        assert!(!is_websocket_handshake("POST", &handshake));
        assert!(!is_websocket_handshake(
            "GET",
            &headers(&[("upgrade", "websocket"), ("connection", "upgrade")])
        ));
        assert!(!is_websocket_handshake(
            "GET",
            &headers(&[("upgrade", "websocket"), ("sec-websocket-key", "x")])
        ));
    }

    #[test]
    fn a_plaintext_control_plane_link_is_allowed_to_loopback_only() {
        assert!(control_plane_link_allowed("wss://control:9090/ws/data-plane", false).is_ok());
        assert!(control_plane_link_allowed("ws://localhost:9090/ws/data-plane", false).is_ok());
        assert!(control_plane_link_allowed("ws://127.0.0.1:9090/ws/data-plane", false).is_ok());
        assert!(control_plane_link_allowed("ws://[::1]:9090/ws/data-plane", false).is_ok());
        assert!(control_plane_link_allowed("ws://control:9090/ws/data-plane", false).is_err());
        assert!(control_plane_link_allowed("ws://10.0.0.5:9090/ws/data-plane", false).is_err());
        assert!(control_plane_link_allowed("ws://control:9090/ws/data-plane", true).is_ok());
    }
}

#[cfg(test)]
mod help_tests {
    use super::*;

    /// Arguments read from these variables carry credentials.
    fn is_credential(variable: &str) -> bool {
        ["API_KEY", "TOKEN", "PASSWORD", "SECRET", "DATABASE_URL"]
            .iter()
            .any(|word| variable.contains(word))
    }

    /// The arguments of `command` and its subcommands whose help would print
    /// a credential from the environment.
    fn credentials_shown(command: &clap::Command, path: &str, shown: &mut Vec<String>) {
        for arg in command.get_arguments() {
            let variable = arg.get_env().and_then(|v| v.to_str()).unwrap_or_default();
            if is_credential(variable) && !arg.is_hide_env_values_set() {
                shown.push(format!("{path} --{} ({variable})", arg.get_id()));
            }
        }
        for sub in command.get_subcommands() {
            credentials_shown(sub, &format!("{path} {}", sub.get_name()), shown);
        }
    }

    #[test]
    fn help_prints_no_credential_from_the_environment() {
        let command = Cli::command().subcommand(barbacane_lib::control::command());
        let mut shown = Vec::new();
        credentials_shown(&command, "barbacane", &mut shown);
        assert!(shown.is_empty(), "{shown:?}");
    }
}
