//! Axum router configuration.

use axum::{
    routing::{delete, get, post},
    Router,
};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use super::{artifacts, compilations, health, plugins, specs};

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Channel to send compilation job IDs to the worker.
    pub compilation_tx: Option<mpsc::Sender<Uuid>>,
}

/// Create the API router with all routes.
pub fn create_router(pool: PgPool, compilation_tx: Option<mpsc::Sender<Uuid>>) -> Router {
    let state = AppState {
        pool,
        compilation_tx,
    };

    Router::new()
        // Health
        .route("/health", get(health::health_check))
        // Specs
        .route("/specs", post(specs::upload_spec))
        .route("/specs", get(specs::list_specs))
        .route("/specs/{id}", get(specs::get_spec))
        .route("/specs/{id}", delete(specs::delete_spec))
        .route("/specs/{id}/history", get(specs::get_spec_history))
        .route("/specs/{id}/content", get(specs::download_spec_content))
        .route("/specs/{id}/compile", post(compilations::start_compilation))
        .route(
            "/specs/{id}/compilations",
            get(compilations::list_spec_compilations),
        )
        // Plugins
        .route("/plugins", post(plugins::register_plugin))
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
        // Compilations
        .route("/compilations/{id}", get(compilations::get_compilation))
        .route(
            "/compilations/{id}",
            delete(compilations::delete_compilation),
        )
        // Middleware
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
