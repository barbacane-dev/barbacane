//! Background compilation worker.

use std::path::Path;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::db::{
    ArtifactsRepository, CompilationsRepository, PluginsRepository, ProjectPluginConfigsRepository,
    SpecsRepository,
};

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
        spec_id = ?compilation.spec_id,
        project_id = ?compilation.project_id,
        "Starting compilation"
    );

    // Get spec content - require spec_id for now (project-level compilation not yet implemented)
    let spec_id = compilation
        .spec_id
        .ok_or_else(|| anyhow::anyhow!("spec_id is required for compilation"))?;

    let spec_revision = specs_repo
        .get_latest_revision(spec_id)
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

    // Resolve project plugins — validate that all plugins referenced in specs
    // are registered and enabled in the project, then load their WASM binaries.
    let plugin_bundles = if let Some(project_id) = compilation.project_id {
        match resolve_project_plugins(pool, project_id, &spec_paths).await {
            Ok(bundles) => bundles,
            Err(e) => {
                let (code, message) = parse_error_code(&e.to_string(), "E1040");

                compilations_repo
                    .mark_failed(
                        compilation_id,
                        serde_json::json!([{ "code": code, "message": message }]),
                    )
                    .await?;

                tracing::warn!(
                    compilation_id = %compilation_id,
                    error = %e,
                    "Plugin resolution failed"
                );
                return Ok(());
            }
        }
    } else {
        vec![]
    };

    // Output path for artifact
    let output_path = temp_dir.path().join("artifact.bca");

    // Run compilation with resolved plugins
    let spec_path_refs: Vec<&Path> = spec_paths.iter().map(|p| p.as_path()).collect();
    let options = barbacane_compiler::CompileOptions {
        allow_plaintext: !compilation.production,
        ..Default::default()
    };
    let compile_result =
        barbacane_compiler::compile(&spec_path_refs, &plugin_bundles, &output_path, &options);

    match compile_result {
        Ok(result) => {
            // Read compiled artifact
            let artifact_data = tokio::fs::read(&output_path).await?;

            // Compute SHA256
            let mut hasher = Sha256::new();
            hasher.update(&artifact_data);
            let sha256 = hex::encode(hasher.finalize());

            // Store artifact (with project_id if available)
            // Note: We store just the manifest, not the full result with warnings
            let artifact = artifacts_repo
                .create(
                    compilation.project_id,
                    serde_json::to_value(&result.manifest)?,
                    artifact_data,
                    &sha256,
                    barbacane_compiler::COMPILER_VERSION,
                )
                .await?;

            // Link artifact to specs
            artifacts_repo
                .link_to_spec(artifact.id, spec_id, spec_revision.revision)
                .await?;

            // Mark compilation succeeded
            compilations_repo
                .mark_succeeded(compilation_id, artifact.id, serde_json::json!([]))
                .await?;

            tracing::info!(
                compilation_id = %compilation_id,
                artifact_id = %artifact.id,
                "Compilation succeeded"
            );
        }
        Err(e) => {
            let (code, message) = parse_error_code(&e.to_string(), "E1000");

            let errors = serde_json::json!([{
                "code": code,
                "message": message
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

/// Resolve plugins for a project: validate that all spec-referenced plugins are
/// registered and enabled, then load their WASM binaries.
async fn resolve_project_plugins(
    pool: &PgPool,
    project_id: Uuid,
    spec_paths: &[std::path::PathBuf],
) -> anyhow::Result<Vec<barbacane_compiler::PluginBundle>> {
    // Parse specs to extract referenced plugin names
    let mut api_specs = Vec::new();
    for path in spec_paths {
        let content = tokio::fs::read_to_string(path).await?;
        let spec = barbacane_compiler::parse_spec(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse spec {}: {}", path.display(), e))?;
        api_specs.push(spec);
    }

    let referenced_plugins = barbacane_compiler::extract_plugin_names(&api_specs);
    if referenced_plugins.is_empty() {
        return Ok(vec![]);
    }

    // Load project's enabled plugin configs
    let project_plugins_repo = ProjectPluginConfigsRepository::new(pool.clone());
    let project_plugins = project_plugins_repo.list_for_project(project_id).await?;
    let enabled_plugin_names: std::collections::HashSet<String> = project_plugins
        .iter()
        .filter(|p| p.enabled)
        .map(|p| p.plugin_name.clone())
        .collect();

    // Check all referenced plugins are enabled in the project
    let missing: Vec<_> = referenced_plugins
        .iter()
        .filter(|name| !enabled_plugin_names.contains(*name))
        .collect();

    if !missing.is_empty() {
        let missing_list = missing
            .iter()
            .map(|s| format!("'{}'", s))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow::anyhow!(
            "E1040: plugin(s) {} used in spec but not enabled in project. \
             Add them on the Plugins page before compiling.",
            missing_list
        ));
    }

    // Load WASM binaries for referenced plugins from the global plugin registry
    let plugins_repo = PluginsRepository::new(pool.clone());
    let mut bundles = Vec::new();

    for plugin_name in &referenced_plugins {
        // Find the project plugin config to get the version
        let project_config = project_plugins
            .iter()
            .find(|p| &p.plugin_name == plugin_name)
            .ok_or_else(|| {
                anyhow::anyhow!("Plugin '{}' not found in project config", plugin_name)
            })?;

        let plugin_with_binary = plugins_repo
            .get_with_binary(&project_config.plugin_name, &project_config.plugin_version)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Plugin '{}' v{} not found in registry. Was it deleted?",
                    project_config.plugin_name,
                    project_config.plugin_version
                )
            })?;

        bundles.push(barbacane_compiler::PluginBundle {
            name: plugin_with_binary.name.clone(),
            version: plugin_with_binary.version.clone(),
            plugin_type: plugin_with_binary.plugin_type.clone(),
            wasm_bytes: plugin_with_binary.wasm_binary,
        });
    }

    tracing::info!(
        project_id = %project_id,
        plugins = ?referenced_plugins,
        "Resolved {} plugin(s) for compilation",
        bundles.len()
    );

    Ok(bundles)
}

/// Extract an error code and clean message from an error string like "E1040: some message".
/// Returns `(code, message)` where the code prefix is stripped from the message.
fn parse_error_code(error_str: &str, default_code: &str) -> (String, String) {
    match error_str.split_once(':') {
        Some((prefix, rest)) if prefix.starts_with('E') && prefix.len() == 5 => {
            (prefix.to_string(), rest.trim().to_string())
        }
        _ => (default_code.to_string(), error_str.to_string()),
    }
}
