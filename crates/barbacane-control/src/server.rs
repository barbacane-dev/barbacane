//! Control plane HTTP server.

use std::net::SocketAddr;
use std::sync::Arc;

use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::api::{create_router, AdminAuth, ConnectionManager};
use crate::db::{CompilationsRepository, DataPlanesRepository};

/// How often to check for stale data planes (seconds).
const STALE_SWEEP_INTERVAL_SECS: u64 = 60;

/// Data planes not seen for this many minutes are marked offline.
const STALE_THRESHOLD_MINUTES: i64 = 2;

/// Offline data planes older than this are hard-deleted by the sweep, bounding
/// unbounded row growth from repeated connect/register/disconnect cycles while
/// still retaining recent offline history.
const OFFLINE_RETENTION_MINUTES: i64 = 24 * 60;

/// How long the server gives open connections and the compilation in
/// progress to finish after a shutdown signal.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Server configuration.
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    pub pool: PgPool,
    pub admin_auth: AdminAuth,
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

    // Compilations a stopped run left behind: `compiling` ones go back to
    // `pending`, and every `pending` one is queued.
    let compilations = CompilationsRepository::new(config.pool.clone());
    let interrupted = compilations.requeue_interrupted().await?;
    if interrupted > 0 {
        tracing::info!(
            count = interrupted,
            "Requeued compilations interrupted by the last stop"
        );
    }
    let pending = compilations.pending().await?;

    // Create channel for compilation jobs
    let (tx, rx) = mpsc::channel::<Uuid>(100);
    let (stop_worker, worker_stop) = tokio::sync::watch::channel(false);

    // Start compilation worker
    let worker_pool = config.pool.clone();
    let worker = tokio::spawn(async move {
        crate::compiler::run_worker(worker_pool, rx, worker_stop).await;
    });
    if !pending.is_empty() {
        tracing::info!(count = pending.len(), "Queueing pending compilations");
        let queue = tx.clone();
        tokio::spawn(async move {
            for id in pending {
                if queue.send(id).await.is_err() {
                    break;
                }
            }
        });
    }

    // Start stale data plane sweep task
    let sweep_pool = config.pool.clone();
    tokio::spawn(async move {
        run_stale_sweep(sweep_pool).await;
    });

    // Create connection manager for data planes
    let connection_manager = Arc::new(ConnectionManager::new());

    // Create router
    let app = create_router(
        config.pool,
        Some(tx),
        Arc::clone(&connection_manager),
        config.admin_auth,
    );

    // Bind and serve
    let listener = TcpListener::bind(config.listen_addr).await?;
    // The bound address, not the requested one: with port 0 the OS picks, and
    // this line is how the caller learns what it picked.
    let bound = listener.local_addr().unwrap_or(config.listen_addr);
    tracing::info!("Control plane listening on {}", bound);

    // At the signal: no new connection, the data planes' sessions end (they
    // reconnect), and the worker takes no further compilation.
    let (signalled_tx, signalled) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        wait_for_shutdown_signal().await;
        tracing::info!("Shutting down");
        let closed = connection_manager.close_all();
        if closed > 0 {
            tracing::info!(count = closed, "Closed data plane sessions");
        }
        let _ = stop_worker.send(true);
        let _ = signalled_tx.send(());
    };
    let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown);
    let grace_expired = async {
        if signalled.await.is_ok() {
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        served = serve => served?,
        () = grace_expired => {
            tracing::warn!("Connections still open after the shutdown grace period; stopping");
        }
    }

    // The compilation in progress finishes, within the same grace period.
    if tokio::time::timeout(SHUTDOWN_GRACE, worker).await.is_err() {
        tracing::warn!("A compilation was still running; it is requeued at the next start");
    }
    tracing::info!("Control plane stopped");

    Ok(())
}

/// Wait for SIGTERM or SIGINT.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut term), Ok(mut int)) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
            }
            _ => {
                tracing::warn!("No signal handler; the control plane stops only when killed");
                std::future::pending::<()>().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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
        match repo
            .delete_offline_older_than(OFFLINE_RETENTION_MINUTES)
            .await
        {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(count, "Deleted long-offline data planes past retention");
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to reap offline data planes");
            }
        }
    }
}
