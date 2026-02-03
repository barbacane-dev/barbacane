//! Project plugin configuration API handlers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::db::{
    NewProjectPluginConfig, PluginsRepository, ProjectPluginConfig, ProjectPluginConfigsRepository,
    ProjectsRepository, UpdateProjectPluginConfig,
};
use crate::error::ProblemDetails;

use super::router::AppState;

/// GET /projects/:id/plugins - List plugin configurations for a project
pub async fn list_project_plugins(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ProjectPluginConfig>>, ProblemDetails> {
    // Verify project exists
    let projects_repo = ProjectsRepository::new(state.pool.clone());
    let _ = projects_repo
        .get_by_id(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", id)))?;

    let repo = ProjectPluginConfigsRepository::new(state.pool.clone());
    let configs = repo.list_for_project(id).await?;
    Ok(Json(configs))
}

/// POST /projects/:id/plugins - Add a plugin to a project
pub async fn add_plugin_to_project(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Json(input): Json<NewProjectPluginConfig>,
) -> Result<(StatusCode, Json<ProjectPluginConfig>), ProblemDetails> {
    // Verify project exists
    let projects_repo = ProjectsRepository::new(state.pool.clone());
    let _ = projects_repo
        .get_by_id(project_id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Project {} not found", project_id)))?;

    // Verify plugin exists in the global registry
    let plugins_repo = PluginsRepository::new(state.pool.clone());
    let _ = plugins_repo
        .get(&input.plugin_name, &input.plugin_version)
        .await?
        .ok_or_else(|| {
            ProblemDetails::not_found(format!(
                "Plugin {}@{} not found in registry",
                input.plugin_name, input.plugin_version
            ))
        })?;

    // Check if plugin already configured for this project
    let repo = ProjectPluginConfigsRepository::new(state.pool.clone());
    if repo.get(project_id, &input.plugin_name).await?.is_some() {
        return Err(ProblemDetails::conflict(format!(
            "Plugin '{}' is already configured for this project",
            input.plugin_name
        )));
    }

    let config = repo.create(project_id, input).await?;
    Ok((StatusCode::CREATED, Json(config)))
}

/// PUT /projects/:id/plugins/:name - Update plugin configuration
pub async fn update_project_plugin(
    State(state): State<AppState>,
    Path((project_id, plugin_name)): Path<(Uuid, String)>,
    Json(input): Json<UpdateProjectPluginConfig>,
) -> Result<Json<ProjectPluginConfig>, ProblemDetails> {
    // If changing version, verify new version exists
    if let Some(ref new_version) = input.plugin_version {
        let plugins_repo = PluginsRepository::new(state.pool.clone());
        let _ = plugins_repo
            .get(&plugin_name, new_version)
            .await?
            .ok_or_else(|| {
                ProblemDetails::not_found(format!(
                    "Plugin {}@{} not found in registry",
                    plugin_name, new_version
                ))
            })?;
    }

    let repo = ProjectPluginConfigsRepository::new(state.pool.clone());
    let config = repo
        .update(project_id, &plugin_name, input)
        .await?
        .ok_or_else(|| {
            ProblemDetails::not_found(format!(
                "Plugin '{}' not configured for project {}",
                plugin_name, project_id
            ))
        })?;
    Ok(Json(config))
}

/// DELETE /projects/:id/plugins/:name - Remove plugin from project
pub async fn remove_plugin_from_project(
    State(state): State<AppState>,
    Path((project_id, plugin_name)): Path<(Uuid, String)>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = ProjectPluginConfigsRepository::new(state.pool.clone());
    let deleted = repo.delete(project_id, &plugin_name).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ProblemDetails::not_found(format!(
            "Plugin '{}' not configured for project {}",
            plugin_name, project_id
        )))
    }
}
