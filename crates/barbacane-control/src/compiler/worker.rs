//! Background compilation worker.

use std::path::Path;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::db::{ArtifactsRepository, CompilationsRepository, SpecsRepository};

/// Run the compilation worker loop.
pub async fn run_worker(pool: PgPool, mut rx: mpsc::Receiver<Uuid>) {
    tracing::info!("Compilation worker started");

    while let Some(compilation_id) = rx.recv().await {
        if let Err(e) = process_compilation(&pool, compilation_id).await {
            tracing::error!(
                compilation_id = %compilation_id,
                error = %e,
                "Compilation failed"
            );
        }
    }

    tracing::info!("Compilation worker stopped");
}

/// Process a single compilation job.
async fn process_compilation(pool: &PgPool, compilation_id: Uuid) -> anyhow::Result<()> {
    let compilations_repo = CompilationsRepository::new(pool.clone());
    let specs_repo = SpecsRepository::new(pool.clone());
    let artifacts_repo = ArtifactsRepository::new(pool.clone());

    // Claim the compilation (atomically set to compiling)
    let compilation = match compilations_repo.claim(compilation_id).await? {
        Some(c) => c,
        None => {
            tracing::debug!(
                compilation_id = %compilation_id,
                "Compilation already claimed or not found"
            );
            return Ok(());
        }
    };

    tracing::info!(
        compilation_id = %compilation_id,
        spec_id = %compilation.spec_id,
        "Starting compilation"
    );

    // Get spec content
    let spec_revision = specs_repo
        .get_latest_revision(compilation.spec_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Spec revision not found"))?;

    // Write spec to temp file
    let temp_dir = tempfile::tempdir()?;
    let spec_path = temp_dir.path().join(&spec_revision.filename);
    tokio::fs::write(&spec_path, &spec_revision.content).await?;

    // Collect all spec paths
    let mut spec_paths = vec![spec_path.clone()];

    // Handle additional specs if any
    let additional_spec_ids: Vec<Uuid> =
        serde_json::from_value(compilation.additional_specs.clone()).unwrap_or_default();

    for additional_id in additional_spec_ids {
        if let Some(additional_revision) = specs_repo.get_latest_revision(additional_id).await? {
            let additional_path = temp_dir.path().join(&additional_revision.filename);
            tokio::fs::write(&additional_path, &additional_revision.content).await?;
            spec_paths.push(additional_path);
        }
    }

    // Output path for artifact
    let output_path = temp_dir.path().join("artifact.bca");

    // Run compilation
    let spec_path_refs: Vec<&Path> = spec_paths.iter().map(|p| p.as_path()).collect();
    let compile_result = barbacane_compiler::compile(&spec_path_refs, &output_path);

    match compile_result {
        Ok(manifest) => {
            // Read compiled artifact
            let artifact_data = tokio::fs::read(&output_path).await?;

            // Compute SHA256
            let mut hasher = Sha256::new();
            hasher.update(&artifact_data);
            let sha256 = hex::encode(hasher.finalize());

            // Store artifact
            let artifact = artifacts_repo
                .create(
                    serde_json::to_value(&manifest)?,
                    artifact_data,
                    &sha256,
                    barbacane_compiler::COMPILER_VERSION,
                )
                .await?;

            // Link artifact to specs
            artifacts_repo
                .link_to_spec(artifact.id, compilation.spec_id, spec_revision.revision)
                .await?;

            // Mark compilation succeeded
            compilations_repo
                .mark_succeeded(artifact.id, artifact.id, serde_json::json!([]))
                .await?;

            tracing::info!(
                compilation_id = %compilation_id,
                artifact_id = %artifact.id,
                "Compilation succeeded"
            );
        }
        Err(e) => {
            // Mark compilation failed
            let errors = serde_json::json!([{
                "code": "E1000",
                "message": e.to_string()
            }]);

            compilations_repo
                .mark_failed(compilation_id, errors)
                .await?;

            tracing::warn!(
                compilation_id = %compilation_id,
                error = %e,
                "Compilation failed"
            );
        }
    }

    Ok(())
}
