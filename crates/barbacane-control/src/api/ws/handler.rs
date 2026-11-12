//! WebSocket handler for data plane connections.

use axum::{
    extract::{
        ws::{close_code, CloseFrame, Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::protocol::{
    check_artifact_hash, check_registration, ControlPlaneMessage, DataPlaneMessage,
    DEFAULT_HEARTBEAT_INTERVAL_SECS, MAX_MESSAGE_BYTES,
};

/// Serialize a control plane message into a WebSocket text frame.
fn to_ws_message(msg: &ControlPlaneMessage) -> Message {
    Message::Text(
        serde_json::to_string(msg)
            .expect("ControlPlaneMessage is always JSON-serializable")
            .into(),
    )
}
use crate::api::router::AppState;
use crate::db::{ApiKeysRepository, ArtifactsRepository, DataPlanesRepository, NewDataPlane};
use sqlx::PgPool;

/// WebSocket endpoint handler.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(|socket| handle_socket(socket, state))
}

/// Handle an individual WebSocket connection.
async fn handle_socket(socket: WebSocket, state: AppState) {
    // Reserve a session slot up front so unauthenticated sockets can't pile up
    // during the registration window. Held for the whole session (released on
    // drop when this function returns).
    let _session_permit = match state.connection_manager.try_acquire_session() {
        Some(permit) => permit,
        None => {
            tracing::warn!("WebSocket session cap reached; rejecting connection");
            return;
        }
    };

    let (mut sender, mut receiver) = socket.split();

    // Wait for registration message
    let registration = match wait_for_registration(&mut receiver).await {
        Some(Ok(reg)) => reg,
        Some(Err(reason)) => {
            tracing::warn!(reason, "Registration refused");
            let msg = ControlPlaneMessage::RegistrationFailed {
                reason: "invalid registration".to_string(),
            };
            let _ = sender.send(to_ws_message(&msg)).await;
            return;
        }
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
            tracing::warn!(project_id = %registration.project_id, "Registration refused: invalid or expired API key");
            let _ = sender.send(to_ws_message(&registration_refused())).await;
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to validate API key");
            let msg = ControlPlaneMessage::RegistrationFailed {
                reason: "Internal error".to_string(),
            };
            let _ = sender.send(to_ws_message(&msg)).await;
            return;
        }
    };

    // Verify project_id matches
    if api_key.project_id != registration.project_id {
        tracing::warn!(project_id = %registration.project_id, key_id = %api_key.id, "Registration refused: API key of another project");
        let _ = sender.send(to_ws_message(&registration_refused())).await;
        return;
    }

    // Enforce the data-plane:connect scope: a key valid for the project must
    // also be authorized for this action (api_keys.scopes), not merely exist.
    if !crate::api::auth::key_has_scope(
        &api_key.scopes,
        crate::api::auth::scopes::DATA_PLANE_CONNECT,
    ) {
        tracing::warn!(project_id = %registration.project_id, key_id = %api_key.id, "Registration refused: API key lacks the data-plane:connect scope");
        let _ = sender.send(to_ws_message(&registration_refused())).await;
        return;
    }

    // A data plane may claim artifacts of its project only.
    let claimed_artifact = match owned_artifact(
        &state.pool,
        registration.project_id,
        registration.artifact_id,
    )
    .await
    {
        Ok(artifact) => artifact,
        Err(e) => {
            tracing::error!(error = %e, "Failed to check the claimed artifact");
            let msg = ControlPlaneMessage::RegistrationFailed {
                reason: "Internal error".to_string(),
            };
            let _ = sender.send(to_ws_message(&msg)).await;
            return;
        }
    };

    // Create data plane record
    let data_planes_repo = DataPlanesRepository::new(state.pool.clone());
    let data_plane = match data_planes_repo
        .create(NewDataPlane {
            project_id: registration.project_id,
            name: registration.name.clone(),
            artifact_id: claimed_artifact,
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
            let _ = sender.send(to_ws_message(&msg)).await;
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
    if let Err(e) = sender.send(to_ws_message(&confirm_msg)).await {
        tracing::error!(error = %e, "Failed to send registration confirmation");
        let _ = data_planes_repo.delete(data_plane_id).await;
        return;
    }

    // Create channel for sending messages to this data plane
    let (tx, mut rx) = mpsc::channel::<ControlPlaneMessage>(32);

    // Register in connection manager
    let close = state
        .connection_manager
        .register(data_plane_id, project_id, api_key.id, tx);

    // The key is rechecked on this period, so its expiry, or a revocation made
    // through another control-plane instance, also ends the session.
    let key_check_period =
        std::time::Duration::from_secs(u64::from(DEFAULT_HEARTBEAT_INTERVAL_SECS));
    let mut key_check = tokio::time::interval_at(
        tokio::time::Instant::now() + key_check_period,
        key_check_period,
    );

    tracing::info!(
        data_plane_id = %data_plane_id,
        project_id = %project_id,
        name = ?registration.name,
        "Data plane connected"
    );

    // Main message loop
    loop {
        tokio::select! {
            // The API key was revoked through this control plane.
            _ = close.notified() => {
                end_session(&mut sender, "API key revoked").await;
                break;
            }

            _ = key_check.tick() => {
                match api_keys_repo.is_active(api_key.id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        end_session(&mut sender, "API key revoked or expired").await;
                        break;
                    }
                    Err(e) => tracing::warn!(error = %e, "Failed to recheck the API key"),
                }
            }

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
                            project_id,
                            &state.pool,
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

/// The one answer to a registration the control plane refuses, whatever the
/// cause: the cause is logged, not told to the holder of the key.
fn registration_refused() -> ControlPlaneMessage {
    ControlPlaneMessage::RegistrationFailed {
        reason: "registration refused".to_string(),
    }
}

/// `artifact_id` when the artifact exists and belongs to `project_id`. A
/// data plane claims what it runs; a claim outside its project is ignored.
async fn owned_artifact(
    pool: &PgPool,
    project_id: Uuid,
    artifact_id: Option<Uuid>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(id) = artifact_id else {
        return Ok(None);
    };
    let owned = ArtifactsRepository::new(pool.clone())
        .get(id)
        .await?
        .is_some_and(|artifact| artifact.project_id == Some(project_id));
    if !owned {
        tracing::warn!(%project_id, artifact_id = %id, "Artifact claimed by a data plane is not in its project; ignored");
    }
    Ok(owned.then_some(id))
}

/// Tell the data plane why its session ends, then close the socket.
async fn end_session(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    reason: &'static str,
) {
    let notice = ControlPlaneMessage::Disconnect {
        reason: reason.to_string(),
    };
    let _ = sender.send(to_ws_message(&notice)).await;
    let _ = sender
        .send(Message::Close(Some(CloseFrame {
            code: close_code::POLICY,
            reason: reason.into(),
        })))
        .await;
}

/// Registration info extracted from the register message.
struct RegistrationInfo {
    project_id: Uuid,
    api_key: String,
    name: Option<String>,
    artifact_id: Option<Uuid>,
    metadata: serde_json::Value,
}

/// Wait for the registration message from the data plane. A registration
/// whose fields exceed what the control plane stores is refused.
async fn wait_for_registration(
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<Result<RegistrationInfo, &'static str>> {
    // Timeout after 30 seconds
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while let Some(result) = receiver.next().await {
            match result {
                Ok(Message::Text(text)) => match serde_json::from_str::<DataPlaneMessage>(&text) {
                    Ok(DataPlaneMessage::Register {
                        project_id,
                        api_key,
                        name,
                        artifact_id,
                        metadata,
                    }) => {
                        if let Err(reason) =
                            check_registration(&api_key, name.as_deref(), &metadata)
                        {
                            return Some(Err(reason));
                        }
                        return Some(Ok(RegistrationInfo {
                            project_id,
                            api_key,
                            name,
                            artifact_id,
                            metadata,
                        }));
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
    project_id: Uuid,
    pool: &PgPool,
    data_planes_repo: &DataPlanesRepository,
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let msg: DataPlaneMessage = serde_json::from_str(text)?;

    match msg {
        DataPlaneMessage::Heartbeat {
            artifact_id,
            artifact_hash,
            uptime_secs,
            requests_total,
        } => {
            check_artifact_hash(artifact_hash.as_deref())?;
            tracing::debug!(
                data_plane_id = %data_plane_id,
                ?artifact_id,
                ?artifact_hash,
                uptime_secs,
                requests_total,
                "Heartbeat received"
            );

            // Sync artifact_id if reported and in the project, otherwise just update last_seen
            if let Some(aid) = owned_artifact(pool, project_id, artifact_id).await? {
                data_planes_repo.update_artifact(data_plane_id, aid).await?;
            } else {
                data_planes_repo.update_last_seen(data_plane_id).await?;
            }

            // Drift detection: compare reported hash against expected
            let drift_detected = if let Some(ref reported_hash) = artifact_hash {
                data_planes_repo
                    .update_artifact_hash(data_plane_id, reported_hash)
                    .await?;

                let expected = data_planes_repo
                    .get_expected_artifact_hash(data_plane_id)
                    .await?;

                let drifted = expected
                    .as_ref()
                    .is_some_and(|expected| expected != reported_hash);

                if drifted {
                    data_planes_repo
                        .set_drift_status(data_plane_id, true)
                        .await?;
                    tracing::warn!(
                        data_plane_id = %data_plane_id,
                        reported = %reported_hash,
                        expected = ?expected,
                        "Configuration drift detected"
                    );
                }

                drifted
            } else {
                false
            };

            let ack = ControlPlaneMessage::HeartbeatAck { drift_detected };
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
                if let Some(aid) = owned_artifact(pool, project_id, Some(artifact_id)).await? {
                    data_planes_repo.update_artifact(data_plane_id, aid).await?;
                }
            }
        }
        DataPlaneMessage::Register { .. } => {
            // Ignore duplicate registration
            tracing::warn!(data_plane_id = %data_plane_id, "Received duplicate register message");
        }
    }

    Ok(())
}
