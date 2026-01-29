//! Barbacane API gateway.
//!
//! Compiles OpenAPI specs into artifacts and runs the data plane server.

use std::convert::Infallible;
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
use tokio::net::TcpListener;

use std::collections::HashMap;

use barbacane_compiler::{compile, load_manifest, load_routes, load_specs, CompiledOperation, Manifest};
use barbacane_router::{RouteEntry, RouteMatch, Router};
use barbacane_validator::{OperationValidator, ProblemDetails, RequestLimits};

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
}

impl Gateway {
    /// Load a gateway from a .bca artifact.
    fn load(artifact_path: &Path, dev_mode: bool, limits: RequestLimits) -> Result<Self, String> {
        let manifest = load_manifest(artifact_path)
            .map_err(|e| format!("failed to load manifest: {}", e))?;

        let routes = load_routes(artifact_path)
            .map_err(|e| format!("failed to load routes: {}", e))?;

        let specs = load_specs(artifact_path)
            .map_err(|e| format!("failed to load specs: {}", e))?;

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
            let validator = OperationValidator::new(
                &op.parameters,
                op.request_body.as_ref(),
            );
            validators.push(validator);
        }

        Ok(Gateway {
            manifest,
            router,
            operations: routes.operations,
            validators,
            specs,
            limits,
            dev_mode,
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
            .filter_map(|(k, v)| {
                Some((k.as_str().to_string(), v.to_str().ok()?.to_string()))
            })
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

                self.dispatch(operation, params).await
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
        _params: Vec<(String, String)>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let dispatch = &operation.dispatch;

        // For M1, only mock dispatcher is implemented
        match dispatch.name.as_str() {
            "mock" => self.dispatch_mock(&dispatch.config),
            _ => {
                // Unknown dispatcher
                let body = if self.dev_mode {
                    format!(
                        r#"{{"error":"unknown dispatcher","dispatcher":"{}"}}"#,
                        dispatch.name
                    )
                } else {
                    r#"{"error":"internal server error"}"#.to_string()
                };

                Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap())
            }
        }
    }

    /// Mock dispatcher: returns configured status and body.
    fn dispatch_mock(
        &self,
        config: &serde_json::Value,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let status_code = config
            .get("status")
            .and_then(|v| v.as_u64())
            .map(|n| n as u16)
            .unwrap_or(200);

        let body = config
            .get("body")
            .map(|v| {
                if v.is_string() {
                    v.as_str().unwrap().to_string()
                } else {
                    v.to_string()
                }
            })
            .unwrap_or_default();

        let status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);

        Ok(Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap())
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
                                        idx + 1, op.method, op.path
                                    ),
                                    location: Some(format!("{}:{} {}", spec_path, op.path, op.method)),
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
                eprintln!("✓ {} is valid (with {} warning(s))", result.file, result.warnings.len());
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
        eprintln!("validated {} spec(s): {} valid, {} invalid",
            total, valid_count, total - valid_count);
    }

    if has_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Run the compile command.
fn run_compile(specs: &[String], output: &str) -> ExitCode {
    let spec_paths: Vec<&Path> = specs.iter().map(|s| Path::new(s)).collect();
    let output_path = Path::new(output);

    // Check that all spec files exist
    for path in &spec_paths {
        if !path.exists() {
            eprintln!("error: spec file not found: {}", path.display());
            return ExitCode::from(1);
        }
    }

    match compile(&spec_paths, output_path) {
        Ok(manifest) => {
            eprintln!("compiled {} spec(s) to {} ({} routes)", specs.len(), output, manifest.routes_count);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: compilation failed: {}", e);
            ExitCode::from(1)
        }
    }
}

/// Run the serve command.
async fn run_serve(artifact: &str, listen: &str, dev: bool, limits: RequestLimits) -> ExitCode {
    let artifact_path = Path::new(artifact);
    if !artifact_path.exists() {
        eprintln!("error: artifact not found: {}", artifact);
        return ExitCode::from(1);
    }

    let gateway = match Gateway::load(artifact_path, dev, limits) {
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

    eprintln!("barbacane: listening on {}", addr);

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
        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let gateway = Arc::clone(&gateway);
                async move { gateway.handle_request(req).await }
            });

            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                eprintln!("error: connection error: {}", e);
            }
        });
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Compile { spec, output } => run_compile(&spec, &output),
        Commands::Validate { spec, format } => run_validate(&spec, &format),
        Commands::Serve {
            artifact,
            listen,
            dev,
            max_body_size,
            max_headers,
            max_header_size,
            max_uri_length,
            ..
        } => {
            let limits = RequestLimits {
                max_body_size,
                max_headers,
                max_header_size,
                max_uri_length,
            };
            run_serve(&artifact, &listen, dev, limits).await
        }
    }
}
