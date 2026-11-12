//! Liveness and readiness probes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{extract::State, http::StatusCode, Json};
use parking_lot::Mutex;
use serde::Serialize;
use sqlx::PgPool;

use super::router::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Serialize)]
pub struct ReadyResponse {
    pub status: &'static str,
}

/// How long a readiness probe's answer stands before the database is asked
/// again, so probes cannot occupy the pool.
const READINESS_CACHE: Duration = Duration::from_secs(1);

/// The last readiness answer and when it was obtained.
#[derive(Default)]
pub struct Readiness {
    last: Mutex<Option<(Instant, bool)>>,
}

impl Readiness {
    /// Whether the database answers, asked at most once per [`READINESS_CACHE`].
    pub async fn database_ready(&self, pool: &PgPool) -> bool {
        if let Some((at, ready)) = *self.last.lock() {
            if at.elapsed() < READINESS_CACHE {
                return ready;
            }
        }
        let ready = sqlx::query("SELECT 1").execute(pool).await.is_ok();
        *self.last.lock() = Some((Instant::now(), ready));
        ready
    }
}

/// GET /health: the process is up. No database query, so a probe cannot
/// occupy the pool and a database outage does not restart the control plane.
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// GET /ready: the control plane can serve requests, which needs its
/// database.
pub async fn readiness_check(State(state): State<AppState>) -> (StatusCode, Json<ReadyResponse>) {
    let readiness: Arc<Readiness> = Arc::clone(&state.readiness);
    if readiness.database_ready(&state.pool).await {
        (StatusCode::OK, Json(ReadyResponse { status: "ready" }))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "unavailable",
            }),
        )
    }
}
