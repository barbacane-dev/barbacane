//! Axum router configuration.

use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse},
    routing::{delete, get, patch, post, put},
    Router,
};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

use scalar_api_reference::scalar_html_default;

use super::auth::{require_admin, AdminAuth};
use super::ws::ConnectionManager;
use super::{
    api_keys, artifacts, compilations, data_planes, health, init, operations, plugins,
    project_plugins, projects, specs, ws,
};

/// OpenAPI spec content embedded at compile time.
const OPENAPI_SPEC: &str = include_str!("../../openapi.yaml");

/// API version header value.
const API_VERSION: &str = "application/vnd.barbacane.v1+json";

/// Body-size ceiling for ordinary JSON API requests. Bounds how much a caller
/// can make the control plane buffer in memory per request.
const MAX_JSON_BODY: usize = 1024 * 1024; // 1 MiB

/// Body-size ceiling for file uploads (spec documents, WASM plugins). Larger
/// than the JSON limit to accommodate real plugin binaries, but still bounded.
const MAX_UPLOAD_BODY: usize = 32 * 1024 * 1024; // 32 MiB

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Channel to send compilation job IDs to the worker.
    pub compilation_tx: Option<mpsc::Sender<Uuid>>,
    /// WebSocket connection manager for data planes.
    pub connection_manager: Arc<ConnectionManager>,
}

/// Handler to serve the OpenAPI specification.
async fn openapi_spec() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/yaml")],
        OPENAPI_SPEC,
    )
}

/// Handler to serve the Scalar API documentation UI.
async fn api_docs() -> Html<String> {
    let config = serde_json::json!({
        "spec": {
            "url": "/api/openapi"
        },
        "theme": "purple",
        "layout": "modern",
        "hideModels": false,
        "hideDownloadButton": false
    });

    Html(scalar_html_default(&config))
}

/// Build a CORS layer from the `BARBACANE_CONTROL_ALLOWED_ORIGINS` environment
/// variable (a comma-separated allowlist of origins). When unset or empty, no
/// cross-origin requests are permitted — the admin API is same-origin by
/// default. Credentials are never combined with a wildcard origin.
fn cors_layer() -> CorsLayer {
    let origins: Vec<HeaderValue> = std::env::var("BARBACANE_CONTROL_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// Create the API router with all routes.
///
/// `admin_auth` is enforced on every route except `/health` and the data-plane
/// WebSocket (`/ws/data-plane`), which authenticates with its own per-data-plane
/// API key.
pub fn create_router(
    pool: PgPool,
    compilation_tx: Option<mpsc::Sender<Uuid>>,
    connection_manager: Arc<ConnectionManager>,
    admin_auth: AdminAuth,
) -> Router {
    let state = AppState {
        pool,
        compilation_tx,
        connection_manager,
    };

    // Public routes: liveness, and the data-plane WebSocket (which performs its
    // own API-key authentication inside the handler).
    let public = Router::new()
        .route("/health", get(health::health_check))
        // WebSocket for data plane connections
        .route("/ws/data-plane", get(ws::ws_handler));

    // Protected routes: everything else requires the admin bearer token.
    let protected = Router::new()
        // OpenAPI spec and documentation
        .route("/openapi", get(openapi_spec))
        .route("/docs", get(api_docs))
        // Init
        .route("/init", post(init::init_project))
        // Specs
        .route(
            "/specs",
            post(specs::upload_spec).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY)),
        )
        .route("/specs", get(specs::list_specs))
        .route("/specs/{id}", get(specs::get_spec))
        .route("/specs/{id}", delete(specs::delete_spec))
        .route("/specs/{id}/history", get(specs::get_spec_history))
        .route("/specs/{id}/content", get(specs::download_spec_content))
        .route("/specs/{id}/compliance", get(specs::get_spec_compliance))
        .route("/specs/{id}/compile", post(compilations::start_compilation))
        .route(
            "/specs/{id}/compilations",
            get(compilations::list_spec_compilations),
        )
        // Plugins
        .route(
            "/plugins",
            post(plugins::register_plugin).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY)),
        )
        .route("/plugins", get(plugins::list_plugins))
        .route("/plugins/{name}", get(plugins::list_plugin_versions))
        .route("/plugins/{name}/{version}", get(plugins::get_plugin))
        .route("/plugins/{name}/{version}", delete(plugins::delete_plugin))
        .route(
            "/plugins/{name}/{version}/download",
            get(plugins::download_plugin),
        )
        // Artifacts
        .route("/artifacts", get(artifacts::list_artifacts))
        .route("/artifacts/{id}", get(artifacts::get_artifact))
        .route("/artifacts/{id}", delete(artifacts::delete_artifact))
        .route(
            "/artifacts/{id}/download",
            get(artifacts::download_artifact),
        )
        // Spec operations (plugin bindings)
        .route(
            "/specs/{id}/operations",
            patch(operations::patch_spec_operations),
        )
        // Compilations
        .route("/compilations/{id}", get(compilations::get_compilation))
        .route(
            "/compilations/{id}",
            delete(compilations::delete_compilation),
        )
        // Projects
        .route("/projects", post(projects::create_project))
        .route("/projects", get(projects::list_projects))
        .route("/projects/{id}", get(projects::get_project))
        .route("/projects/{id}", put(projects::update_project))
        .route("/projects/{id}", delete(projects::delete_project))
        // Project specs
        .route("/projects/{id}/specs", get(projects::list_project_specs))
        .route(
            "/projects/{id}/specs",
            post(projects::upload_spec_to_project).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY)),
        )
        // Project plugins
        .route(
            "/projects/{id}/plugins",
            get(project_plugins::list_project_plugins),
        )
        .route(
            "/projects/{id}/plugins",
            post(project_plugins::add_plugin_to_project),
        )
        .route(
            "/projects/{id}/plugins/{name}",
            put(project_plugins::update_project_plugin),
        )
        .route(
            "/projects/{id}/plugins/{name}",
            delete(project_plugins::remove_plugin_from_project),
        )
        // Project operations (plugin bindings across all specs)
        .route(
            "/projects/{id}/operations",
            get(operations::get_project_operations),
        )
        // Project compilations and artifacts
        .route(
            "/projects/{id}/compilations",
            get(projects::list_project_compilations),
        )
        .route(
            "/projects/{id}/artifacts",
            get(projects::list_project_artifacts),
        )
        // Project API keys
        .route("/projects/{id}/api-keys", post(api_keys::create_api_key))
        .route("/projects/{id}/api-keys", get(api_keys::list_api_keys))
        .route(
            "/projects/{id}/api-keys/{key_id}",
            delete(api_keys::revoke_api_key),
        )
        // Project data planes
        .route(
            "/projects/{id}/data-planes",
            get(data_planes::list_data_planes),
        )
        .route(
            "/projects/{id}/data-planes/{dp_id}",
            get(data_planes::get_data_plane),
        )
        .route(
            "/projects/{id}/data-planes/{dp_id}",
            delete(data_planes::disconnect_data_plane),
        )
        .route(
            "/projects/{id}/deploy",
            post(data_planes::deploy_to_data_planes),
        )
        // Admin authentication on every protected route.
        .layer(axum::middleware::from_fn_with_state(
            admin_auth,
            require_admin,
        ));

    Router::new()
        .merge(public)
        .merge(protected)
        // Default request-body ceiling for all routes (upload routes opt into a
        // larger limit via their own inner DefaultBodyLimit layer).
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY))
        // Middleware applied to all routes
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer())
        // API versioning: set Content-Type to versioned media type for JSON responses
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static(API_VERSION),
        ))
        .with_state(state)
}
