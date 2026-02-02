//! API key management endpoints.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use super::router::AppState;
use crate::db::{ApiKey, ApiKeyCreated, ApiKeysRepository, NewApiKey};
use crate::error::ProblemDetails;

/// POST /projects/{id}/api-keys - Create a new API key.
pub async fn create_api_key(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(input): Json<NewApiKey>,
) -> Result<(StatusCode, Json<ApiKeyCreated>), ProblemDetails> {
    let repo = ApiKeysRepository::new(state.pool.clone());

    let created = repo.create(project_id, input).await.map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to create API key: {}", e))
    })?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// GET /projects/{id}/api-keys - List API keys for a project.
pub async fn list_api_keys(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<ApiKey>>, ProblemDetails> {
    let repo = ApiKeysRepository::new(state.pool.clone());

    let keys = repo.list_for_project(project_id).await.map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to list API keys: {}", e))
    })?;

    Ok(Json(keys))
}

/// DELETE /projects/{id}/api-keys/{key_id} - Revoke an API key.
pub async fn revoke_api_key(
    State(state): State<AppState>,
    Path((project_id, key_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = ApiKeysRepository::new(state.pool.clone());

    // Verify the key belongs to the project
    let key = repo
        .get(key_id)
        .await
        .map_err(|e| {
            ProblemDetails::internal_error_with_detail(format!("Failed to get API key: {}", e))
        })?
        .ok_or_else(|| ProblemDetails::not_found(format!("API key {} not found", key_id)))?;

    if key.project_id != project_id {
        return Err(ProblemDetails::not_found(format!(
            "API key {} not found in project {}",
            key_id, project_id
        )));
    }

    repo.revoke(key_id).await.map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to revoke API key: {}", e))
    })?;

    Ok(StatusCode::NO_CONTENT)
}
