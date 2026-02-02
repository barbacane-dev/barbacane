//! Data plane management endpoints.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::router::AppState;
use crate::db::{ArtifactsRepository, DataPlane, DataPlanesRepository};
use crate::error::ProblemDetails;

/// GET /projects/{id}/data-planes - List data planes for a project.
pub async fn list_data_planes(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<DataPlane>>, ProblemDetails> {
    let repo = DataPlanesRepository::new(state.pool.clone());

    let data_planes = repo.list_for_project(project_id).await.map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to list data planes: {}", e))
    })?;

    Ok(Json(data_planes))
}

/// GET /projects/{id}/data-planes/{dp_id} - Get a data plane.
pub async fn get_data_plane(
    State(state): State<AppState>,
    Path((project_id, dp_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<DataPlane>, ProblemDetails> {
    let repo = DataPlanesRepository::new(state.pool.clone());

    let data_plane = repo
        .get(dp_id)
        .await
        .map_err(|e| {
            ProblemDetails::internal_error_with_detail(format!("Failed to get data plane: {}", e))
        })?
        .ok_or_else(|| ProblemDetails::not_found(format!("Data plane {} not found", dp_id)))?;

    if data_plane.project_id != project_id {
        return Err(ProblemDetails::not_found(format!(
            "Data plane {} not found in project {}",
            dp_id, project_id
        )));
    }

    Ok(Json(data_plane))
}

/// DELETE /projects/{id}/data-planes/{dp_id} - Disconnect a data plane.
pub async fn disconnect_data_plane(
    State(state): State<AppState>,
    Path((project_id, dp_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = DataPlanesRepository::new(state.pool.clone());

    // Verify the data plane belongs to the project
    let data_plane = repo
        .get(dp_id)
        .await
        .map_err(|e| {
            ProblemDetails::internal_error_with_detail(format!("Failed to get data plane: {}", e))
        })?
        .ok_or_else(|| ProblemDetails::not_found(format!("Data plane {} not found", dp_id)))?;

    if data_plane.project_id != project_id {
        return Err(ProblemDetails::not_found(format!(
            "Data plane {} not found in project {}",
            dp_id, project_id
        )));
    }

    // Remove from connection manager (will close WebSocket)
    state.connection_manager.remove(dp_id);

    // Delete the record
    repo.delete(dp_id).await.map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to delete data plane: {}", e))
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Deploy request body.
#[derive(Debug, Deserialize)]
pub struct DeployRequest {
    /// Artifact ID to deploy. If not specified, uses the latest artifact.
    pub artifact_id: Option<Uuid>,
}

/// Deploy response.
#[derive(Debug, Serialize)]
pub struct DeployResponse {
    /// Artifact ID being deployed.
    pub artifact_id: Uuid,
    /// Number of data planes notified.
    pub data_planes_notified: usize,
}

/// POST /projects/{id}/deploy - Deploy artifact to all connected data planes.
pub async fn deploy_to_data_planes(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(request): Json<DeployRequest>,
) -> Result<(StatusCode, Json<DeployResponse>), ProblemDetails> {
    let artifacts_repo = ArtifactsRepository::new(state.pool.clone());

    // Get the artifact to deploy
    let artifact = if let Some(artifact_id) = request.artifact_id {
        artifacts_repo
            .get(artifact_id)
            .await
            .map_err(|e| {
                ProblemDetails::internal_error_with_detail(format!("Failed to get artifact: {}", e))
            })?
            .ok_or_else(|| {
                ProblemDetails::not_found(format!("Artifact {} not found", artifact_id))
            })?
    } else {
        // Get the latest artifact for this project
        let artifacts = artifacts_repo
            .list_for_project(project_id)
            .await
            .map_err(|e| {
                ProblemDetails::internal_error_with_detail(format!(
                    "Failed to list artifacts: {}",
                    e
                ))
            })?;

        artifacts.into_iter().next().ok_or_else(|| {
            ProblemDetails::not_found(format!("No artifacts found for project {}", project_id))
        })?
    };

    // Verify artifact belongs to project
    if artifact.project_id != Some(project_id) {
        return Err(ProblemDetails::bad_request(format!(
            "Artifact {} does not belong to project {}",
            artifact.id, project_id
        )));
    }

    // Build download URL
    let download_url = format!("/artifacts/{}/download", artifact.id);

    // Get count of connected data planes before notification
    let data_planes_notified = state
        .connection_manager
        .project_connection_count(project_id);

    // Notify all connected data planes
    state
        .connection_manager
        .notify_artifact_available(
            project_id,
            artifact.id,
            download_url,
            artifact.sha256.clone(),
        )
        .await;

    Ok((
        StatusCode::OK,
        Json(DeployResponse {
            artifact_id: artifact.id,
            data_planes_notified,
        }),
    ))
}
