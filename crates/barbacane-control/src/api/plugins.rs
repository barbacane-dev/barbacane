//! Plugins API handlers.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::db::{ArtifactsRepository, NewPlugin, Plugin, PluginsRepository};
use crate::error::ProblemDetails;

use super::router::AppState;

#[derive(Debug, Deserialize)]
pub struct ListPluginsQuery {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub plugin_type: Option<String>,
}

/// POST /plugins - Register a new plugin
pub async fn register_plugin(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Plugin>), ProblemDetails> {
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut plugin_type: Option<String> = None;
    let mut description: Option<String> = None;
    let mut capabilities: Option<serde_json::Value> = None;
    let mut config_schema: Option<serde_json::Value> = None;
    let mut wasm_binary: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ProblemDetails::bad_request(format!("Invalid multipart data: {}", e)))?
    {
        let field_name = field.name().unwrap_or_default().to_string();

        match field_name.as_str() {
            "name" => {
                name =
                    Some(field.text().await.map_err(|e| {
                        ProblemDetails::bad_request(format!("Invalid name: {}", e))
                    })?);
            }
            "version" => {
                version =
                    Some(field.text().await.map_err(|e| {
                        ProblemDetails::bad_request(format!("Invalid version: {}", e))
                    })?);
            }
            "type" => {
                plugin_type =
                    Some(field.text().await.map_err(|e| {
                        ProblemDetails::bad_request(format!("Invalid type: {}", e))
                    })?);
            }
            "description" => {
                description = Some(field.text().await.map_err(|e| {
                    ProblemDetails::bad_request(format!("Invalid description: {}", e))
                })?);
            }
            "capabilities" => {
                let text = field.text().await.map_err(|e| {
                    ProblemDetails::bad_request(format!("Invalid capabilities: {}", e))
                })?;
                capabilities = Some(serde_json::from_str(&text).map_err(|e| {
                    ProblemDetails::bad_request(format!("Invalid capabilities JSON: {}", e))
                })?);
            }
            "config_schema" => {
                let text = field.text().await.map_err(|e| {
                    ProblemDetails::bad_request(format!("Invalid config_schema: {}", e))
                })?;
                config_schema = Some(serde_json::from_str(&text).map_err(|e| {
                    ProblemDetails::bad_request(format!("Invalid config_schema JSON: {}", e))
                })?);
            }
            "file" => {
                wasm_binary = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| {
                            ProblemDetails::bad_request(format!("Failed to read file: {}", e))
                        })?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    let name = name.ok_or_else(|| ProblemDetails::bad_request("Missing 'name' field"))?;
    let version = version.ok_or_else(|| ProblemDetails::bad_request("Missing 'version' field"))?;
    let plugin_type =
        plugin_type.ok_or_else(|| ProblemDetails::bad_request("Missing 'type' field"))?;
    let wasm_binary =
        wasm_binary.ok_or_else(|| ProblemDetails::bad_request("Missing 'file' field"))?;

    // Validate plugin type
    if plugin_type != "middleware" && plugin_type != "dispatcher" {
        return Err(ProblemDetails::bad_request(
            "Invalid plugin type. Must be 'middleware' or 'dispatcher'",
        ));
    }

    // Compute SHA256
    let mut hasher = Sha256::new();
    hasher.update(&wasm_binary);
    let sha256 = hex::encode(hasher.finalize());

    let repo = PluginsRepository::new(state.pool.clone());

    // Check if plugin already exists
    if repo.exists(&name, &version).await? {
        return Err(ProblemDetails::conflict(format!(
            "Plugin {}:{} already exists",
            name, version
        )));
    }

    let new_plugin = NewPlugin {
        name,
        version,
        plugin_type,
        description,
        capabilities: capabilities.unwrap_or(serde_json::json!([])),
        config_schema: config_schema.unwrap_or(serde_json::json!({})),
        wasm_binary,
        sha256,
    };

    let plugin = repo.create(new_plugin).await?;
    Ok((StatusCode::CREATED, Json(plugin)))
}

/// GET /plugins - List all plugins
pub async fn list_plugins(
    State(state): State<AppState>,
    Query(query): Query<ListPluginsQuery>,
) -> Result<Json<Vec<Plugin>>, ProblemDetails> {
    let repo = PluginsRepository::new(state.pool.clone());
    let plugins = repo
        .list(query.plugin_type.as_deref(), query.name.as_deref())
        .await?;
    Ok(Json(plugins))
}

/// GET /plugins/:name - List all versions of a plugin
pub async fn list_plugin_versions(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<Plugin>>, ProblemDetails> {
    let repo = PluginsRepository::new(state.pool.clone());
    let plugins = repo.list_versions(&name).await?;
    if plugins.is_empty() {
        return Err(ProblemDetails::not_found(format!(
            "Plugin {} not found",
            name
        )));
    }
    Ok(Json(plugins))
}

/// GET /plugins/:name/:version - Get specific plugin version
pub async fn get_plugin(
    State(state): State<AppState>,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<Plugin>, ProblemDetails> {
    let repo = PluginsRepository::new(state.pool.clone());
    let plugin = repo.get(&name, &version).await?.ok_or_else(|| {
        ProblemDetails::not_found(format!("Plugin {}:{} not found", name, version))
    })?;
    Ok(Json(plugin))
}

/// GET /plugins/:name/:version/download - Download plugin WASM binary
pub async fn download_plugin(
    State(state): State<AppState>,
    Path((name, version)): Path<(String, String)>,
) -> Result<
    (
        StatusCode,
        [(axum::http::header::HeaderName, String); 2],
        Vec<u8>,
    ),
    ProblemDetails,
> {
    let repo = PluginsRepository::new(state.pool.clone());
    let plugin = repo
        .get_with_binary(&name, &version)
        .await?
        .ok_or_else(|| {
            ProblemDetails::not_found(format!("Plugin {}:{} not found", name, version))
        })?;

    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/wasm".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}-{}.wasm\"", name, version),
            ),
        ],
        plugin.wasm_binary,
    ))
}

/// DELETE /plugins/:name/:version - Delete a plugin version
pub async fn delete_plugin(
    State(state): State<AppState>,
    Path((name, version)): Path<(String, String)>,
) -> Result<StatusCode, ProblemDetails> {
    // Refuse to delete if any artifact still bundles this plugin version.
    let artifacts_repo = ArtifactsRepository::new(state.pool.clone());
    if artifacts_repo
        .plugin_is_referenced(&name, &version)
        .await
        .map_err(|e| {
            ProblemDetails::internal_error_with_detail(format!(
                "Failed to check artifact references: {}",
                e
            ))
        })?
    {
        return Err(ProblemDetails::conflict(format!(
            "Plugin {}:{} is referenced by existing artifacts and cannot be deleted",
            name, version
        )));
    }

    let repo = PluginsRepository::new(state.pool.clone());
    let deleted = repo.delete(&name, &version).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ProblemDetails::not_found(format!(
            "Plugin {}:{} not found",
            name, version
        )))
    }
}
