//! Control plane HTTP server.

use std::net::SocketAddr;
use std::sync::Arc;

use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::api::{create_router, ConnectionManager};
use crate::db::DataPlanesRepository;

/// How often to check for stale data planes (seconds).
const STALE_SWEEP_INTERVAL_SECS: u64 = 60;

/// Data planes not seen for this many minutes are marked offline.
const STALE_THRESHOLD_MINUTES: i64 = 2;

/// Server configuration.
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    pub pool: PgPool,
}

/// Run the control plane server.
pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    // Mark any leftover "online" data planes as offline — no WebSocket connections
    // exist yet since we just started, so any online status is stale from a
    // previous run.
    let repo = DataPlanesRepository::new(config.pool.clone());
    let marked = repo.mark_all_offline().await?;
    if marked > 0 {
        tracing::info!(
            count = marked,
            "Marked stale data planes as offline on startup"
        );
    }

    // Create channel for compilation jobs
    let (tx, rx) = mpsc::channel::<Uuid>(100);

    // Start compilation worker
    let worker_pool = config.pool.clone();
    tokio::spawn(async move {
        crate::compiler::run_worker(worker_pool, rx).await;
    });

    // Start stale data plane sweep task
    let sweep_pool = config.pool.clone();
    tokio::spawn(async move {
        run_stale_sweep(sweep_pool).await;
    });

    // Create connection manager for data planes
    let connection_manager = Arc::new(ConnectionManager::new());

    // Create router
    let app = create_router(config.pool, Some(tx), connection_manager);

    // Bind and serve
    let listener = TcpListener::bind(config.listen_addr).await?;
    tracing::info!("Control plane listening on {}", config.listen_addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Periodically marks data planes as offline if they haven't sent a heartbeat
/// within the threshold. This catches connections that drop without a clean
/// WebSocket close (e.g. network failures, killed processes).
async fn run_stale_sweep(pool: PgPool) {
    let repo = DataPlanesRepository::new(pool);
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(STALE_SWEEP_INTERVAL_SECS));
    interval.tick().await; // skip immediate first tick

    loop {
        interval.tick().await;
        match repo.mark_stale_offline(STALE_THRESHOLD_MINUTES).await {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(count, "Marked stale data planes as offline");
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to sweep stale data planes");
            }
        }
    }
}
