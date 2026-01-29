//! Barbacane data plane.
//!
//! Loads a compiled `.bca` artifact at startup and processes HTTP requests
//! through the pipeline: route → validate → middleware → dispatch → respond.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use bytes::Bytes;
use clap::Parser;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use std::collections::HashMap;

use barbacane_compiler::{load_manifest, load_routes, load_specs, CompiledOperation, Manifest};
use barbacane_router::{RouteEntry, RouteMatch, Router};

#[derive(Parser, Debug)]
#[command(name = "barbacane", about = "Barbacane API gateway data plane", version)]
struct Cli {
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
}

/// Shared gateway state.
struct Gateway {
    manifest: Manifest,
    router: Router,
    operations: Vec<CompiledOperation>,
    /// Source specs embedded in the artifact (filename -> content).
    specs: HashMap<String, String>,
    dev_mode: bool,
}

impl Gateway {
    /// Load a gateway from a .bca artifact.
    fn load(artifact_path: &Path, dev_mode: bool) -> Result<Self, String> {
        let manifest = load_manifest(artifact_path)
            .map_err(|e| format!("failed to load manifest: {}", e))?;

        let routes = load_routes(artifact_path)
            .map_err(|e| format!("failed to load routes: {}", e))?;

        let specs = load_specs(artifact_path)
            .map_err(|e| format!("failed to load specs: {}", e))?;

        let mut router = Router::new();
        for op in &routes.operations {
            router.insert(
                &op.path,
                &op.method,
                RouteEntry {
                    operation_index: op.index,
                },
            );
        }

        Ok(Gateway {
            manifest,
            router,
            operations: routes.operations,
            specs,
            dev_mode,
        })
    }

    /// Handle an incoming HTTP request.
    async fn handle_request(
        &self,
        req: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        let path = req.uri().path();
        let method = req.method();

        // Reserved /__barbacane/* endpoints
        if path.starts_with("/__barbacane/") {
            return Ok(self.handle_barbacane_endpoint(path, method));
        }

        // Route lookup
        let method_str = method.as_str();
        match self.router.lookup(path, method_str) {
            RouteMatch::Found { entry, params } => {
                let operation = &self.operations[entry.operation_index];
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
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // Load the artifact
    let artifact_path = Path::new(&cli.artifact);
    if !artifact_path.exists() {
        eprintln!("error: artifact not found: {}", cli.artifact);
        return ExitCode::from(1);
    }

    let gateway = match Gateway::load(artifact_path, cli.dev) {
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
    let addr: SocketAddr = match cli.listen.parse() {
        Ok(a) => a,
        Err(_) => {
            eprintln!("error: invalid listen address: {}", cli.listen);
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
