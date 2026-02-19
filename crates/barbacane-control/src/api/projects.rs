//! Projects API handlers.

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    Json,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{
    ArtifactsRepository, CompilationsRepository, NewProject, NewSpec, Project, ProjectsRepository,
    SpecsRepository, UpdateProject,
};
use crate::error::ProblemDetails;

use super::router::AppState;
use super::specs::{check_spec_compliance, UploadResponse};

/// POST /projects - Create a new project
pub async fn create_project(
    State(state): State<AppState>,
    Json(input): Json<NewProject>,
) -> Result<(StatusCode, Json<Project>), ProblemDetails> {
    let repo = ProjectsRepository::new(state.pool.clone());

    // Check if project with this name already exists
    if repo.get_by_name(&input.name).await?.is_some() {
        return Err(ProblemDetails::conflict(format!(
            "Project '{}' already exists",
            input.name
        )));
    }

    let project = repo.create(input).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// GET /projects - List all projects
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<Project>>, ProblemDetails> {
    let repo = ProjectsRepository::new(state.pool.clone());
    let projects = repo.list().await?;
    Ok(Json(projects))
}

/// GET /projects/:id - Get project by ID
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Project>, ProblemDetails> {
    let repo = ProjectsRepository::new(state.pool.clone());
    let project = repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", id)))?;
    Ok(Json(project))
}

/// PUT /projects/:id - Update project
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateProject>,
) -> Result<Json<Project>, ProblemDetails> {
    let repo = ProjectsRepository::new(state.pool.clone());

    // Check if new name conflicts with existing project
    if let Some(ref new_name) = input.name {
        if let Some(existing) = repo.get_by_name(new_name).await? {
            if existing.id != id {
                return Err(ProblemDetails::conflict(format!(
                    "Project '{}' already exists",
                    new_name
                )));
            }
        }
    }

    let project = repo
        .update(id, input)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", id)))?;
    Ok(Json(project))
}

/// DELETE /projects/:id - Delete project
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = ProjectsRepository::new(state.pool.clone());
    let deleted = repo.delete(id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ProblemDetails::not_found(format!(
            "Project {} not found",
            id
        )))
    }
}

/// GET /projects/:id/specs - List specs in a project
pub async fn list_project_specs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<crate::db::Spec>>, ProblemDetails> {
    // Verify project exists
    let projects_repo = ProjectsRepository::new(state.pool.clone());
    let _ = projects_repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", id)))?;

    let specs_repo = SpecsRepository::new(state.pool.clone());
    let specs = specs_repo.list_for_project(id).await?;
    Ok(Json(specs))
}

/// POST /projects/:id/specs - Upload a spec to a project
pub async fn upload_spec_to_project(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ProblemDetails> {
    // Verify project exists
    let projects_repo = ProjectsRepository::new(state.pool.clone());
    let _ = projects_repo
        .get_by_id(project_id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", project_id)))?;

    let (content, filename) = super::multipart::extract_file_field(multipart).await?;

    // Parse the spec to extract metadata
    let content_str = String::from_utf8(content.clone())
        .map_err(|_| ProblemDetails::bad_request("File is not valid UTF-8"))?;

    let parsed = barbacane_compiler::parse_spec(&content_str)
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
        barbacane_compiler::SpecFormat::OpenApi => "openapi",
        barbacane_compiler::SpecFormat::AsyncApi => "asyncapi",
    };

    let specs_repo = SpecsRepository::new(state.pool.clone());

    // Check if spec with this name exists in this project
    let existing = specs_repo
        .get_by_project_and_name(project_id, &name)
        .await?;

    let (spec, revision) = if let Some(_existing_spec) = existing {
        // Create new revision
        let (spec, revision) = specs_repo
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
        // Create new spec in project
        let new_spec = NewSpec {
            project_id,
            name: name.clone(),
            spec_type: spec_type.to_string(),
            spec_version: parsed.version.clone(),
            sha256: sha256.clone(),
            content,
            filename: filename.clone(),
        };
        let spec = specs_repo.create(new_spec).await?;
        (spec, 1)
    };

    // Run compliance checks (non-blocking — warnings only)
    let warnings = check_spec_compliance(&parsed, &state.pool, Some(project_id)).await;

    Ok((
        StatusCode::CREATED,
        Json(UploadResponse {
            id: spec.id,
            name: spec.name,
            revision,
            sha256,
            warnings,
        }),
    ))
}

/// GET /projects/:id/compilations - List compilations for a project
pub async fn list_project_compilations(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<crate::db::Compilation>>, ProblemDetails> {
    // Verify project exists
    let projects_repo = ProjectsRepository::new(state.pool.clone());
    let _ = projects_repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", id)))?;

    let compilations_repo = CompilationsRepository::new(state.pool.clone());
    let compilations = compilations_repo.list_for_project(id).await?;
    Ok(Json(compilations))
}

/// GET /projects/:id/artifacts - List artifacts for a project
pub async fn list_project_artifacts(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<crate::db::Artifact>>, ProblemDetails> {
    // Verify project exists
    let projects_repo = ProjectsRepository::new(state.pool.clone());
    let _ = projects_repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", id)))?;

    let artifacts_repo = ArtifactsRepository::new(state.pool.clone());
    let artifacts = artifacts_repo.list_for_project(id).await?;
    Ok(Json(artifacts))
}
