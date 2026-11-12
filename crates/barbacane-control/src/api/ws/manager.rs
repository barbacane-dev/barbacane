//! Connection manager for tracking active WebSocket connections.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::protocol::ControlPlaneMessage;

/// Ceiling on concurrently handled WebSocket sessions (registered *and* those
/// still pending registration). Bounds how many unauthenticated sockets an
/// attacker can pin during the pre-registration window.
const MAX_CONCURRENT_WS_SESSIONS: usize = 1024;

/// Information about a connected data plane.
#[derive(Debug, Clone)]
struct DataPlaneConnection {
    project_id: Uuid,
    /// The API key the data plane registered with.
    key_id: Uuid,
    tx: mpsc::Sender<ControlPlaneMessage>,
    /// Signalled to end the session.
    close: Arc<Notify>,
}

/// Manages active WebSocket connections to data planes.
#[derive(Debug)]
pub struct ConnectionManager {
    /// Active connections: data_plane_id -> connection info
    connections: DashMap<Uuid, DataPlaneConnection>,
    /// Index: project_id -> Vec<data_plane_id>
    project_connections: DashMap<Uuid, Vec<Uuid>>,
    /// Bounds concurrent WebSocket session handlers (pre-auth + registered).
    session_slots: Arc<Semaphore>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    /// Create a new connection manager.
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            project_connections: DashMap::new(),
            session_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_WS_SESSIONS)),
        }
    }

    /// Try to reserve a slot for a new (as-yet-unauthenticated) WebSocket
    /// session. Returns `None` when the concurrent-session cap is reached, so the
    /// handler can shed load instead of letting sockets pile up. The permit is
    /// held for the session's lifetime and released on drop.
    pub fn try_acquire_session(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.session_slots).try_acquire_owned().ok()
    }

    /// Register a new connection, authenticated with the API key `key_id`.
    /// Returns the signal that ends the session.
    pub fn register(
        &self,
        data_plane_id: Uuid,
        project_id: Uuid,
        key_id: Uuid,
        tx: mpsc::Sender<ControlPlaneMessage>,
    ) -> Arc<Notify> {
        let close = Arc::new(Notify::new());
        let conn = DataPlaneConnection {
            project_id,
            key_id,
            tx,
            close: Arc::clone(&close),
        };

        self.connections.insert(data_plane_id, conn);

        self.project_connections
            .entry(project_id)
            .or_default()
            .push(data_plane_id);

        tracing::info!(
            data_plane_id = %data_plane_id,
            project_id = %project_id,
            "Data plane registered"
        );
        close
    }

    /// End every session registered with the API key `key_id`. Returns how
    /// many were signalled.
    pub fn close_sessions_for_key(&self, key_id: Uuid) -> usize {
        let mut closed = 0;
        for conn in self.connections.iter().filter(|c| c.key_id == key_id) {
            conn.close.notify_one();
            closed += 1;
        }
        closed
    }

    /// Remove a connection and end its session, so a data plane whose record
    /// is deleted stops receiving deployments at once.
    pub fn remove(&self, data_plane_id: Uuid) {
        if let Some((_, conn)) = self.connections.remove(&data_plane_id) {
            conn.close.notify_one();
            // Remove from project index
            if let Some(mut ids) = self.project_connections.get_mut(&conn.project_id) {
                ids.retain(|id| *id != data_plane_id);
            }

            tracing::info!(
                data_plane_id = %data_plane_id,
                project_id = %conn.project_id,
                "Data plane disconnected"
            );
        }
    }

    /// List all connected data plane IDs for a project.
    pub fn list_for_project(&self, project_id: Uuid) -> Vec<Uuid> {
        self.project_connections
            .get(&project_id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Get the number of connected data planes for a project.
    pub fn project_connection_count(&self, project_id: Uuid) -> usize {
        self.project_connections
            .get(&project_id)
            .map(|r| r.len())
            .unwrap_or(0)
    }

    /// Send a message to a specific data plane.
    pub async fn send(
        &self,
        data_plane_id: Uuid,
        message: ControlPlaneMessage,
    ) -> Result<(), mpsc::error::SendError<ControlPlaneMessage>> {
        if let Some(conn) = self.connections.get(&data_plane_id) {
            conn.tx.send(message).await
        } else {
            Err(mpsc::error::SendError(message))
        }
    }

    /// Broadcast a message to all data planes in a project.
    pub async fn broadcast_to_project(&self, project_id: Uuid, message: ControlPlaneMessage) {
        let ids = self.list_for_project(project_id);
        for id in ids {
            if let Err(e) = self.send(id, message.clone()).await {
                tracing::warn!(
                    data_plane_id = %id,
                    error = %e,
                    "Failed to send message to data plane"
                );
            }
        }
    }

    /// Notify all data planes in a project about a new artifact.
    pub async fn notify_artifact_available(
        &self,
        project_id: Uuid,
        artifact_id: Uuid,
        download_url: String,
        sha256: String,
    ) {
        let message = ControlPlaneMessage::ArtifactAvailable {
            artifact_id,
            download_url,
            sha256,
        };
        self.broadcast_to_project(project_id, message).await;

        let count = self.project_connection_count(project_id);
        tracing::info!(
            project_id = %project_id,
            artifact_id = %artifact_id,
            data_planes_notified = count,
            "Broadcast artifact availability"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_a_key_signals_only_its_sessions() {
        let manager = ConnectionManager::new();
        let (revoked_key, other_key) = (Uuid::new_v4(), Uuid::new_v4());
        let (tx, _rx) = mpsc::channel(1);
        let revoked = manager.register(Uuid::new_v4(), Uuid::new_v4(), revoked_key, tx.clone());
        let other = manager.register(Uuid::new_v4(), Uuid::new_v4(), other_key, tx);

        assert_eq!(manager.close_sessions_for_key(revoked_key), 1);

        let wait = std::time::Duration::from_millis(50);
        assert!(tokio::time::timeout(wait, revoked.notified()).await.is_ok());
        assert!(tokio::time::timeout(wait, other.notified()).await.is_err());
    }
}
