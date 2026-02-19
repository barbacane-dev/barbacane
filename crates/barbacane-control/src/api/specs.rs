//! Specs API handlers.

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::{
    NewSpec, PluginsRepository, ProjectPluginConfigsRepository, Spec, SpecRevisionSummary,
    SpecsRepository,
};
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ComplianceWarning>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComplianceWarning {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// POST /specs - Upload a new spec or new revision
pub async fn upload_spec(
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ProblemDetails> {
    let (content, filename) = super::multipart::extract_file_field(multipart).await?;
    store_spec(&state.pool, content, filename, DEFAULT_PROJECT_ID, None).await
}

/// Parse, hash, upsert, and compliance-check a spec upload.
///
/// `project_id` determines where the spec is stored.
/// `check_project_id` is passed to the compliance checker to test project-level plugin
/// enablement; pass `None` to skip that check (used for the global /specs endpoint).
pub(super) async fn store_spec(
    pool: &PgPool,
    content: Vec<u8>,
    filename: String,
    project_id: Uuid,
    check_project_id: Option<Uuid>,
) -> Result<(StatusCode, Json<UploadResponse>), ProblemDetails> {
    let content_str = String::from_utf8(content.clone())
        .map_err(|_| ProblemDetails::bad_request("File is not valid UTF-8"))?;

    let parsed = barbacane_compiler::parse_spec(&content_str)
        .map_err(|e| ProblemDetails::bad_request(format!("Invalid spec: {}", e)))?;

    let mut hasher = Sha256::new();
    hasher.update(&content);
    let sha256 = hex::encode(hasher.finalize());

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

    let repo = SpecsRepository::new(pool.clone());
    let existing = repo.get_by_project_and_name(project_id, &name).await?;

    let (spec, revision) = if existing.is_some() {
        repo.update(
            project_id,
            &name,
            spec_type,
            &parsed.version,
            &sha256,
            content.clone(),
            &filename,
        )
        .await?
    } else {
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

    let warnings = check_spec_compliance(&parsed, pool, check_project_id).await;

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

// ── Spec compliance checks ──────────────────────────────────────────

/// A plugin reference extracted from x-barbacane-* extensions.
struct PluginUsage {
    name: String,
    used_as: &'static str, // "middleware" or "dispatcher"
    location: String,
    config: serde_json::Value,
}

/// Normalize a plugin name by stripping an optional `@version` suffix.
fn normalize_plugin_name(name: &str) -> String {
    match name.split_once('@') {
        Some((base, _)) => base.to_string(),
        None => name.to_string(),
    }
}

/// Extract all plugin references from a parsed spec with their usage context.
fn extract_plugin_usages(spec: &barbacane_compiler::ApiSpec) -> Vec<PluginUsage> {
    let mut usages = Vec::new();

    for mw in &spec.global_middlewares {
        usages.push(PluginUsage {
            name: normalize_plugin_name(&mw.name),
            used_as: "middleware",
            location: "global middlewares".to_string(),
            config: mw.config.clone(),
        });
    }

    for op in &spec.operations {
        let op_loc = format!("{} {}", op.method, op.path);

        if let Some(dispatch) = &op.dispatch {
            usages.push(PluginUsage {
                name: normalize_plugin_name(&dispatch.name),
                used_as: "dispatcher",
                location: op_loc.clone(),
                config: dispatch.config.clone(),
            });
        }

        if let Some(middlewares) = &op.middlewares {
            for mw in middlewares {
                usages.push(PluginUsage {
                    name: normalize_plugin_name(&mw.name),
                    used_as: "middleware",
                    location: op_loc.clone(),
                    config: mw.config.clone(),
                });
            }
        }
    }

    usages
}

/// Info about a registered plugin, used during compliance checks.
struct RegistryEntry<'a> {
    plugin_type: &'a str,
    config_schema: &'a serde_json::Value,
}

/// Check a parsed spec for compliance issues.
///
/// Returns non-blocking warnings about:
/// - `W1001` — plugin referenced but not registered in the global registry
/// - `W1002` — plugin type mismatch (e.g. dispatcher used as middleware)
/// - `W1003` — plugin referenced but not enabled in the project
/// - `W1004` — plugin config does not match the registered config schema
pub async fn check_spec_compliance(
    spec: &barbacane_compiler::ApiSpec,
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Vec<ComplianceWarning> {
    let usages = extract_plugin_usages(spec);
    if usages.is_empty() {
        return vec![];
    }

    // Load registry plugins into a name → entry map
    let plugins_repo = PluginsRepository::new(pool.clone());
    let all_plugins = match plugins_repo.list(None, None).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load plugin registry for compliance check");
            return vec![];
        }
    };
    let registry: HashMap<&str, RegistryEntry<'_>> = all_plugins
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                RegistryEntry {
                    plugin_type: p.plugin_type.as_str(),
                    config_schema: &p.config_schema,
                },
            )
        })
        .collect();

    // Optionally load project-enabled plugins
    let enabled_in_project: Option<HashSet<String>> = if let Some(pid) = project_id {
        let repo = ProjectPluginConfigsRepository::new(pool.clone());
        match repo.list_for_project(pid).await {
            Ok(configs) => Some(
                configs
                    .into_iter()
                    .filter(|c| c.enabled)
                    .map(|c| c.plugin_name)
                    .collect(),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load project plugins for compliance check");
                None
            }
        }
    } else {
        None
    };

    let mut warnings = Vec::new();
    let mut warned_once: HashSet<String> = HashSet::new();

    for usage in &usages {
        // W1001: unknown plugin
        if !registry.contains_key(usage.name.as_str()) {
            if warned_once.insert(format!("unknown:{}", usage.name)) {
                warnings.push(ComplianceWarning {
                    code: "W1001".to_string(),
                    message: format!("Plugin '{}' is not registered", usage.name),
                    location: Some(usage.location.clone()),
                });
            }
            continue;
        }

        let entry = &registry[usage.name.as_str()];

        // W1002: type mismatch
        if entry.plugin_type != usage.used_as {
            warnings.push(ComplianceWarning {
                code: "W1002".to_string(),
                message: format!(
                    "Plugin '{}' is a {} but used as {}",
                    usage.name, entry.plugin_type, usage.used_as
                ),
                location: Some(usage.location.clone()),
            });
        }

        // W1003: not enabled in project
        if let Some(ref enabled) = enabled_in_project {
            if !enabled.contains(&usage.name)
                && warned_once.insert(format!("project:{}", usage.name))
            {
                warnings.push(ComplianceWarning {
                    code: "W1003".to_string(),
                    message: format!("Plugin '{}' is not enabled in this project", usage.name),
                    location: Some(usage.location.clone()),
                });
            }
        }

        // W1004: config schema validation
        if entry.config_schema.is_object() {
            if let Ok(validator) = jsonschema::validator_for(entry.config_schema) {
                let errors: Vec<String> = validator
                    .iter_errors(&usage.config)
                    .map(|e| e.to_string())
                    .collect();
                if !errors.is_empty() {
                    warnings.push(ComplianceWarning {
                        code: "W1004".to_string(),
                        message: format!("Plugin '{}' config: {}", usage.name, errors.join("; ")),
                        location: Some(usage.location.clone()),
                    });
                }
            }
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use barbacane_compiler::{ApiSpec, DispatchConfig, MiddlewareConfig, Operation, SpecFormat};
    use std::collections::BTreeMap;

    fn empty_spec() -> ApiSpec {
        ApiSpec {
            filename: None,
            format: SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "Test".to_string(),
            api_version: "1.0.0".to_string(),
            operations: vec![],
            global_middlewares: vec![],
            extensions: BTreeMap::new(),
        }
    }

    fn test_operation(method: &str, path: &str) -> Operation {
        Operation {
            path: path.to_string(),
            method: method.to_string(),
            operation_id: None,
            parameters: vec![],
            request_body: None,
            dispatch: None,
            middlewares: None,
            deprecated: false,
            sunset: None,
            extensions: BTreeMap::new(),
            messages: vec![],
            bindings: BTreeMap::new(),
        }
    }

    #[test]
    fn normalize_plugin_name_strips_version() {
        assert_eq!(normalize_plugin_name("cors@1.0.0"), "cors");
        assert_eq!(normalize_plugin_name("rate-limit@2.3.1"), "rate-limit");
    }

    #[test]
    fn normalize_plugin_name_without_version() {
        assert_eq!(normalize_plugin_name("cors"), "cors");
        assert_eq!(normalize_plugin_name("http-upstream"), "http-upstream");
    }

    #[test]
    fn extract_usages_empty_spec() {
        let spec = empty_spec();
        let usages = extract_plugin_usages(&spec);
        assert!(usages.is_empty());
    }

    #[test]
    fn extract_usages_global_middlewares() {
        let mut spec = empty_spec();
        spec.global_middlewares = vec![
            MiddlewareConfig {
                name: "cors".to_string(),
                config: serde_json::json!({"origins": ["*"]}),
            },
            MiddlewareConfig {
                name: "rate-limit@1.0.0".to_string(),
                config: serde_json::json!({"quota": 60}),
            },
        ];

        let usages = extract_plugin_usages(&spec);
        assert_eq!(usages.len(), 2);
        assert_eq!(usages[0].name, "cors");
        assert_eq!(usages[0].used_as, "middleware");
        assert_eq!(usages[0].location, "global middlewares");
        assert_eq!(usages[1].name, "rate-limit");
    }

    #[test]
    fn extract_usages_operation_dispatch() {
        let mut spec = empty_spec();
        let mut op = test_operation("GET", "/users");
        op.dispatch = Some(DispatchConfig {
            name: "http-upstream".to_string(),
            config: serde_json::json!({"url": "http://backend:8080"}),
        });
        spec.operations.push(op);

        let usages = extract_plugin_usages(&spec);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].name, "http-upstream");
        assert_eq!(usages[0].used_as, "dispatcher");
        assert_eq!(usages[0].location, "GET /users");
    }

    #[test]
    fn extract_usages_operation_middlewares() {
        let mut spec = empty_spec();
        let mut op = test_operation("POST", "/bookings");
        op.middlewares = Some(vec![MiddlewareConfig {
            name: "auth".to_string(),
            config: serde_json::json!({}),
        }]);
        spec.operations.push(op);

        let usages = extract_plugin_usages(&spec);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].name, "auth");
        assert_eq!(usages[0].used_as, "middleware");
        assert_eq!(usages[0].location, "POST /bookings");
    }

    #[test]
    fn extract_usages_combined() {
        let mut spec = empty_spec();
        spec.global_middlewares = vec![MiddlewareConfig {
            name: "cors".to_string(),
            config: serde_json::json!({}),
        }];

        let mut op = test_operation("GET", "/stations");
        op.dispatch = Some(DispatchConfig {
            name: "http-upstream".to_string(),
            config: serde_json::json!({}),
        });
        op.middlewares = Some(vec![MiddlewareConfig {
            name: "cache".to_string(),
            config: serde_json::json!({"ttl": 300}),
        }]);
        spec.operations.push(op);

        let usages = extract_plugin_usages(&spec);
        assert_eq!(usages.len(), 3);
        assert_eq!(usages[0].name, "cors");
        assert_eq!(usages[0].used_as, "middleware");
        assert_eq!(usages[1].name, "http-upstream");
        assert_eq!(usages[1].used_as, "dispatcher");
        assert_eq!(usages[2].name, "cache");
        assert_eq!(usages[2].used_as, "middleware");
    }

    #[test]
    fn extract_usages_preserves_config() {
        let mut spec = empty_spec();
        let config = serde_json::json!({"url": "nats://localhost:4222", "subject": "events"});
        let mut op = test_operation("SEND", "/events");
        op.dispatch = Some(DispatchConfig {
            name: "nats".to_string(),
            config: config.clone(),
        });
        spec.operations.push(op);

        let usages = extract_plugin_usages(&spec);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].config, config);
    }
}
