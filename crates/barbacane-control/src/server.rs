//! Control plane HTTP server.

use std::net::SocketAddr;

use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::api::create_router;

/// Server configuration.
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    pub pool: PgPool,
}

/// Run the control plane server.
pub async fn run(config: ServerConfig) -> anyhow::Result<()> {
    // Create channel for compilation jobs
    let (tx, rx) = mpsc::channel::<Uuid>(100);

    // Start compilation worker
    let worker_pool = config.pool.clone();
    tokio::spawn(async move {
        crate::compiler::run_worker(worker_pool, rx).await;
    });

    // Create router
    let app = create_router(config.pool, Some(tx));

    // Bind and serve
    let listener = TcpListener::bind(config.listen_addr).await?;
    tracing::info!("Control plane listening on {}", config.listen_addr);

    axum::serve(listener, app).await?;

    Ok(())
}
