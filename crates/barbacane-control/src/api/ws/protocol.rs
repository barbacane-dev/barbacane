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

/// Largest WebSocket message, and frame, the control plane reads. Protocol
/// messages are small JSON documents; this bounds what an unregistered
/// socket can make the control plane buffer.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Longest data-plane name stored.
pub const MAX_NAME_CHARS: usize = 128;

/// Longest API key accepted in a registration.
pub const MAX_API_KEY_CHARS: usize = 256;

/// Largest registration `metadata` document stored, as serialized JSON.
pub const MAX_METADATA_BYTES: usize = 4096;

/// Longest artifact hash accepted in a heartbeat (a SHA-256 is 64 hex digits).
pub const MAX_ARTIFACT_HASH_CHARS: usize = 128;

/// Whether a registration's fields fit what the control plane stores.
pub fn check_registration(
    api_key: &str,
    name: Option<&str>,
    metadata: &serde_json::Value,
) -> Result<(), &'static str> {
    if api_key.chars().count() > MAX_API_KEY_CHARS {
        return Err("api_key is too long");
    }
    if name.is_some_and(|name| name.chars().count() > MAX_NAME_CHARS) {
        return Err("name is too long");
    }
    if metadata.to_string().len() > MAX_METADATA_BYTES {
        return Err("metadata is too large");
    }
    Ok(())
}

/// Whether a heartbeat's artifact hash fits what the control plane stores.
pub fn check_artifact_hash(artifact_hash: Option<&str>) -> Result<(), &'static str> {
    if artifact_hash.is_some_and(|hash| hash.chars().count() > MAX_ARTIFACT_HASH_CHARS) {
        return Err("artifact_hash is too long");
    }
    Ok(())
}

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

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn a_registration_within_the_limits_is_accepted() {
        let metadata = serde_json::json!({"version": "1.0.0", "host": "dp-1"});
        assert_eq!(
            check_registration("bbk_abc", Some("edge-1"), &metadata),
            Ok(())
        );
        assert_eq!(
            check_registration("bbk_abc", None, &serde_json::Value::Null),
            Ok(())
        );
    }

    #[test]
    fn oversized_registration_fields_are_refused() {
        let ok = serde_json::Value::Null;
        assert!(check_registration(&"k".repeat(MAX_API_KEY_CHARS + 1), None, &ok).is_err());
        assert!(check_registration("bbk_abc", Some(&"n".repeat(MAX_NAME_CHARS + 1)), &ok).is_err());
        let big = serde_json::json!({ "pad": "x".repeat(MAX_METADATA_BYTES) });
        assert!(check_registration("bbk_abc", None, &big).is_err());
    }

    #[test]
    fn an_oversized_artifact_hash_is_refused() {
        assert_eq!(check_artifact_hash(Some(&"a".repeat(64))), Ok(()));
        assert_eq!(check_artifact_hash(None), Ok(()));
        assert!(check_artifact_hash(Some(&"a".repeat(MAX_ARTIFACT_HASH_CHARS + 1))).is_err());
    }
}
