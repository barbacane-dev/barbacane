//! WebSocket handler for data plane connections.

use axum::{
    extract::{
        ws::{close_code, CloseFrame, Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
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
use super::manager::SessionEnd;
use crate::api::router::AppState;
use crate::db::{
    ApiKeysRepository, ArtifactsRepository, DataPlanesRepository, NewDataPlane, ProjectsRepository,
};
use sqlx::PgPool;

/// How long a data plane has, once upgraded, to send its registration.
const REGISTRATION_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// WebSocket endpoint handler. The upgrade request carries the data plane's
/// API key as a bearer token; a key that is unknown, expired or without the
/// `data-plane:connect` scope is answered 401 before any upgrade, so a client
/// without one holds no session.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let Some(token) = crate::api::auth::bearer_token(&headers) else {
        return crate::api::auth::unauthorized("a data-plane API key is required");
    };
    let api_key = match ApiKeysRepository::new(state.pool.clone())
        .validate_and_touch(token)
        .await
    {
        Ok(Some(key))
            if crate::api::auth::key_has_scope(
                &key.scopes,
                crate::api::auth::scopes::DATA_PLANE_CONNECT,
            ) =>
        {
            key
        }
        Ok(_) => {
            tracing::warn!("Data-plane upgrade refused: unknown, expired or unscoped API key");
            return crate::api::auth::unauthorized("a valid data-plane API key is required");
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to validate API key");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let token = token.to_string();
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, api_key, token))
}

/// Handle an individual WebSocket connection, whose upgrade presented
/// `api_key` as `token`.
async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    api_key: crate::db::ApiKey,
    token: String,
) {
    // A session slot, held from registration to the end of the session
    // (released on drop when this function returns).
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

    // The registration names the key the upgrade presented.
    let api_keys_repo = ApiKeysRepository::new(state.pool.clone());
    if !same_key(&registration.api_key, &token) {
        tracing::warn!(project_id = %registration.project_id, key_id = %api_key.id, "Registration refused: another API key than the upgrade's");
        let _ = sender.send(to_ws_message(&registration_refused())).await;
        return;
    }

    // Verify project_id matches
    if api_key.project_id != registration.project_id {
        tracing::warn!(project_id = %registration.project_id, key_id = %api_key.id, "Registration refused: API key of another project");
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

    // A data plane that registers without the project's deployment is sent
    // it: a restarted instance, a new replica, one that was offline.
    match deployment_to_send(&state.pool, project_id, claimed_artifact).await {
        Ok(Some(available)) => {
            if let Err(e) = sender.send(to_ws_message(&available)).await {
                tracing::error!(error = %e, "Failed to send the project's deployment");
                let _ = data_planes_repo.mark_offline(data_plane_id).await;
                return;
            }
            let _ = data_planes_repo.mark_one_deploying(data_plane_id).await;
        }
        Ok(None) => {}
        Err(e) => tracing::error!(error = %e, "Failed to read the project's deployment"),
    }

    // Create channel for sending messages to this data plane
    let (tx, mut rx) = mpsc::channel::<ControlPlaneMessage>(32);

    // Register in connection manager
    let mut close = state
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

    // A data plane sends a heartbeat every interval; one that sends nothing for
    // three is gone, though its connection may never report it.
    let silence_limit = state.connection_manager.silence_limit();
    let silence = tokio::time::sleep(silence_limit);
    tokio::pin!(silence);

    // Main message loop
    loop {
        tokio::select! {
            () = &mut silence => {
                tracing::warn!(data_plane_id = %data_plane_id, "Data plane silent; ending the session");
                end_session(&mut sender, SessionEnd::Silent).await;
                break;
            }

            // Ended by this control plane: a revoked key, a deleted record,
            // a shutdown.
            end = async { close.wait_for(Option::is_some).await.ok().and_then(|end| *end) } => {
                if let Some(end) = end {
                    end_session(&mut sender, end).await;
                }
                break;
            }

            _ = key_check.tick() => {
                match api_keys_repo.is_active(api_key.id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        end_session(&mut sender, SessionEnd::KeyInactive).await;
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
                if matches!(result, Some(Ok(_))) {
                    silence.as_mut().reset(tokio::time::Instant::now() + silence_limit);
                }
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
    state.connection_manager.unregister(data_plane_id);
    if let Err(e) = data_planes_repo.mark_offline(data_plane_id).await {
        tracing::error!(error = %e, "Failed to mark data plane offline");
    }

    tracing::info!(data_plane_id = %data_plane_id, "Data plane disconnected");
}

/// Whether two keys are the same, compared by their SHA-256 digests so the
/// comparison's time says nothing of the key.
fn same_key(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    Sha256::digest(a.as_bytes()) == Sha256::digest(b.as_bytes())
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

/// Tell the data plane why its session ends, and whether to register again,
/// then close the socket.
async fn end_session(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    end: SessionEnd,
) {
    let notice = ControlPlaneMessage::Disconnect {
        reason: end.reason().to_string(),
        reconnect: end.reconnect(),
    };
    let _ = sender.send(to_ws_message(&notice)).await;
    let code = match end {
        SessionEnd::ShuttingDown => close_code::AWAY,
        _ => close_code::POLICY,
    };
    let _ = sender
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: end.reason().into(),
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
    let timeout = tokio::time::timeout(REGISTRATION_WINDOW, async {
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
                let loaded = owned_artifact(pool, project_id, Some(artifact_id)).await?;
                data_planes_repo
                    .record_deploy_result(data_plane_id, loaded, None)
                    .await?;
            } else {
                let error = error.unwrap_or_else(|| "the data plane gave no reason".to_string());
                let error: String = error.chars().take(MAX_DEPLOY_ERROR_CHARS).collect();
                data_planes_repo
                    .record_deploy_result(data_plane_id, None, Some(&error))
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

/// The longest deploy error a data plane's report is kept with.
const MAX_DEPLOY_ERROR_CHARS: usize = 2048;

/// The `ArtifactAvailable` message for the project's deployment, unless the
/// project has none or the data plane already runs it.
async fn deployment_to_send(
    pool: &PgPool,
    project_id: Uuid,
    running: Option<Uuid>,
) -> Result<Option<ControlPlaneMessage>, sqlx::Error> {
    let Some(project) = ProjectsRepository::new(pool.clone())
        .get_by_id(project_id)
        .await?
    else {
        return Ok(None);
    };
    let Some(deployed) = project
        .deployed_artifact_id
        .filter(|id| Some(*id) != running)
    else {
        return Ok(None);
    };
    let Some(artifact) = ArtifactsRepository::new(pool.clone()).get(deployed).await? else {
        return Ok(None);
    };
    Ok(Some(ControlPlaneMessage::ArtifactAvailable {
        artifact_id: artifact.id,
        download_url: format!("/artifacts/{}/download", artifact.id),
        sha256: artifact.sha256,
    }))
}
