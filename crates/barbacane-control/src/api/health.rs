//! Health check endpoint.

use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use super::router::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// GET /health
pub async fn health_check(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, StatusCode> {
    // Verify database connectivity
    sqlx::query("SELECT 1")
        .execute(&state.pool)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok(Json(HealthResponse {
        status: "healthy",
        version: env!("CARGO_PKG_VERSION"),
    }))
}
