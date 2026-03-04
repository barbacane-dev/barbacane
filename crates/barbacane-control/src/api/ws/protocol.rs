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
        artifact_hash: Option<String>,
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
    /// Heartbeat acknowledgment with drift detection status.
    HeartbeatAck { drift_detected: bool },
    /// Request disconnect.
    Disconnect { reason: String },
    /// Error message.
    Error { message: String },
}

/// Default heartbeat interval in seconds.
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u32 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_with_artifact_hash_deserialization() {
        let json = r#"{
            "type": "heartbeat",
            "artifact_hash": "sha256:abc123def",
            "uptime_secs": 3600,
            "requests_total": 500
        }"#;

        let msg: DataPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            DataPlaneMessage::Heartbeat {
                artifact_hash,
                uptime_secs,
                ..
            } => {
                assert_eq!(artifact_hash, Some("sha256:abc123def".to_string()));
                assert_eq!(uptime_secs, 3600);
            }
            _ => panic!("Expected Heartbeat message"),
        }
    }

    #[test]
    fn heartbeat_without_artifact_hash_deserialization() {
        let json = r#"{
            "type": "heartbeat",
            "uptime_secs": 100,
            "requests_total": 0
        }"#;

        let msg: DataPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            DataPlaneMessage::Heartbeat { artifact_hash, .. } => {
                assert!(artifact_hash.is_none());
            }
            _ => panic!("Expected Heartbeat message"),
        }
    }

    #[test]
    fn heartbeat_ack_drift_detected_serialization() {
        let msg = ControlPlaneMessage::HeartbeatAck {
            drift_detected: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"drift_detected\":true"));

        let msg = ControlPlaneMessage::HeartbeatAck {
            drift_detected: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"drift_detected\":false"));
    }

    #[test]
    fn heartbeat_ack_round_trip() {
        let original = ControlPlaneMessage::HeartbeatAck {
            drift_detected: true,
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ControlPlaneMessage = serde_json::from_str(&json).unwrap();

        match deserialized {
            ControlPlaneMessage::HeartbeatAck { drift_detected } => {
                assert!(drift_detected);
            }
            _ => panic!("Expected HeartbeatAck"),
        }
    }
}
