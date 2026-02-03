//! Specs API handlers.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{NewSpec, Spec, SpecRevisionSummary, SpecsRepository};
use crate::error::ProblemDetails;

use super::router::AppState;

/// Default project UUID for backward compatibility.
/// Specs uploaded via the global /specs endpoint go to this project.
const DEFAULT_PROJECT_ID: Uuid = Uuid::from_bytes([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

#[derive(Debug, Deserialize)]
pub struct ListSpecsQuery {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub spec_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SpecResponse {
    #[serde(flatten)]
    pub spec: Spec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<SpecRevisionSummary>>,
}

#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub id: Uuid,
    pub name: String,
    pub revision: i32,
    pub sha256: String,
}

/// POST /specs - Upload a new spec or new revision
pub async fn upload_spec(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ProblemDetails> {
    let mut file_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ProblemDetails::bad_request(format!("Invalid multipart data: {}", e)))?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            filename = field.file_name().map(String::from);
            file_data = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| {
                        ProblemDetails::bad_request(format!("Failed to read file: {}", e))
                    })?
                    .to_vec(),
            );
        }
    }

    let content = file_data.ok_or_else(|| ProblemDetails::bad_request("Missing 'file' field"))?;
    let filename = filename.ok_or_else(|| ProblemDetails::bad_request("Missing filename"))?;

    // Parse the spec to extract metadata
    let content_str = String::from_utf8(content.clone())
        .map_err(|_| ProblemDetails::bad_request("File is not valid UTF-8"))?;

    let parsed = barbacane_spec_parser::parse_spec(&content_str)
        .map_err(|e| ProblemDetails::bad_request(format!("Invalid spec: {}", e)))?;

    // Compute SHA256
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let sha256 = hex::encode(hasher.finalize());

    // Extract spec name from the parsed spec or filename
    let name = if parsed.title.is_empty() {
        filename
            .trim_end_matches(".yaml")
            .trim_end_matches(".json")
            .to_string()
    } else {
        parsed.title.clone()
    };

    let spec_type = match parsed.format {
        barbacane_spec_parser::SpecFormat::OpenApi => "openapi",
        barbacane_spec_parser::SpecFormat::AsyncApi => "asyncapi",
    };

    let repo = SpecsRepository::new(state.pool.clone());

    // Use default project for backward compatibility
    let project_id = DEFAULT_PROJECT_ID;

    // Check if spec with this name exists in the default project
    let existing = repo.get_by_project_and_name(project_id, &name).await?;

    let (spec, revision) = if let Some(_existing_spec) = existing {
        // Create new revision
        let (spec, revision) = repo
            .update(
                project_id,
                &name,
                spec_type,
                &parsed.version,
                &sha256,
                content.clone(),
                &filename,
            )
            .await?;
        (spec, revision)
    } else {
        // Create new spec in default project
        let new_spec = NewSpec {
            project_id,
            name: name.clone(),
            spec_type: spec_type.to_string(),
            spec_version: parsed.version.clone(),
            sha256: sha256.clone(),
            content,
            filename: filename.clone(),
        };
        let spec = repo.create(new_spec).await?;
        (spec, 1)
    };

    Ok((
        StatusCode::CREATED,
        Json(UploadResponse {
            id: spec.id,
            name: spec.name,
            revision,
            sha256,
        }),
    ))
}

/// GET /specs - List all specs
pub async fn list_specs(
    State(state): State<AppState>,
    Query(query): Query<ListSpecsQuery>,
) -> Result<Json<Vec<Spec>>, ProblemDetails> {
    let repo = SpecsRepository::new(state.pool.clone());
    let specs = repo
        .list(query.spec_type.as_deref(), query.name.as_deref())
        .await?;
    Ok(Json(specs))
}

/// GET /specs/:id - Get spec by ID
pub async fn get_spec(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SpecResponse>, ProblemDetails> {
    let repo = SpecsRepository::new(state.pool.clone());
    let spec = repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Spec {} not found", id)))?;

    Ok(Json(SpecResponse {
        spec,
        history: None,
    }))
}

/// GET /specs/:id/history - Get spec revision history
pub async fn get_spec_history(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<SpecRevisionSummary>>, ProblemDetails> {
    let repo = SpecsRepository::new(state.pool.clone());

    // First verify spec exists
    let _ = repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Spec {} not found", id)))?;

    let history = repo.get_history(id).await?;
    Ok(Json(history))
}

/// GET /specs/:id/content - Download spec content
pub async fn download_spec_content(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<DownloadQuery>,
) -> Result<
    (
        StatusCode,
        [(axum::http::header::HeaderName, String); 2],
        Vec<u8>,
    ),
    ProblemDetails,
> {
    let repo = SpecsRepository::new(state.pool.clone());

    let revision = if let Some(rev) = query.revision {
        repo.get_revision(id, rev)
            .await?
            .ok_or_else(|| ProblemDetails::not_found(format!("Revision {} not found", rev)))?
    } else {
        repo.get_latest_revision(id)
            .await?
            .ok_or_else(|| ProblemDetails::not_found(format!("Spec {} not found", id)))?
    };

    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/yaml".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", revision.filename),
            ),
        ],
        revision.content,
    ))
}

#[derive(Debug, Deserialize)]
pub struct DownloadQuery {
    pub revision: Option<i32>,
}

/// DELETE /specs/:id - Delete spec and all revisions
pub async fn delete_spec(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = SpecsRepository::new(state.pool.clone());
    let deleted = repo.delete(id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ProblemDetails::not_found(format!("Spec {} not found", id)))
    }
}
