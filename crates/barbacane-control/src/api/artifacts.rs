//! Artifacts API handlers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::db::{Artifact, ArtifactsRepository};
use crate::error::ProblemDetails;

use super::router::AppState;

/// GET /artifacts - List all artifacts
pub async fn list_artifacts(
    State(state): State<AppState>,
) -> Result<Json<Vec<Artifact>>, ProblemDetails> {
    let repo = ArtifactsRepository::new(state.pool.clone());
    let artifacts = repo.list().await?;
    Ok(Json(artifacts))
}

/// GET /artifacts/:id - Get artifact metadata
pub async fn get_artifact(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Artifact>, ProblemDetails> {
    let repo = ArtifactsRepository::new(state.pool.clone());
    let artifact = repo
        .get(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Artifact {} not found", id)))?;
    Ok(Json(artifact))
}

/// GET /artifacts/:id/download - Download artifact .bca file
pub async fn download_artifact(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<
    (
        StatusCode,
        [(axum::http::header::HeaderName, String); 2],
        Vec<u8>,
    ),
    ProblemDetails,
> {
    let repo = ArtifactsRepository::new(state.pool.clone());
    let artifact = repo
        .get_with_data(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Artifact {} not found", id)))?;

    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.bca\"", id),
            ),
        ],
        artifact.data,
    ))
}

/// DELETE /artifacts/:id - Delete an artifact
pub async fn delete_artifact(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = ArtifactsRepository::new(state.pool.clone());
    let deleted = repo.delete(id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ProblemDetails::not_found(format!(
            "Artifact {} not found",
            id
        )))
    }
}
