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
    if let Some(scope) = input
        .scopes
        .iter()
        .find(|scope| scope.as_str() != super::auth::scopes::DATA_PLANE_CONNECT)
    {
        return Err(ProblemDetails::bad_request(format!(
            "Unknown scope '{scope}'; the scopes are: {}",
            super::auth::scopes::DATA_PLANE_CONNECT
        )));
    }
    if input.expires_at.is_some_and(|at| at <= chrono::Utc::now()) {
        return Err(ProblemDetails::bad_request(
            "expires_at is in the past; a key must expire after its creation",
        ));
    }
    let repo = ApiKeysRepository::new(state.pool.clone());

    // DB errors route through the generic `From<sqlx::Error>` mapper, which logs
    // the detail server-side and returns a generic 500 (no schema disclosure).
    let created = repo.create(project_id, input).await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// GET /projects/{id}/api-keys - List API keys for a project.
pub async fn list_api_keys(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<ApiKey>>, ProblemDetails> {
    let repo = ApiKeysRepository::new(state.pool.clone());

    let keys = repo.list_for_project(project_id).await?;

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
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("API key {} not found", key_id)))?;

    if key.project_id != project_id {
        return Err(ProblemDetails::not_found(format!(
            "API key {} not found in project {}",
            key_id, project_id
        )));
    }

    repo.revoke(key_id).await?;
    // Sessions registered with the key end now, not at their next reconnect.
    state.connection_manager.close_sessions_for_key(key_id);

    Ok(StatusCode::NO_CONTENT)
}
