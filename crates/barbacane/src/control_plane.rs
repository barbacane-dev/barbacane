//! Control plane client for connected mode.
//!
//! This module handles WebSocket communication with the control plane,
//! including registration, heartbeat, and artifact notifications.

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

/// Messages sent from data plane to control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataPlaneMessage {
    /// Initial registration with authentication.
    Register {
        project_id: Uuid,
        api_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        #[serde(default)]
        metadata: serde_json::Value,
    },
    /// Periodic heartbeat.
    Heartbeat {
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        uptime_secs: u64,
        requests_total: u64,
    },
    /// Acknowledgment of artifact download.
    ArtifactDownloaded {
        artifact_id: Uuid,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
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

/// Configuration for the control plane client.
#[derive(Clone)]
pub struct ControlPlaneConfig {
    pub control_plane_url: String,
    pub project_id: Uuid,
    pub api_key: String,
    pub data_plane_name: Option<String>,
}

/// Notification that a new artifact is available.
#[derive(Debug, Clone)]
pub struct ArtifactNotification {
    pub artifact_id: Uuid,
    pub download_url: String,
    pub sha256: String,
}

/// Response to send back to the control plane after downloading an artifact.
#[derive(Debug, Clone)]
pub struct ArtifactDownloadedResponse {
    pub artifact_id: Uuid,
    pub success: bool,
    pub error: Option<String>,
}

/// Control plane client that maintains connection and handles messages.
pub struct ControlPlaneClient {
    config: ControlPlaneConfig,
}

impl ControlPlaneClient {
    /// Create a new control plane client.
    pub fn new(config: ControlPlaneConfig) -> Self {
        Self { config }
    }

    /// Start the connection loop in a background task.
    /// Returns a receiver for artifact notifications and a sender for download responses.
    pub fn start(
        self,
        shutdown_rx: watch::Receiver<bool>,
    ) -> (
        mpsc::Receiver<ArtifactNotification>,
        mpsc::Sender<ArtifactDownloadedResponse>,
    ) {
        let (artifact_tx, artifact_rx) = mpsc::channel::<ArtifactNotification>(16);
        let (response_tx, response_rx) = mpsc::channel::<ArtifactDownloadedResponse>(16);

        tokio::spawn(async move {
            self.connection_loop(shutdown_rx, artifact_tx, response_rx)
                .await;
        });

        (artifact_rx, response_tx)
    }

    /// Main connection loop with reconnection logic.
    async fn connection_loop(
        &self,
        mut shutdown_rx: watch::Receiver<bool>,
        artifact_tx: mpsc::Sender<ArtifactNotification>,
        mut response_rx: mpsc::Receiver<ArtifactDownloadedResponse>,
    ) {
        const INITIAL_BACKOFF_MS: u64 = 1000;
        const MAX_BACKOFF_MS: u64 = 60000;
        const BACKOFF_MULTIPLIER: f64 = 2.0;

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        loop {
            // Check for shutdown
            if *shutdown_rx.borrow() {
                tracing::info!("Control plane client shutting down");
                return;
            }

            tracing::info!(url = %self.config.control_plane_url, "Connecting to control plane");

            match self
                .try_connect(&mut shutdown_rx, &artifact_tx, &mut response_rx)
                .await
            {
                Ok(()) => {
                    // Connection was cleanly closed (e.g., shutdown)
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        backoff_ms = backoff_ms,
                        "Control plane connection failed, will retry"
                    );
                }
            }

            // Wait before reconnecting (or abort if shutdown)
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
            }

            // Increase backoff for next attempt
            backoff_ms =
                ((backoff_ms as f64) * BACKOFF_MULTIPLIER).min(MAX_BACKOFF_MS as f64) as u64;
        }
    }

    /// Attempt to connect and handle messages.
    async fn try_connect(
        &self,
        shutdown_rx: &mut watch::Receiver<bool>,
        artifact_tx: &mpsc::Sender<ArtifactNotification>,
        response_rx: &mut mpsc::Receiver<ArtifactDownloadedResponse>,
    ) -> Result<(), String> {
        // Connect to WebSocket
        let (ws_stream, _response) = connect_async(&self.config.control_plane_url)
            .await
            .map_err(|e| format!("WebSocket connection failed: {}", e))?;

        let (mut sender, mut receiver) = ws_stream.split();

        // Send registration message
        let register_msg = DataPlaneMessage::Register {
            project_id: self.config.project_id,
            api_key: self.config.api_key.clone(),
            name: self.config.data_plane_name.clone(),
            artifact_id: None, // TODO: pass current artifact ID
            metadata: serde_json::json!({}),
        };

        let register_json = serde_json::to_string(&register_msg)
            .map_err(|e| format!("Failed to serialize register message: {}", e))?;

        sender
            .send(Message::Text(register_json.into()))
            .await
            .map_err(|e| format!("Failed to send register message: {}", e))?;

        // Wait for registration response
        let registration_response = tokio::time::timeout(Duration::from_secs(30), receiver.next())
            .await
            .map_err(|_| "Registration timeout")?
            .ok_or("Connection closed before registration")?
            .map_err(|e| format!("WebSocket error: {}", e))?;

        let heartbeat_interval_secs = match registration_response {
            Message::Text(text) => {
                let msg: ControlPlaneMessage = serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse registration response: {}", e))?;

                match msg {
                    ControlPlaneMessage::Registered {
                        data_plane_id,
                        heartbeat_interval_secs,
                    } => {
                        tracing::info!(
                            data_plane_id = %data_plane_id,
                            heartbeat_interval_secs,
                            "Registered with control plane"
                        );
                        heartbeat_interval_secs
                    }
                    ControlPlaneMessage::RegistrationFailed { reason } => {
                        return Err(format!("Registration failed: {}", reason));
                    }
                    other => {
                        return Err(format!("Unexpected registration response: {:?}", other));
                    }
                }
            }
            other => {
                return Err(format!("Unexpected message type: {:?}", other));
            }
        };

        // Start heartbeat timer
        let mut heartbeat_interval =
            tokio::time::interval(Duration::from_secs(heartbeat_interval_secs as u64));
        let start_time = std::time::Instant::now();

        // Main message loop
        loop {
            tokio::select! {
                // Shutdown signal
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Disconnecting from control plane");
                        let _ = sender.close().await;
                        return Ok(());
                    }
                }

                // Heartbeat timer
                _ = heartbeat_interval.tick() => {
                    let heartbeat = DataPlaneMessage::Heartbeat {
                        artifact_id: None, // TODO: pass current artifact ID
                        uptime_secs: start_time.elapsed().as_secs(),
                        requests_total: 0, // TODO: pass actual metrics
                    };

                    let json = serde_json::to_string(&heartbeat)
                        .map_err(|e| format!("Failed to serialize heartbeat: {}", e))?;

                    if let Err(e) = sender.send(Message::Text(json.into())).await {
                        return Err(format!("Failed to send heartbeat: {}", e));
                    }

                    tracing::debug!("Heartbeat sent");
                }

                // Artifact download response from main loop
                Some(response) = response_rx.recv() => {
                    let msg = DataPlaneMessage::ArtifactDownloaded {
                        artifact_id: response.artifact_id,
                        success: response.success,
                        error: response.error,
                    };

                    let json = serde_json::to_string(&msg)
                        .map_err(|e| format!("Failed to serialize artifact downloaded: {}", e))?;

                    if let Err(e) = sender.send(Message::Text(json.into())).await {
                        tracing::warn!(error = %e, "Failed to send artifact downloaded response");
                    } else {
                        tracing::info!(
                            artifact_id = %response.artifact_id,
                            success = response.success,
                            "Sent artifact downloaded response to control plane"
                        );
                    }
                }

                // Incoming messages
                result = receiver.next() => {
                    match result {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<ControlPlaneMessage>(&text) {
                                Ok(msg) => {
                                    if let Err(e) = self.handle_message(msg, artifact_tx, &mut sender).await {
                                        tracing::warn!(error = %e, "Error handling control plane message");
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to parse control plane message");
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = sender.send(Message::Pong(data)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            return Err("Connection closed by control plane".to_string());
                        }
                        Some(Err(e)) => {
                            return Err(format!("WebSocket error: {}", e));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Handle a message from the control plane.
    async fn handle_message(
        &self,
        msg: ControlPlaneMessage,
        artifact_tx: &mpsc::Sender<ArtifactNotification>,
        _sender: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<(), String> {
        match msg {
            ControlPlaneMessage::HeartbeatAck => {
                tracing::debug!("Heartbeat acknowledged");
            }
            ControlPlaneMessage::ArtifactAvailable {
                artifact_id,
                download_url,
                sha256,
            } => {
                tracing::info!(
                    artifact_id = %artifact_id,
                    download_url = %download_url,
                    "New artifact available"
                );

                // Notify the main loop about the new artifact
                if let Err(e) = artifact_tx
                    .send(ArtifactNotification {
                        artifact_id,
                        download_url,
                        sha256,
                    })
                    .await
                {
                    tracing::warn!(error = %e, "Failed to send artifact notification");
                }
            }
            ControlPlaneMessage::Disconnect { reason } => {
                tracing::info!(reason = %reason, "Disconnecting at control plane request");
                return Err(format!("Disconnected by control plane: {}", reason));
            }
            ControlPlaneMessage::Error { message } => {
                tracing::warn!(message = %message, "Error from control plane");
            }
            // These shouldn't happen after registration
            ControlPlaneMessage::Registered { .. }
            | ControlPlaneMessage::RegistrationFailed { .. } => {
                tracing::warn!("Unexpected registration message after already registered");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_plane_message_register_serialization() {
        let msg = DataPlaneMessage::Register {
            project_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            api_key: "test-key".to_string(),
            name: Some("my-data-plane".to_string()),
            artifact_id: None,
            metadata: serde_json::json!({"version": "1.0"}),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"register\""));
        assert!(json.contains("\"project_id\":"));
        assert!(json.contains("\"api_key\":\"test-key\""));
        assert!(json.contains("\"name\":\"my-data-plane\""));
    }

    #[test]
    fn test_data_plane_message_heartbeat_serialization() {
        let msg = DataPlaneMessage::Heartbeat {
            artifact_id: Some(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()),
            uptime_secs: 3600,
            requests_total: 1000,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"heartbeat\""));
        assert!(json.contains("\"uptime_secs\":3600"));
        assert!(json.contains("\"requests_total\":1000"));
    }

    #[test]
    fn test_data_plane_message_artifact_downloaded_success() {
        let artifact_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let msg = DataPlaneMessage::ArtifactDownloaded {
            artifact_id,
            success: true,
            error: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"artifact_downloaded\""));
        assert!(json.contains("\"success\":true"));
        assert!(!json.contains("\"error\":")); // None should be skipped
    }

    #[test]
    fn test_data_plane_message_artifact_downloaded_failure() {
        let artifact_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let msg = DataPlaneMessage::ArtifactDownloaded {
            artifact_id,
            success: false,
            error: Some("checksum mismatch".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"artifact_downloaded\""));
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error\":\"checksum mismatch\""));
    }

    #[test]
    fn test_control_plane_message_registered_deserialization() {
        let json = r#"{
            "type": "registered",
            "data_plane_id": "550e8400-e29b-41d4-a716-446655440000",
            "heartbeat_interval_secs": 30
        }"#;

        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::Registered {
                data_plane_id,
                heartbeat_interval_secs,
            } => {
                assert_eq!(
                    data_plane_id,
                    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
                );
                assert_eq!(heartbeat_interval_secs, 30);
            }
            _ => panic!("Expected Registered message"),
        }
    }

    #[test]
    fn test_control_plane_message_artifact_available_deserialization() {
        let json = r#"{
            "type": "artifact_available",
            "artifact_id": "550e8400-e29b-41d4-a716-446655440000",
            "download_url": "http://localhost:9090/artifacts/123/download",
            "sha256": "abc123def456"
        }"#;

        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::ArtifactAvailable {
                artifact_id,
                download_url,
                sha256,
            } => {
                assert_eq!(
                    artifact_id,
                    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
                );
                assert_eq!(download_url, "http://localhost:9090/artifacts/123/download");
                assert_eq!(sha256, "abc123def456");
            }
            _ => panic!("Expected ArtifactAvailable message"),
        }
    }

    #[test]
    fn test_control_plane_message_disconnect_deserialization() {
        let json = r#"{
            "type": "disconnect",
            "reason": "server shutting down"
        }"#;

        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::Disconnect { reason } => {
                assert_eq!(reason, "server shutting down");
            }
            _ => panic!("Expected Disconnect message"),
        }
    }

    #[test]
    fn test_artifact_downloaded_response_creation() {
        let artifact_id = Uuid::new_v4();

        let success_response = ArtifactDownloadedResponse {
            artifact_id,
            success: true,
            error: None,
        };
        assert!(success_response.success);
        assert!(success_response.error.is_none());

        let failure_response = ArtifactDownloadedResponse {
            artifact_id,
            success: false,
            error: Some("download failed".to_string()),
        };
        assert!(!failure_response.success);
        assert_eq!(failure_response.error.as_deref(), Some("download failed"));
    }

    #[test]
    fn test_artifact_notification_creation() {
        let notification = ArtifactNotification {
            artifact_id: Uuid::new_v4(),
            download_url: "http://example.com/artifact.bca".to_string(),
            sha256: "abc123".to_string(),
        };

        assert!(!notification.download_url.is_empty());
        assert!(!notification.sha256.is_empty());
    }

    #[test]
    fn test_control_plane_config_creation() {
        let config = ControlPlaneConfig {
            control_plane_url: "ws://localhost:9090/ws/data-plane".to_string(),
            project_id: Uuid::new_v4(),
            api_key: "test-api-key".to_string(),
            data_plane_name: Some("test-plane".to_string()),
        };

        assert!(config.control_plane_url.starts_with("ws://"));
        assert_eq!(config.api_key, "test-api-key");
        assert_eq!(config.data_plane_name.as_deref(), Some("test-plane"));
    }
}
