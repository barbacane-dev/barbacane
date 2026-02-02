//! WebSocket handler for data plane connections.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::protocol::{ControlPlaneMessage, DataPlaneMessage, DEFAULT_HEARTBEAT_INTERVAL_SECS};
use crate::api::router::AppState;
use crate::db::{ApiKeysRepository, DataPlanesRepository, NewDataPlane};

/// WebSocket endpoint handler.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

/// Handle an individual WebSocket connection.
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    // Wait for registration message
    let registration = match wait_for_registration(&mut receiver).await {
        Some(reg) => reg,
        None => {
            tracing::warn!("WebSocket closed before registration");
            return;
        }
    };

    // Validate API key
    let api_keys_repo = ApiKeysRepository::new(state.pool.clone());
    let api_key = match api_keys_repo
        .validate_and_touch(&registration.api_key)
        .await
    {
        Ok(Some(key)) => key,
        Ok(None) => {
            let msg = ControlPlaneMessage::RegistrationFailed {
                reason: "Invalid or expired API key".to_string(),
            };
            let _ = sender
                .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                .await;
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to validate API key");
            let msg = ControlPlaneMessage::RegistrationFailed {
                reason: "Internal error".to_string(),
            };
            let _ = sender
                .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                .await;
            return;
        }
    };

    // Verify project_id matches
    if api_key.project_id != registration.project_id {
        let msg = ControlPlaneMessage::RegistrationFailed {
            reason: "API key does not match project".to_string(),
        };
        let _ = sender
            .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
            .await;
        return;
    }

    // Create data plane record
    let data_planes_repo = DataPlanesRepository::new(state.pool.clone());
    let data_plane = match data_planes_repo
        .create(NewDataPlane {
            project_id: registration.project_id,
            name: registration.name.clone(),
            metadata: registration.metadata.clone(),
        })
        .await
    {
        Ok(dp) => dp,
        Err(e) => {
            tracing::error!(error = %e, "Failed to create data plane record");
            let msg = ControlPlaneMessage::RegistrationFailed {
                reason: "Failed to register data plane".to_string(),
            };
            let _ = sender
                .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
                .await;
            return;
        }
    };

    let data_plane_id = data_plane.id;
    let project_id = data_plane.project_id;

    // Send registration confirmation
    let confirm_msg = ControlPlaneMessage::Registered {
        data_plane_id,
        heartbeat_interval_secs: DEFAULT_HEARTBEAT_INTERVAL_SECS,
    };
    if let Err(e) = sender
        .send(Message::Text(
            serde_json::to_string(&confirm_msg).unwrap().into(),
        ))
        .await
    {
        tracing::error!(error = %e, "Failed to send registration confirmation");
        let _ = data_planes_repo.delete(data_plane_id).await;
        return;
    }

    // Create channel for sending messages to this data plane
    let (tx, mut rx) = mpsc::channel::<ControlPlaneMessage>(32);

    // Register in connection manager
    state
        .connection_manager
        .register(data_plane_id, project_id, registration.name.clone(), tx);

    tracing::info!(
        data_plane_id = %data_plane_id,
        project_id = %project_id,
        name = ?registration.name,
        "Data plane connected"
    );

    // Main message loop
    loop {
        tokio::select! {
            // Messages from connection manager (e.g., artifact notifications)
            Some(msg) = rx.recv() => {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to serialize message");
                        continue;
                    }
                };
                if let Err(e) = sender.send(Message::Text(json.into())).await {
                    tracing::warn!(error = %e, "Failed to send message, closing connection");
                    break;
                }
            }

            // Messages from data plane
            result = receiver.next() => {
                match result {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = handle_message(
                            &text,
                            data_plane_id,
                            &data_planes_repo,
                            &mut sender,
                        ).await {
                            tracing::warn!(error = %e, "Error handling message");
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if let Err(e) = sender.send(Message::Pong(data)).await {
                            tracing::warn!(error = %e, "Failed to send pong");
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::info!(data_plane_id = %data_plane_id, "Data plane closing connection");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "WebSocket error");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // Cleanup
    state.connection_manager.remove(data_plane_id);
    if let Err(e) = data_planes_repo.mark_offline(data_plane_id).await {
        tracing::error!(error = %e, "Failed to mark data plane offline");
    }

    tracing::info!(data_plane_id = %data_plane_id, "Data plane disconnected");
}

/// Registration info extracted from the register message.
struct RegistrationInfo {
    project_id: Uuid,
    api_key: String,
    name: Option<String>,
    metadata: serde_json::Value,
}

/// Wait for the registration message from the data plane.
async fn wait_for_registration(
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<RegistrationInfo> {
    // Timeout after 30 seconds
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while let Some(result) = receiver.next().await {
            match result {
                Ok(Message::Text(text)) => match serde_json::from_str::<DataPlaneMessage>(&text) {
                    Ok(DataPlaneMessage::Register {
                        project_id,
                        api_key,
                        name,
                        metadata,
                        ..
                    }) => {
                        return Some(RegistrationInfo {
                            project_id,
                            api_key,
                            name,
                            metadata,
                        });
                    }
                    Ok(_) => {
                        tracing::warn!("Expected register message, got something else");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to parse message");
                        continue;
                    }
                },
                Ok(Message::Ping(_)) => continue,
                Ok(Message::Close(_)) | Err(_) => return None,
                _ => continue,
            }
        }
        None
    });

    timeout.await.ok().flatten()
}

/// Handle a message from the data plane.
async fn handle_message(
    text: &str,
    data_plane_id: Uuid,
    data_planes_repo: &DataPlanesRepository,
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let msg: DataPlaneMessage = serde_json::from_str(text)?;

    match msg {
        DataPlaneMessage::Heartbeat {
            artifact_id,
            uptime_secs,
            requests_total,
        } => {
            tracing::debug!(
                data_plane_id = %data_plane_id,
                ?artifact_id,
                uptime_secs,
                requests_total,
                "Heartbeat received"
            );
            data_planes_repo.update_last_seen(data_plane_id).await?;

            let ack = ControlPlaneMessage::HeartbeatAck;
            sender
                .send(Message::Text(serde_json::to_string(&ack)?.into()))
                .await?;
        }
        DataPlaneMessage::ArtifactDownloaded {
            artifact_id,
            success,
            error,
        } => {
            tracing::info!(
                data_plane_id = %data_plane_id,
                artifact_id = %artifact_id,
                success,
                ?error,
                "Artifact download reported"
            );
            if success {
                data_planes_repo
                    .update_artifact(data_plane_id, artifact_id)
                    .await?;
            }
        }
        DataPlaneMessage::Register { .. } => {
            // Ignore duplicate registration
            tracing::warn!(data_plane_id = %data_plane_id, "Received duplicate register message");
        }
    }

    Ok(())
}
