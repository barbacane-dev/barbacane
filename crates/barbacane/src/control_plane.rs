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
#[allow(dead_code)]
pub struct ArtifactNotification {
    pub artifact_id: Uuid,
    pub download_url: String,
    pub sha256: String,
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
    /// Returns a receiver for artifact notifications.
    pub fn start(self, shutdown_rx: watch::Receiver<bool>) -> mpsc::Receiver<ArtifactNotification> {
        let (tx, rx) = mpsc::channel::<ArtifactNotification>(16);

        tokio::spawn(async move {
            self.connection_loop(shutdown_rx, tx).await;
        });

        rx
    }

    /// Main connection loop with reconnection logic.
    async fn connection_loop(
        &self,
        mut shutdown_rx: watch::Receiver<bool>,
        artifact_tx: mpsc::Sender<ArtifactNotification>,
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

            match self.try_connect(&mut shutdown_rx, &artifact_tx).await {
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
