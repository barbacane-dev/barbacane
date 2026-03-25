//! Admin API listener for operational endpoints.
//!
//! Serves health, metrics, and provenance on a dedicated port
//! separate from user traffic (ADR-0022).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::sync::watch;

use barbacane_compiler::Manifest;
use barbacane_telemetry::MetricsRegistry;

/// Shared state for the admin server.
pub struct AdminState {
    pub manifest: Arc<ArcSwap<Manifest>>,
    pub metrics: Arc<MetricsRegistry>,
    pub drift_detected: Arc<AtomicBool>,
    pub started_at: Instant,
}

/// Start the admin HTTP server.
///
/// Serves `/health`, `/metrics`, and `/provenance` on a dedicated port.
pub async fn start_admin_server(
    addr: SocketAddr,
    state: Arc<AdminState>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("admin: failed to bind to {}: {}", addr, e))?;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return Ok(());
                }
            }
            result = listener.accept() => {
                let (stream, _) = result.map_err(|e| format!("admin accept: {}", e))?;
                let state = state.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let state = state.clone();
                        async move { handle_request(req, &state) }
                    });
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        }
    }
}

fn handle_request(
    req: Request<hyper::body::Incoming>,
    state: &AdminState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path();
    let method = req.method();

    if method != Method::GET {
        return Ok(json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"error":"method not allowed"}"#,
        ));
    }

    let response = match path {
        "/health" => health_response(state),
        "/metrics" => metrics_response(state),
        "/provenance" => provenance_response(state),
        _ => json_response(StatusCode::NOT_FOUND, r#"{"error":"not found"}"#),
    };

    Ok(response)
}

fn health_response(state: &AdminState) -> Response<Full<Bytes>> {
    let manifest = state.manifest.load();
    let uptime_secs = state.started_at.elapsed().as_secs();

    let body = serde_json::json!({
        "status": "healthy",
        "artifact_version": manifest.barbacane_artifact_version,
        "compiler_version": manifest.compiler_version,
        "routes_count": manifest.routes_count,
        "uptime_secs": uptime_secs,
    });

    json_response(StatusCode::OK, &body.to_string())
}

fn metrics_response(state: &AdminState) -> Response<Full<Bytes>> {
    let body = barbacane_telemetry::prometheus::render_metrics(&state.metrics);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", barbacane_telemetry::PROMETHEUS_CONTENT_TYPE)
        .body(Full::new(Bytes::from(body)))
        .expect("valid response")
}

fn provenance_response(state: &AdminState) -> Response<Full<Bytes>> {
    let manifest = state.manifest.load();

    let body = serde_json::json!({
        "artifact_hash": manifest.artifact_hash,
        "compiled_at": manifest.compiled_at,
        "compiler_version": manifest.compiler_version,
        "artifact_version": manifest.barbacane_artifact_version,
        "provenance": manifest.provenance,
        "source_specs": manifest.source_specs.iter().map(|s| {
            serde_json::json!({
                "file": s.file,
                "sha256": s.sha256,
                "type": s.spec_type,
            })
        }).collect::<Vec<_>>(),
        "plugins": manifest.plugins.iter().map(|p| {
            serde_json::json!({
                "name": p.name,
                "version": p.version,
                "sha256": p.sha256,
            })
        }).collect::<Vec<_>>(),
        "drift_detected": state.drift_detected.load(Ordering::Relaxed),
    });

    json_response(StatusCode::OK, &body.to_string())
}

fn json_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("valid response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::collections::BTreeMap;

    async fn extract_body(response: Response<Full<Bytes>>) -> String {
        let collected = response.into_body().collect().await.expect("collect body");
        String::from_utf8(collected.to_bytes().to_vec()).expect("valid utf8")
    }

    fn test_manifest() -> Manifest {
        Manifest {
            barbacane_artifact_version: 2,
            compiled_at: "2026-03-01T00:00:00Z".to_string(),
            compiler_version: "0.2.1".to_string(),
            source_specs: vec![barbacane_compiler::SourceSpec {
                file: "petstore.yaml".to_string(),
                sha256: "abc123".to_string(),
                spec_type: "openapi".to_string(),
                version: "3.0.3".to_string(),
            }],
            routes_count: 5,
            checksums: BTreeMap::from([("routes.json".to_string(), "sha256:def456".to_string())]),
            plugins: vec![],
            artifact_hash: "sha256:combined123".to_string(),
            provenance: barbacane_compiler::Provenance {
                commit: Some("abc123def".to_string()),
                source: Some("ci/github-actions".to_string()),
            },
            mcp: barbacane_compiler::McpConfig::default(),
        }
    }

    fn test_state() -> Arc<AdminState> {
        Arc::new(AdminState {
            manifest: Arc::new(ArcSwap::new(Arc::new(test_manifest()))),
            metrics: Arc::new(MetricsRegistry::new()),
            drift_detected: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        })
    }

    #[test]
    fn test_health_returns_200() {
        let state = test_state();
        let response = health_response(&state);
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_provenance_response_contains_hash() {
        let state = test_state();
        let response = provenance_response(&state);
        assert_eq!(response.status(), StatusCode::OK);

        let body = extract_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert_eq!(json["artifact_hash"], "sha256:combined123");
        assert_eq!(json["provenance"]["commit"], "abc123def");
        assert_eq!(json["provenance"]["source"], "ci/github-actions");
        assert_eq!(json["drift_detected"], false);
        assert_eq!(json["source_specs"][0]["file"], "petstore.yaml");
    }

    #[tokio::test]
    async fn test_provenance_reflects_drift() {
        let state = test_state();
        state.drift_detected.store(true, Ordering::Relaxed);

        let response = provenance_response(&state);
        let body = extract_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert_eq!(json["drift_detected"], true);
    }

    #[test]
    fn test_metrics_returns_200() {
        let state = test_state();
        let response = metrics_response(&state);
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_contains_expected_fields() {
        let state = test_state();
        let response = health_response(&state);
        let body = extract_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["artifact_version"], 2);
        assert_eq!(json["compiler_version"], "0.2.1");
        assert_eq!(json["routes_count"], 5);
        assert!(json["uptime_secs"].is_u64());
    }

    #[test]
    fn test_not_found_response() {
        let response = json_response(StatusCode::NOT_FOUND, r#"{"error":"not found"}"#);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_method_not_allowed_response() {
        let response = json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"error":"method not allowed"}"#,
        );
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_provenance_without_provenance_metadata() {
        let manifest = Manifest {
            barbacane_artifact_version: 2,
            compiled_at: "2026-03-01T00:00:00Z".to_string(),
            compiler_version: "0.2.1".to_string(),
            source_specs: vec![],
            routes_count: 0,
            checksums: BTreeMap::new(),
            plugins: vec![],
            artifact_hash: "sha256:test".to_string(),
            provenance: barbacane_compiler::Provenance::default(),
            mcp: barbacane_compiler::McpConfig::default(),
        };
        let state = Arc::new(AdminState {
            manifest: Arc::new(ArcSwap::new(Arc::new(manifest))),
            metrics: Arc::new(MetricsRegistry::new()),
            drift_detected: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        });

        let response = provenance_response(&state);
        let body = extract_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert!(json["provenance"]["commit"].is_null());
        assert!(json["provenance"]["source"].is_null());
        assert_eq!(json["source_specs"].as_array().unwrap().len(), 0);
        assert_eq!(json["plugins"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_provenance_with_plugins() {
        let manifest = Manifest {
            barbacane_artifact_version: 2,
            compiled_at: "2026-03-01T00:00:00Z".to_string(),
            compiler_version: "0.2.1".to_string(),
            source_specs: vec![],
            routes_count: 0,
            checksums: BTreeMap::new(),
            plugins: vec![barbacane_compiler::BundledPlugin {
                name: "rate-limit".to_string(),
                version: "1.0.0".to_string(),
                plugin_type: "middleware".to_string(),
                wasm_path: "plugins/rate-limit.wasm".to_string(),
                sha256: "sha256:plugin_hash".to_string(),
                capabilities: barbacane_compiler::PluginCapabilities::default(),
            }],
            artifact_hash: "sha256:test".to_string(),
            provenance: barbacane_compiler::Provenance::default(),
            mcp: barbacane_compiler::McpConfig::default(),
        };
        let state = Arc::new(AdminState {
            manifest: Arc::new(ArcSwap::new(Arc::new(manifest))),
            metrics: Arc::new(MetricsRegistry::new()),
            drift_detected: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        });

        let response = provenance_response(&state);
        let body = extract_body(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        let plugins = json["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["name"], "rate-limit");
        assert_eq!(plugins[0]["version"], "1.0.0");
        assert_eq!(plugins[0]["sha256"], "sha256:plugin_hash");
    }

    #[test]
    fn test_metrics_content_type_is_prometheus() {
        let state = test_state();
        let response = metrics_response(&state);
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.starts_with("text/plain"),
            "Metrics content type should be text/plain for Prometheus, got: {}",
            ct,
        );
    }
}
