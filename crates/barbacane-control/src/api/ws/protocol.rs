//! WebSocket protocol messages for data plane communication.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Messages sent from data plane to control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataPlaneMessage {
    /// Initial registration with authentication.
    Register {
        project_id: Uuid,
        api_key: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        artifact_id: Option<Uuid>,
        #[serde(default)]
        metadata: serde_json::Value,
    },
    /// Periodic heartbeat.
    Heartbeat {
        #[serde(default)]
        artifact_id: Option<Uuid>,
        #[serde(default)]
        uptime_secs: u64,
        #[serde(default)]
        requests_total: u64,
    },
    /// Acknowledgment of artifact download.
    ArtifactDownloaded {
        artifact_id: Uuid,
        success: bool,
        #[serde(default)]
        error: Option<String>,
    },
}

/// Messages sent from control plane to data plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlPlaneMessage {
    /// Registration successful.
    Registered {
        data_plane_id: Uuid,
        heartbeat_interval_secs: u32,
    },
    /// Registration failed.
    RegistrationFailed { reason: String },
    /// New artifact available for download.
    ArtifactAvailable {
        artifact_id: Uuid,
        download_url: String,
        sha256: String,
    },
    /// Heartbeat acknowledgment.
    HeartbeatAck,
    /// Request disconnect.
    Disconnect { reason: String },
    /// Error message.
    Error { message: String },
}

/// Default heartbeat interval in seconds.
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u32 = 30;
