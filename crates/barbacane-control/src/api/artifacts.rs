//! Artifacts API handlers.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use crate::db::{ApiKey, ApiKeysRepository, Artifact, ArtifactsRepository};
use crate::error::ProblemDetails;

use super::auth::{bearer_token, key_has_scope, scopes, unauthorized, AdminAuth};
use super::router::AppState;

/// Whether `key` may download an artifact of `artifact_project`: the key
/// connects data planes, and the artifact is in its project.
fn key_may_download(key: &ApiKey, artifact_project: Option<Uuid>) -> bool {
    key_has_scope(&key.scopes, scopes::DATA_PLANE_CONNECT)
        && artifact_project == Some(key.project_id)
}

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
///
/// Takes the admin token, or the data-plane API key of the artifact's
/// project. An artifact outside the key's project is not found, so a key
/// learns nothing about other projects' artifacts.
pub async fn download_artifact(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<
    (
        StatusCode,
        [(axum::http::header::HeaderName, String); 2],
        Vec<u8>,
    ),
    Response,
> {
    let repo = ArtifactsRepository::new(state.pool.clone());
    let not_found =
        || ProblemDetails::not_found(format!("Artifact {} not found", id)).into_response();

    if !matches!(state.admin_auth, AdminAuth::Disabled) {
        let Some(token) = bearer_token(&headers) else {
            return Err(unauthorized(
                "an admin bearer token or a data-plane API key is required",
            ));
        };
        if !state.admin_auth.accepts(token) {
            let key = ApiKeysRepository::new(state.pool.clone())
                .validate_and_touch(token)
                .await
                .map_err(|e| ProblemDetails::from(e).into_response())?;
            let Some(key) = key else {
                return Err(unauthorized("the API key is invalid, expired or revoked"));
            };
            let artifact = repo
                .get(id)
                .await
                .map_err(|e| ProblemDetails::from(e).into_response())?;
            if !artifact.is_some_and(|artifact| key_may_download(&key, artifact.project_id)) {
                return Err(not_found());
            }
        }
    }

    let data = repo
        .get_data(id)
        .await
        .map_err(|e| ProblemDetails::from(e).into_response())?
        .ok_or_else(not_found)?;

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
        data,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(project_id: Uuid, scopes: &[&str]) -> ApiKey {
        ApiKey {
            id: Uuid::new_v4(),
            project_id,
            name: "data-plane".into(),
            _key_hash: String::new(),
            key_prefix: "bbk_".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            expires_at: None,
            last_used_at: None,
            created_at: chrono::Utc::now(),
            revoked_at: None,
        }
    }

    #[test]
    fn a_connect_key_downloads_its_own_projects_artifacts_only() {
        let project = Uuid::new_v4();
        let connect = key(project, &[scopes::DATA_PLANE_CONNECT]);
        assert!(key_may_download(&connect, Some(project)));
        assert!(!key_may_download(&connect, Some(Uuid::new_v4())));
        assert!(!key_may_download(&connect, None));
    }

    #[test]
    fn a_key_without_the_connect_scope_downloads_nothing() {
        let project = Uuid::new_v4();
        assert!(!key_may_download(&key(project, &[]), Some(project)));
        assert!(!key_may_download(
            &key(project, &["project:read"]),
            Some(project)
        ));
    }
}
