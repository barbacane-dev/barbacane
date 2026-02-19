//! Projects API handlers.

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::db::{
    ArtifactsRepository, CompilationsRepository, NewProject, Project, ProjectsRepository,
    SpecsRepository, UpdateProject,
};
use crate::error::ProblemDetails;

use super::router::AppState;
use super::specs::UploadResponse;

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
    super::specs::store_spec(&state.pool, content, filename, project_id, Some(project_id)).await
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
