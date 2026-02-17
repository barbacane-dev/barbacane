//! Operations API handlers — extract and modify plugin bindings from specs.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::SpecsRepository;
use crate::error::ProblemDetails;

use super::router::AppState;

// ============================================================================
// Response types
// ============================================================================

#[derive(Debug, Serialize)]
pub struct ProjectOperationsResponse {
    pub specs: Vec<SpecOperations>,
}

#[derive(Debug, Serialize)]
pub struct SpecOperations {
    pub spec_id: Uuid,
    pub spec_name: String,
    pub spec_type: String,
    pub global_middlewares: Vec<MiddlewareBinding>,
    pub operations: Vec<OperationSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareBinding {
    pub name: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchBinding {
    pub name: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub config: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct OperationSummary {
    pub path: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<DispatchBinding>,
    /// `None` = inherits global, `Some([])` = opt-out, `Some([..])` = override.
    pub middlewares: Option<Vec<MiddlewareBinding>>,
    pub deprecated: bool,
}

// ============================================================================
// PATCH request types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct PatchSpecOperationsRequest {
    /// Replace the entire global middleware list. `None` = no change.
    #[serde(default)]
    pub global_middlewares: Option<Vec<MiddlewareBinding>>,
    /// Per-operation patches. `None` = no change.
    #[serde(default)]
    pub operations: Option<Vec<OperationPatch>>,
}

#[derive(Debug, Deserialize)]
pub struct OperationPatch {
    pub path: String,
    pub method: String,
    /// `None` (missing) = no change, `Some(None)` (null) = remove, `Some(Some(..))` = set.
    #[serde(default)]
    pub dispatch: Option<Option<DispatchBinding>>,
    /// `None` (missing) = no change, `Some(None)` (null) = revert to global,
    /// `Some(Some([]))` = opt-out, `Some(Some([..]))` = override.
    #[serde(default)]
    pub middlewares: Option<Option<Vec<MiddlewareBinding>>>,
}

// ============================================================================
// Handlers
// ============================================================================

/// GET /projects/{id}/operations — extract operations from all project specs.
pub async fn get_project_operations(
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<ProjectOperationsResponse>, ProblemDetails> {
    let repo = SpecsRepository::new(state.pool.clone());
    let specs = repo.list_for_project(project_id).await?;

    let mut spec_ops = Vec::with_capacity(specs.len());

    for spec in &specs {
        let revision = match repo.get_latest_revision(spec.id).await? {
            Some(rev) => rev,
            None => continue,
        };

        let content_str = String::from_utf8(revision.content).map_err(|_| {
            ProblemDetails::internal_error_with_detail(format!(
                "Spec '{}' content is not valid UTF-8",
                spec.name
            ))
        })?;

        let parsed = match barbacane_compiler::parse_spec(&content_str) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(spec_id = %spec.id, error = %e, "Failed to parse spec for operations view");
                continue;
            }
        };

        spec_ops.push(build_spec_operations(spec.id, &spec.name, &parsed));
    }

    Ok(Json(ProjectOperationsResponse { specs: spec_ops }))
}

/// PATCH /specs/{id}/operations — modify plugin bindings in a spec.
pub async fn patch_spec_operations(
    State(state): State<AppState>,
    Path(spec_id): Path<Uuid>,
    Json(request): Json<PatchSpecOperationsRequest>,
) -> Result<Json<SpecOperations>, ProblemDetails> {
    let repo = SpecsRepository::new(state.pool.clone());

    let spec = repo
        .get_by_id(spec_id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Spec {} not found", spec_id)))?;

    let revision = repo
        .get_latest_revision(spec_id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("No revisions for spec {}", spec_id)))?;

    let content_str = String::from_utf8(revision.content).map_err(|_| {
        ProblemDetails::internal_error_with_detail("Spec content is not valid UTF-8")
    })?;

    // Parse into serde_yaml::Value for tree manipulation
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&content_str).map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to parse YAML: {e}"))
    })?;

    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| ProblemDetails::bad_request("Spec root is not a YAML mapping"))?;

    // --- Apply global middleware changes ---
    if let Some(ref global_mws) = request.global_middlewares {
        let key = serde_yaml::Value::String("x-barbacane-middlewares".into());
        if global_mws.is_empty() {
            root.remove(&key);
        } else {
            let value = serde_yaml::to_value(global_mws).map_err(|e| {
                ProblemDetails::internal_error_with_detail(format!(
                    "Failed to serialize middlewares: {e}"
                ))
            })?;
            root.insert(key, value);
        }
    }

    // --- Apply per-operation changes ---
    if let Some(ref op_patches) = request.operations {
        // Determine the container key (OpenAPI: "paths", AsyncAPI: "channels")
        let paths_key = serde_yaml::Value::String("paths".into());
        let paths = root
            .get_mut(&paths_key)
            .and_then(|v| v.as_mapping_mut())
            .ok_or_else(|| ProblemDetails::bad_request("Spec has no 'paths' mapping"))?;

        for patch in op_patches {
            let path_key = serde_yaml::Value::String(patch.path.clone());
            let path_item = paths
                .get_mut(&path_key)
                .and_then(|v| v.as_mapping_mut())
                .ok_or_else(|| {
                    ProblemDetails::bad_request(format!("Path '{}' not found in spec", patch.path))
                })?;

            let method_key = serde_yaml::Value::String(patch.method.to_lowercase());
            let operation = path_item
                .get_mut(&method_key)
                .and_then(|v| v.as_mapping_mut())
                .ok_or_else(|| {
                    ProblemDetails::bad_request(format!(
                        "Method '{}' not found at path '{}'",
                        patch.method, patch.path
                    ))
                })?;

            apply_dispatch_patch(operation, &patch.dispatch)?;
            apply_middlewares_patch(operation, &patch.middlewares)?;
        }
    }

    // Serialize back to YAML
    let new_content_str = serde_yaml::to_string(&doc).map_err(|e| {
        ProblemDetails::internal_error_with_detail(format!("Failed to serialize YAML: {e}"))
    })?;

    // Validate by re-parsing with the compiler
    let parsed = barbacane_compiler::parse_spec(&new_content_str)
        .map_err(|e| ProblemDetails::bad_request(format!("Modified spec is invalid: {e}")))?;

    // Compute SHA256 and create new revision
    let content_bytes = new_content_str.into_bytes();
    let mut hasher = Sha256::new();
    hasher.update(&content_bytes);
    let sha256 = hex::encode(hasher.finalize());

    let spec_type_str = match parsed.format {
        barbacane_compiler::SpecFormat::OpenApi => "openapi",
        barbacane_compiler::SpecFormat::AsyncApi => "asyncapi",
    };

    repo.update(
        spec.project_id,
        &spec.name,
        spec_type_str,
        &parsed.version,
        &sha256,
        content_bytes,
        &revision.filename,
    )
    .await?;

    Ok(Json(build_spec_operations(spec.id, &spec.name, &parsed)))
}

// ============================================================================
// Helpers
// ============================================================================

/// Build a `SpecOperations` response from a parsed spec.
fn build_spec_operations(
    spec_id: Uuid,
    spec_name: &str,
    parsed: &barbacane_compiler::ApiSpec,
) -> SpecOperations {
    let spec_type = match parsed.format {
        barbacane_compiler::SpecFormat::OpenApi => "openapi",
        barbacane_compiler::SpecFormat::AsyncApi => "asyncapi",
    };

    let global_middlewares = parsed
        .global_middlewares
        .iter()
        .map(|m| MiddlewareBinding {
            name: m.name.clone(),
            config: m.config.clone(),
        })
        .collect();

    let operations = parsed
        .operations
        .iter()
        .map(|op| OperationSummary {
            path: op.path.clone(),
            method: op.method.clone(),
            operation_id: op.operation_id.clone(),
            dispatch: op.dispatch.as_ref().map(|d| DispatchBinding {
                name: d.name.clone(),
                config: d.config.clone(),
            }),
            middlewares: op.middlewares.as_ref().map(|mws| {
                mws.iter()
                    .map(|m| MiddlewareBinding {
                        name: m.name.clone(),
                        config: m.config.clone(),
                    })
                    .collect()
            }),
            deprecated: op.deprecated,
        })
        .collect();

    SpecOperations {
        spec_id,
        spec_name: spec_name.to_string(),
        spec_type: spec_type.to_string(),
        global_middlewares,
        operations,
    }
}

/// Apply dispatch changes to an operation YAML mapping.
#[allow(clippy::result_large_err)]
fn apply_dispatch_patch(
    operation: &mut serde_yaml::Mapping,
    dispatch: &Option<Option<DispatchBinding>>,
) -> Result<(), ProblemDetails> {
    let Some(ref dispatch_opt) = dispatch else {
        return Ok(());
    };

    let key = serde_yaml::Value::String("x-barbacane-dispatch".into());
    match dispatch_opt {
        None => {
            operation.remove(&key);
        }
        Some(d) => {
            let value = serde_yaml::to_value(d).map_err(|e| {
                ProblemDetails::internal_error_with_detail(format!(
                    "Failed to serialize dispatch: {e}"
                ))
            })?;
            operation.insert(key, value);
        }
    }
    Ok(())
}

/// Apply middleware changes to an operation YAML mapping.
#[allow(clippy::result_large_err)]
fn apply_middlewares_patch(
    operation: &mut serde_yaml::Mapping,
    middlewares: &Option<Option<Vec<MiddlewareBinding>>>,
) -> Result<(), ProblemDetails> {
    let Some(ref mw_opt) = middlewares else {
        return Ok(());
    };

    let key = serde_yaml::Value::String("x-barbacane-middlewares".into());
    match mw_opt {
        None => {
            // Revert to global: remove the operation-level override
            operation.remove(&key);
        }
        Some(mws) if mws.is_empty() => {
            // Explicit opt-out: empty array
            operation.insert(key, serde_yaml::Value::Sequence(vec![]));
        }
        Some(mws) => {
            let value = serde_yaml::to_value(mws).map_err(|e| {
                ProblemDetails::internal_error_with_detail(format!(
                    "Failed to serialize middlewares: {e}"
                ))
            })?;
            operation.insert(key, value);
        }
    }
    Ok(())
}
