//! Barbacane API gateway.
//!
//! Compiles OpenAPI specs into artifacts and runs the data plane server.

use std::convert::Infallible;
use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use bytes::Bytes;
use clap::{Parser, Subcommand};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use std::collections::HashMap;

use barbacane_compiler::{
    compile, compile_with_manifest, load_manifest, load_plugins, load_routes, load_specs,
    CompileOptions, CompiledOperation, Manifest, ProjectManifest,
};
use barbacane_router::{RouteEntry, RouteMatch, Router};
use barbacane_validator::{OperationValidator, ProblemDetails, RequestLimits};
use barbacane_wasm::{HttpClient, HttpClientConfig, InstancePool, PluginLimits, WasmEngine};

#[derive(Parser, Debug)]
#[command(name = "barbacane", about = "Barbacane API gateway", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Compile OpenAPI spec(s) into a .bca artifact.
    Compile {
        /// Input spec file(s) (YAML or JSON).
        #[arg(short, long, required = true, num_args = 1..)]
        spec: Vec<String>,

        /// Output artifact path.
        #[arg(short, long)]
        output: String,

        /// Path to barbacane.yaml manifest (required for plugin resolution).
        #[arg(short, long)]
        manifest: Option<String>,

        /// Allow plaintext HTTP upstream URLs (development only).
        #[arg(long)]
        allow_plaintext: bool,
    },

    /// Validate OpenAPI spec(s) without compiling.
    ///
    /// Checks for spec validity (E1001-E1004) and extension validity (E1010-E1015).
    /// Does not resolve plugins or produce an artifact.
    Validate {
        /// Input spec file(s) (YAML or JSON).
        #[arg(short, long, required = true, num_args = 1..)]
        spec: Vec<String>,

        /// Output format (text or json).
        #[arg(long, default_value = "text")]
        format: String,
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

        /// Log level.
        #[arg(long, default_value = "info")]
        log_level: String,

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
    },
}

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
    /// WASM engine for plugin execution (kept for future plugin compilation).
    #[allow(dead_code)]
    wasm_engine: Arc<WasmEngine>,
    /// Plugin instance pool.
    plugin_pool: Arc<InstancePool>,
    /// Plugin resource limits (kept for future dynamic limit adjustment).
    #[allow(dead_code)]
    plugin_limits: PluginLimits,
    /// HTTP client for plugins making outbound calls (kept alive for pool lifetime).
    #[allow(dead_code)]
    http_client: Arc<HttpClient>,
}

impl Gateway {
    /// Load a gateway from a .bca artifact.
    fn load(
        artifact_path: &Path,
        dev_mode: bool,
        limits: RequestLimits,
        allow_plaintext_upstream: bool,
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

        // Create plugin instance pool with HTTP client for outbound calls
        let plugin_pool = InstancePool::with_http_client(
            wasm_engine.clone(),
            plugin_limits.clone(),
            http_client.clone(),
        );

        // Load plugins from the artifact
        let bundled_plugins = load_plugins(artifact_path)
            .map_err(|e| format!("failed to load plugins from artifact: {}", e))?;

        if bundled_plugins.is_empty() {
            tracing::warn!("no plugins bundled in artifact - ensure barbacane.yaml manifest was used during compilation");
        }

        for (name, (version, wasm_bytes)) in bundled_plugins {
            let module = wasm_engine
                .compile(&wasm_bytes, name.clone(), version.clone())
                .map_err(|e| format!("failed to compile plugin '{}': {}", name, e))?;
            plugin_pool.register_module(module);

            if dev_mode {
                tracing::debug!(plugin = %name, version = %version, "loaded plugin from artifact");
            }
        }

        let mut router = Router::new();
        let mut validators = Vec::new();

        for op in &routes.operations {
            router.insert(
                &op.path,
                &op.method,
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

        Ok(Gateway {
            manifest,
            router,
            operations: routes.operations,
            validators,
            specs,
            limits,
            dev_mode,
            wasm_engine,
            plugin_pool: Arc::new(plugin_pool),
            plugin_limits,
            http_client,
        })
    }

    /// Handle an incoming HTTP request.
    async fn handle_request(
        &self,
        req: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let uri_string = req.uri().to_string();
        let path = req.uri().path().to_string();
        let query_string = req.uri().query().map(|s| s.to_string());
        let method = req.method().clone();

        // Check URI length limit early
        if let Err(e) = self.limits.validate_uri(&uri_string) {
            return Ok(self.validation_error_response(&[e]));
        }

        // Reserved /__barbacane/* endpoints (skip other limits for internal endpoints)
        if path.starts_with("/__barbacane/") {
            return Ok(self.handle_barbacane_endpoint(&path, &method));
        }

        // Extract headers for validation
        let headers: HashMap<String, String> = req
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();

        // Check header limits
        if let Err(e) = self.limits.validate_headers(&headers) {
            return Ok(self.validation_error_response(&[e]));
        }

        // Check content-length before reading body (if present)
        if let Some(content_length) = headers.get("content-length") {
            if let Ok(len) = content_length.parse::<usize>() {
                if let Err(e) = self.limits.validate_body_size(len) {
                    return Ok(self.validation_error_response(&[e]));
                }
            }
        }

        // Route lookup
        let method_str = method.as_str();
        match self.router.lookup(&path, method_str) {
            RouteMatch::Found { entry, params } => {
                let operation = &self.operations[entry.operation_index];
                let validator = &self.validators[entry.operation_index];

                let content_type = headers.get("content-type").map(|s| s.as_str());

                // Collect body bytes
                let body_bytes = match req.collect().await {
                    Ok(collected) => collected.to_bytes(),
                    Err(_) => {
                        return Ok(self.bad_request_response("failed to read request body"));
                    }
                };

                // Validate actual body size (in case content-length was missing or wrong)
                if let Err(e) = self.limits.validate_body_size(body_bytes.len()) {
                    return Ok(self.validation_error_response(&[e]));
                }

                // Validate request against OpenAPI spec
                if let Err(errors) = validator.validate_request(
                    &params,
                    query_string.as_deref(),
                    &headers,
                    content_type,
                    &body_bytes,
                ) {
                    return Ok(self.validation_error_response(&errors));
                }

                self.dispatch(operation, params, &body_bytes, &headers)
                    .await
            }
            RouteMatch::MethodNotAllowed { allowed } => {
                Ok(self.method_not_allowed_response(allowed))
            }
            RouteMatch::NotFound => Ok(self.not_found_response()),
        }
    }

    /// Dispatch a request to the appropriate handler.
    async fn dispatch(
        &self,
        operation: &CompiledOperation,
        params: Vec<(String, String)>,
        request_body: &[u8],
        headers: &HashMap<String, String>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let dispatch = &operation.dispatch;

        // Log middleware chain (M3 feature - actual execution requires bundled plugins)
        if !operation.middlewares.is_empty() {
            tracing::debug!(
                middlewares = ?operation.middlewares.iter().map(|m| &m.name).collect::<Vec<_>>(),
                "middleware chain for operation (not yet executed - requires bundled plugins)"
            );
        }

        // All dispatchers must be WASM plugins loaded from the artifact
        if self.plugin_pool.has_plugin(&dispatch.name) {
            return self
                .dispatch_wasm_plugin(
                    &dispatch.name,
                    &dispatch.config,
                    operation,
                    params,
                    request_body,
                    headers,
                )
                .await;
        }

        // Unknown dispatcher - not bundled in the artifact
        let detail = if self.dev_mode {
            Some(format!(
                "unknown dispatcher '{}' - not found in artifact plugins",
                dispatch.name
            ))
        } else {
            None
        };

        Ok(self.internal_error_response(detail.as_deref()))
    }

    /// Dispatch via a WASM plugin.
    async fn dispatch_wasm_plugin(
        &self,
        plugin_name: &str,
        config: &serde_json::Value,
        operation: &CompiledOperation,
        params: Vec<(String, String)>,
        request_body: &[u8],
        headers: &HashMap<String, String>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
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
                let detail = if self.dev_mode {
                    Some(format!("failed to get plugin instance: {}", e))
                } else {
                    None
                };
                return Ok(self.internal_error_response(detail.as_deref()));
            }
        };

        // Build the Request object for the plugin (using BTreeMap for WASM compatibility)
        let path_params: std::collections::BTreeMap<String, String> = params.into_iter().collect();
        let headers_btree: std::collections::BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let plugin_request = barbacane_wasm::Request {
            method: operation.method.clone(),
            path: operation.path.clone(),
            query: None, // TODO: pass query string
            headers: headers_btree,
            body: if request_body.is_empty() {
                None
            } else {
                String::from_utf8(request_body.to_vec()).ok()
            },
            client_ip: "0.0.0.0".to_string(), // TODO: get actual client IP
            path_params,
        };

        // Serialize request for the plugin
        let request_json = match serde_json::to_vec(&plugin_request) {
            Ok(j) => j,
            Err(e) => {
                let detail = if self.dev_mode {
                    Some(format!("failed to serialize request: {}", e))
                } else {
                    None
                };
                return Ok(self.internal_error_response(detail.as_deref()));
            }
        };

        // Call the dispatch function
        if let Err(e) = instance.dispatch(&request_json) {
            let detail = if self.dev_mode {
                Some(format!("plugin dispatch failed: {}", e))
            } else {
                None
            };
            return Ok(self.internal_error_response(detail.as_deref()));
        }

        // Get the output
        let output = instance.take_output();
        if output.is_empty() {
            let detail = if self.dev_mode {
                Some("plugin returned empty output".to_string())
            } else {
                None
            };
            return Ok(self.internal_error_response(detail.as_deref()));
        }

        // Parse the response
        let plugin_response: barbacane_wasm::Response = match serde_json::from_slice(&output) {
            Ok(r) => r,
            Err(e) => {
                let detail = if self.dev_mode {
                    Some(format!("failed to parse plugin response: {}", e))
                } else {
                    None
                };
                return Ok(self.internal_error_response(detail.as_deref()));
            }
        };

        // Build HTTP response
        let status = StatusCode::from_u16(plugin_response.status).unwrap_or(StatusCode::OK);
        let mut builder = Response::builder().status(status);

        for (key, value) in &plugin_response.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        let body = plugin_response.body.unwrap_or_default();
        Ok(builder.body(Full::new(Bytes::from(body))).unwrap())
    }

    /// Handle reserved /__barbacane/* endpoints.
    fn handle_barbacane_endpoint(&self, path: &str, method: &Method) -> Response<Full<Bytes>> {
        if method != Method::GET {
            return self.method_not_allowed_response(vec!["GET".to_string()]);
        }

        match path {
            "/__barbacane/health" => self.health_response(),
            "/__barbacane/openapi" => self.openapi_response(),
            _ => {
                // Check for specific spec file: /__barbacane/openapi/{filename}
                if let Some(filename) = path.strip_prefix("/__barbacane/openapi/") {
                    self.spec_file_response(filename)
                } else {
                    self.not_found_response()
                }
            }
        }
    }

    /// Build the OpenAPI response (list of specs or single merged spec).
    fn openapi_response(&self) -> Response<Full<Bytes>> {
        // If there's exactly one spec, return it directly
        if self.specs.len() == 1 {
            let (filename, content) = self.specs.iter().next().unwrap();
            let content_type = if filename.ends_with(".json") {
                "application/json"
            } else {
                "text/yaml"
            };

            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", content_type)
                .body(Full::new(Bytes::from(content.clone())))
                .unwrap();
        }

        // Multiple specs: return a JSON index
        let spec_list: Vec<_> = self
            .specs
            .keys()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "url": format!("/__barbacane/openapi/{}", name),
                })
            })
            .collect();

        let body = serde_json::json!({
            "specs": spec_list,
            "count": self.specs.len(),
        });

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
            .unwrap()
    }

    /// Serve a specific spec file.
    fn spec_file_response(&self, filename: &str) -> Response<Full<Bytes>> {
        if let Some(content) = self.specs.get(filename) {
            let content_type = if filename.ends_with(".json") {
                "application/json"
            } else {
                "text/yaml"
            };

            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", content_type)
                .body(Full::new(Bytes::from(content.clone())))
                .unwrap()
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
            .unwrap()
    }

    /// Build a 404 Not Found response.
    fn not_found_response(&self) -> Response<Full<Bytes>> {
        let body = r#"{"error":"not found"}"#;

        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap()
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
            .unwrap()
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
            .unwrap()
    }

    /// Build a 400 validation error response (RFC 9457).
    fn validation_error_response(
        &self,
        errors: &[barbacane_validator::ValidationError2],
    ) -> Response<Full<Bytes>> {
        let problem = ProblemDetails::validation_error(errors, self.dev_mode);

        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("content-type", "application/problem+json")
            .body(Full::new(Bytes::from(problem.to_json())))
            .unwrap()
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
            .unwrap()
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
    let mut parsed_specs: Vec<(String, barbacane_spec_parser::ApiSpec)> = Vec::new();

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
        match barbacane_spec_parser::parse_spec_file(path) {
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

                // Check for unknown x-barbacane-* extensions (E1015 - warning)
                let known_extensions = [
                    "x-barbacane-dispatch",
                    "x-barbacane-middlewares",
                    "x-barbacane-ratelimit",
                    "x-barbacane-cache",
                    "x-barbacane-observability",
                ];

                for key in spec.extensions.keys() {
                    if !known_extensions.contains(&key.as_str()) {
                        warnings.push(ValidationIssue {
                            code: "E1015".to_string(),
                            message: format!("unknown extension: {}", key),
                            location: Some(spec_path.clone()),
                        });
                    }
                }

                for op in &spec.operations {
                    for key in op.extensions.keys() {
                        if !known_extensions.contains(&key.as_str()) {
                            warnings.push(ValidationIssue {
                                code: "E1015".to_string(),
                                message: format!("unknown extension: {}", key),
                                location: Some(format!("{}:{} {}", spec_path, op.path, op.method)),
                            });
                        }
                    }
                }

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
                    barbacane_spec_parser::ParseError::UnknownFormat => {
                        ("E1001".to_string(), e.to_string())
                    }
                    barbacane_spec_parser::ParseError::ParseError(_) => {
                        ("E1002".to_string(), e.to_string())
                    }
                    barbacane_spec_parser::ParseError::UnresolvedRef(_) => {
                        ("E1003".to_string(), e.to_string())
                    }
                    barbacane_spec_parser::ParseError::SchemaError(_) => {
                        ("E1004".to_string(), e.to_string())
                    }
                    barbacane_spec_parser::ParseError::Io(io_err) => {
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
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
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

/// Run the compile command.
fn run_compile(
    specs: &[String],
    output: &str,
    manifest_path: Option<&str>,
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

    let options = CompileOptions { allow_plaintext };

    let result = if let Some(manifest_file) = manifest_path {
        // Manifest-based compilation: validates plugins and bundles them
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

        compile_with_manifest(
            &spec_paths,
            &project_manifest,
            base_path,
            output_path,
            &options,
        )
    } else {
        // Legacy compilation without manifest (no plugin validation)
        compile(&spec_paths, output_path)
    };

    match result {
        Ok(manifest) => {
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
}

/// Load TLS certificates and create a rustls ServerConfig.
///
/// Configuration:
/// - TLS 1.2 minimum, TLS 1.3 preferred
/// - ALPN: h2, http/1.1
fn load_tls_config(config: &TlsConfig) -> Result<Arc<ServerConfig>, String> {
    // Load certificate chain
    let cert_file = File::open(&config.cert_path)
        .map_err(|e| format!("failed to open certificate file '{}': {}", config.cert_path, e))?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to parse certificate file '{}': {}", config.cert_path, e))?;

    if certs.is_empty() {
        return Err(format!(
            "no certificates found in '{}'",
            config.cert_path
        ));
    }

    // Load private key
    let key_file = File::open(&config.key_path)
        .map_err(|e| format!("failed to open key file '{}': {}", config.key_path, e))?;
    let mut key_reader = BufReader::new(key_file);
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| format!("failed to parse key file '{}': {}", config.key_path, e))?
        .ok_or_else(|| format!("no private key found in '{}'", config.key_path))?;

    // Build TLS config with modern settings
    // - TLS 1.2 minimum (via default provider)
    // - TLS 1.3 preferred (default behavior)
    // - ALPN: h2, http/1.1
    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("failed to build TLS config: {}", e))?;

    // Set ALPN protocols: prefer HTTP/2, fallback to HTTP/1.1
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(Arc::new(server_config))
}

/// Run the serve command.
async fn run_serve(
    artifact: &str,
    listen: &str,
    dev: bool,
    limits: RequestLimits,
    allow_plaintext_upstream: bool,
    tls_config: Option<TlsConfig>,
) -> ExitCode {
    let artifact_path = Path::new(artifact);
    if !artifact_path.exists() {
        eprintln!("error: artifact not found: {}", artifact);
        return ExitCode::from(1);
    }

    let gateway = match Gateway::load(artifact_path, dev, limits, allow_plaintext_upstream) {
        Ok(g) => Arc::new(g),
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::from(1);
        }
    };

    eprintln!(
        "barbacane: loaded {} route(s) from artifact",
        gateway.manifest.routes_count
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

    // Accept connections
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("error: accept failed: {}", e);
                continue;
            }
        };

        let gateway = Arc::clone(&gateway);
        let tls_acceptor = tls_acceptor.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let gateway = Arc::clone(&gateway);
                async move { gateway.handle_request(req).await }
            });

            if let Some(acceptor) = tls_acceptor {
                // TLS connection
                match acceptor.accept(stream).await {
                    Ok(tls_stream) => {
                        let io = TokioIo::new(tls_stream);
                        if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                            eprintln!("error: connection error: {}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: TLS handshake failed: {}", e);
                    }
                }
            } else {
                // Plain TCP connection
                let io = TokioIo::new(stream);
                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    eprintln!("error: connection error: {}", e);
                }
            }
        });
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
        } => run_compile(&spec, &output, manifest.as_deref(), allow_plaintext),
        Commands::Validate { spec, format } => run_validate(&spec, &format),
        Commands::Serve {
            artifact,
            listen,
            dev,
            max_body_size,
            max_headers,
            max_header_size,
            max_uri_length,
            allow_plaintext_upstream,
            tls_cert,
            tls_key,
            ..
        } => {
            // Validate TLS arguments
            let tls_config = match (tls_cert, tls_key) {
                (Some(cert), Some(key)) => Some(TlsConfig {
                    cert_path: cert,
                    key_path: key,
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
            run_serve(&artifact, &listen, dev, limits, allow_plaintext_upstream, tls_config).await
        }
    }
}
