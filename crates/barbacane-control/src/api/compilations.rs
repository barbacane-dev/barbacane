//! Compilations API handlers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::db::{Compilation, CompilationsRepository, SpecsRepository};
use crate::error::ProblemDetails;

use super::router::AppState;

#[derive(Debug, Deserialize)]
pub struct CompileRequest {
    /// Enable production mode checks.
    #[serde(default = "default_true")]
    pub production: bool,
    /// Additional spec IDs to bundle.
    #[serde(default)]
    pub additional_specs: Vec<Uuid>,
}

fn default_true() -> bool {
    true
}

/// POST /specs/:id/compile - Start async compilation
pub async fn start_compilation(
    State(state): State<AppState>,
    Path(spec_id): Path<Uuid>,
    Json(request): Json<CompileRequest>,
) -> Result<(StatusCode, Json<Compilation>), ProblemDetails> {
    // Verify spec exists and get project_id
    let specs_repo = SpecsRepository::new(state.pool.clone());
    let spec = specs_repo
        .get_by_id(spec_id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Spec {} not found", spec_id)))?;

    let compilations_repo = CompilationsRepository::new(state.pool.clone());

    let compilation = compilations_repo
        .create(
            spec_id,
            Some(spec.project_id),
            request.production,
            serde_json::to_value(&request.additional_specs).unwrap_or(serde_json::json!([])),
        )
        .await?;

    // Send to compilation worker via channel
    if let Some(tx) = &state.compilation_tx {
        let _ = tx.send(compilation.id).await;
    }

    Ok((StatusCode::ACCEPTED, Json(compilation)))
}

/// GET /compilations/:id - Get compilation status
pub async fn get_compilation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Compilation>, ProblemDetails> {
    let repo = CompilationsRepository::new(state.pool.clone());
    let compilation = repo
        .get(id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Compilation {} not found", id)))?;
    Ok(Json(compilation))
}

/// GET /specs/:id/compilations - List compilations for a spec
pub async fn list_spec_compilations(
    State(state): State<AppState>,
    Path(spec_id): Path<Uuid>,
) -> Result<Json<Vec<Compilation>>, ProblemDetails> {
    // Verify spec exists
    let specs_repo = SpecsRepository::new(state.pool.clone());
    let _ = specs_repo
        .get_by_id(spec_id)
        .await?
        .ok_or_else(|| ProblemDetails::not_found(format!("Spec {} not found", spec_id)))?;

    let compilations_repo = CompilationsRepository::new(state.pool.clone());
    let compilations = compilations_repo.list_for_spec(spec_id).await?;
    Ok(Json(compilations))
}

/// DELETE /compilations/:id - Delete a compilation record
pub async fn delete_compilation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ProblemDetails> {
    let repo = CompilationsRepository::new(state.pool.clone());
    let deleted = repo.delete(id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ProblemDetails::not_found(format!(
            "Compilation {} not found",
            id
        )))
    }
}
