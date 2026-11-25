//! Background compilation worker.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::db::{
    ArtifactsRepository, CompilationsRepository, PluginWithBinary, PluginsRepository,
    ProjectPluginConfigsRepository, SpecsRepository,
};

/// Run the compilation worker loop until `stop` turns true or the channel
/// closes. A compilation in progress finishes; the queued ones stay `pending`
/// for the next start.
pub async fn run_worker(
    pool: PgPool,
    mut rx: mpsc::Receiver<Uuid>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    tracing::info!("Compilation worker started");

    loop {
        let compilation_id = tokio::select! {
            biased;
            _ = stop.wait_for(|stop| *stop) => break,
            next = rx.recv() => match next {
                Some(id) => id,
                None => break,
            },
        };
        run_compilation(&pool, compilation_id).await;
    }

    tracing::info!("Compilation worker stopped");
}

/// Process a compilation. One that fails after it is claimed, on a database or
/// file error, is recorded as failed, so it does not stay `compiling`.
pub(crate) async fn run_compilation(pool: &PgPool, compilation_id: Uuid) {
    let Err(e) = process_compilation(pool, compilation_id).await else {
        return;
    };
    tracing::error!(compilation_id = %compilation_id, error = %e, "Compilation failed");
    let errors = serde_json::json!([{
        "code": "E1000",
        "message": "the control plane could not finish the compilation; its log has the cause",
    }]);
    if let Err(e) = CompilationsRepository::new(pool.clone())
        .fail_if_compiling(compilation_id, errors)
        .await
    {
        tracing::error!(compilation_id = %compilation_id, error = %e, "Failed to record the failure");
    }
}

/// Write a spec into its own directory under `dir`, so specs with the same file
/// name do not overwrite each other; the compiler refuses those with `E1058`.
async fn write_spec(
    dir: &Path,
    index: usize,
    filename: &str,
    content: &[u8],
) -> std::io::Result<PathBuf> {
    let spec_dir = dir.join(index.to_string());
    tokio::fs::create_dir(&spec_dir).await?;
    let path = spec_dir.join(crate::api::multipart::safe_filename(filename));
    tokio::fs::write(&path, content).await?;
    Ok(path)
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

    let temp_dir = tempfile::tempdir()?;
    let spec_path = write_spec(
        temp_dir.path(),
        0,
        &spec_revision.filename,
        &spec_revision.content,
    )
    .await?;
    let mut spec_paths = vec![spec_path];

    let additional_spec_ids: Vec<Uuid> =
        serde_json::from_value(compilation.additional_specs.clone()).unwrap_or_default();
    for (index, additional_id) in additional_spec_ids.into_iter().enumerate() {
        let Some(revision) = specs_repo.get_latest_revision(additional_id).await? else {
            compilations_repo
                .mark_failed(
                    compilation_id,
                    serde_json::json!([{
                        "code": "E1000",
                        "message": format!("the spec {additional_id} to bundle no longer exists"),
                    }]),
                )
                .await?;
            return Ok(());
        };
        spec_paths.push(
            write_spec(
                temp_dir.path(),
                index + 1,
                &revision.filename,
                &revision.content,
            )
            .await?,
        );
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

    // Run compilation with resolved plugins.
    //
    // `compile` is synchronous throughout: it reads and parses every spec,
    // compiles each schema validator, hashes the plugin binaries and writes the
    // archive. Running that on the worker that polled this task would stop it
    // serving anything else for as long as the project takes, so it goes to the
    // blocking pool. Nothing here is used afterwards except `output_path`, which
    // the caller reads back, so the inputs move in.
    let options = barbacane_compiler::CompileOptions {
        allow_plaintext: !compilation.production,
        ..Default::default()
    };
    let joined = {
        let output_path = output_path.clone();
        tokio::task::spawn_blocking(move || {
            let spec_path_refs: Vec<&Path> = spec_paths.iter().map(|p| p.as_path()).collect();
            barbacane_compiler::compile_with_capabilities(
                &spec_path_refs,
                &plugin_bundles,
                &output_path,
                &options,
            )
        })
        .await
    };

    // A panic inside the compiler arrives as a `JoinError`. Returning it here
    // would leave the row this worker claimed sitting in `compiling` forever,
    // since `run_worker` only logs what `process_compilation` returns. Record
    // the failure first, so the compilation ends in a state a caller can see.
    let compile_result = match joined {
        Ok(result) => result,
        Err(e) => {
            compilations_repo
                .mark_failed(
                    compilation_id,
                    serde_json::json!([{
                        "code": "E1000",
                        "message": format!("the compiler stopped unexpectedly: {e}"),
                    }]),
                )
                .await?;
            tracing::error!(
                compilation_id = %compilation_id,
                error = %e,
                "Compilation task did not finish"
            );
            return Ok(());
        }
    };

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
    // Read the specs asynchronously, then parse them off the runtime. Parsing is
    // YAML plus a full traversal of the document, so a large spec blocks the
    // worker exactly as `compile` did.
    let mut sources = Vec::with_capacity(spec_paths.len());
    for path in spec_paths {
        sources.push((path.clone(), tokio::fs::read_to_string(path).await?));
    }

    let referenced_plugins = tokio::task::spawn_blocking(move || {
        let mut api_specs = Vec::with_capacity(sources.len());
        for (path, content) in &sources {
            let spec = barbacane_compiler::parse_spec(content)
                .map_err(|e| anyhow::anyhow!("Failed to parse spec {}: {}", path.display(), e))?;
            api_specs.push(spec);
        }
        Ok::<_, anyhow::Error>(barbacane_compiler::extract_plugin_names(&api_specs))
    })
    .await??;
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
             Enable them in the project before compiling.",
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

        bundles.push(with_project_limits(
            registry_bundle(plugin_with_binary)?,
            &project_config.limits,
        )?);
    }

    tracing::info!(
        project_id = %project_id,
        plugins = ?referenced_plugins,
        "Resolved {} plugin(s) for compilation",
        bundles.len()
    );

    Ok(bundles)
}

/// Build a compile bundle from a registry plugin. The plugin's family,
/// capabilities and config schema travel inside its WASM, so they are read from
/// the binary the registry stores. A plugin without an embedded manifest is
/// refused: its capabilities are unknown, and the artifact enforces them.
fn registry_bundle(plugin: PluginWithBinary) -> anyhow::Result<barbacane_compiler::PluginBundle> {
    let capabilities =
        barbacane_compiler::embedded_capabilities(&plugin.name, &plugin.wasm_binary)?.ok_or_else(
            || {
                anyhow::anyhow!(
                    "E1041: plugin '{}' v{} carries no embedded manifest, so its capabilities \
                 are unknown. Rebuild it with the current barbacane-plugin-sdk.",
                    plugin.name,
                    plugin.version
                )
            },
        )?;
    let category = barbacane_compiler::embedded_category(&plugin.wasm_binary);
    let implements = barbacane_compiler::embedded_implements(&plugin.wasm_binary);
    let config_schema = barbacane_compiler::embedded_config_schema(&plugin.wasm_binary);
    let secret_fields = config_schema
        .as_ref()
        .map(|schema| {
            barbacane_compiler::artifact::collect_writeonly_fields(schema)
                .into_iter()
                .collect()
        })
        .unwrap_or_default();

    Ok(barbacane_compiler::PluginBundle {
        name: plugin.name,
        version: plugin.version,
        plugin_type: plugin.plugin_type,
        category,
        implements,
        wasm_bytes: plugin.wasm_binary,
        body_access: capabilities.body_access,
        host_functions: capabilities.host_functions,
        limits: capabilities.limits,
        secret_fields,
        config_schema,
    })
}

/// A bundle with the limits the project sets in place of the plugin's own,
/// key by key.
fn with_project_limits(
    mut bundle: barbacane_compiler::PluginBundle,
    limits: &Option<serde_json::Value>,
) -> anyhow::Result<barbacane_compiler::PluginBundle> {
    let Some(limits) = limits else {
        return Ok(bundle);
    };
    let operator = match limits {
        serde_json::Value::Object(object) => barbacane_compiler::DeclaredLimits::from_json(object),
        _ => Err("limits are an object of memory_mib, fuel and timeout_ms".to_string()),
    }
    .map_err(|reason| {
        anyhow::anyhow!(
            "E1044: the project sets invalid limits for plugin '{}': {reason}",
            bundle.name
        )
    })?;
    bundle.limits = bundle.limits.overridden_by(&operator);
    Ok(bundle)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A module whose `barbacane_manifest` custom section holds `manifest`, as
    /// the plugin macros embed `plugin.toml`.
    fn module_with_manifest(manifest: &str) -> Vec<u8> {
        fn leb128(mut n: usize, out: &mut Vec<u8>) {
            loop {
                let byte = (n & 0x7f) as u8;
                n >>= 7;
                if n == 0 {
                    out.push(byte);
                    return;
                }
                out.push(byte | 0x80);
            }
        }
        let name = b"barbacane_manifest";
        let mut section = Vec::new();
        leb128(name.len(), &mut section);
        section.extend_from_slice(name);
        section.extend_from_slice(manifest.as_bytes());

        let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        wasm.push(0x00);
        leb128(section.len(), &mut wasm);
        wasm.extend_from_slice(&section);
        wasm
    }

    fn registry_plugin(wasm_binary: Vec<u8>) -> PluginWithBinary {
        PluginWithBinary {
            name: "probe".to_string(),
            version: "1.0.0".to_string(),
            plugin_type: "middleware".to_string(),
            wasm_binary,
        }
    }

    #[test]
    fn registry_bundle_reads_capabilities_from_the_embedded_manifest() {
        let wasm = module_with_manifest(
            "[plugin]\nname = \"probe\"\nversion = \"1.0.0\"\ntype = \"middleware\"\n\n\
             [capabilities]\nbody_access = true\nhost_functions = [\"log\", \"http_call\"]\n",
        );
        let bundle = registry_bundle(registry_plugin(wasm)).expect("bundle");
        assert!(bundle.body_access);
        assert_eq!(bundle.host_functions, vec!["log", "http_call"]);
        assert!(bundle.limits.is_empty());
    }

    #[test]
    fn registry_bundle_reads_the_limits_and_refuses_invalid_ones() {
        let manifest = "[plugin]\nname = \"probe\"\nversion = \"1.0.0\"\ntype = \"middleware\"\n\n\
                        [limits]\nmemory_mib = 4\n";
        let bundle =
            registry_bundle(registry_plugin(module_with_manifest(manifest))).expect("bundle");
        assert_eq!(bundle.limits.memory_mib, Some(4));

        let invalid = manifest.replace("memory_mib = 4", "memory_mib = 0");
        let Err(err) = registry_bundle(registry_plugin(module_with_manifest(&invalid))) else {
            panic!("a plugin with invalid limits must be refused");
        };
        assert!(err.to_string().starts_with("E1044"), "{err}");
    }

    #[test]
    fn the_projects_limits_replace_the_plugins_key_by_key() {
        let manifest = "[plugin]\nname = \"probe\"\nversion = \"1.0.0\"\ntype = \"middleware\"\n\n\
                        [limits]\nmemory_mib = 4\nfuel = 5000\n";
        let bundle =
            || registry_bundle(registry_plugin(module_with_manifest(manifest))).expect("bundle");

        let unchanged = with_project_limits(bundle(), &None).expect("no limits");
        assert_eq!(unchanged.limits.memory_mib, Some(4));

        let limits = Some(serde_json::json!({"memory_mib": 64, "timeout_ms": 250}));
        let merged = with_project_limits(bundle(), &limits).expect("merged");
        assert_eq!(
            merged.limits,
            barbacane_compiler::DeclaredLimits {
                memory_mib: Some(64),
                fuel: Some(5000),
                timeout_ms: Some(250),
            }
        );

        let invalid = Some(serde_json::json!({"memory_mib": 4096}));
        let Err(err) = with_project_limits(bundle(), &invalid) else {
            panic!("limits above the ceiling must be refused");
        };
        assert!(err.to_string().starts_with("E1044"), "{err}");
    }

    #[test]
    fn registry_bundle_refuses_a_plugin_without_an_embedded_manifest() {
        let wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        let Err(err) = registry_bundle(registry_plugin(wasm)) else {
            panic!("a plugin without an embedded manifest must be refused");
        };
        assert!(err.to_string().starts_with("E1041"), "{err}");
    }

    /// Once stopped, the worker takes no queued compilation and returns.
    #[tokio::test]
    async fn a_stopped_worker_takes_no_further_compilation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(1))
            .connect_lazy("postgres://nobody@127.0.0.1:1/unreachable")
            .expect("a lazy pool needs no server");
        let (queue, jobs) = mpsc::channel(4);
        queue.send(Uuid::new_v4()).await.expect("queued");
        let (_stop, stopped) = tokio::sync::watch::channel(true);

        let worker = tokio::spawn(run_worker(pool, jobs, stopped));
        tokio::time::timeout(std::time::Duration::from_millis(500), worker)
            .await
            .expect("the worker returns at once")
            .expect("no panic");
        drop(queue);
    }

    /// The test database, migrated; `None` when `DATABASE_URL` is unreachable.
    async fn pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string()
        });
        let pool = crate::db::create_pool(&url).await.ok()?;
        crate::db::run_migrations(&pool).await.ok()?;
        Some(pool)
    }

    const EMPTY_SPEC: &str = "openapi: 3.1.0\ninfo:\n  title: t\n  version: '1'\npaths: {}\n";

    /// A project with one spec per `(file name, content)`, and their ids.
    async fn project_with_specs(pool: &PgPool, specs: &[(&str, &str)]) -> (Uuid, Vec<Uuid>) {
        let project = crate::db::ProjectsRepository::new(pool.clone())
            .create(crate::db::NewProject {
                name: format!("worker-{}", Uuid::new_v4().simple()),
                description: None,
            })
            .await
            .expect("project");
        let mut ids = Vec::new();
        for (filename, content) in specs {
            let spec = SpecsRepository::new(pool.clone())
                .create(crate::db::NewSpec {
                    project_id: project.id,
                    name: format!("spec-{}", Uuid::new_v4().simple()),
                    spec_type: "openapi".into(),
                    spec_version: "3.1.0".into(),
                    sha256: "0".repeat(64),
                    content: content.as_bytes().to_vec(),
                    filename: filename.to_string(),
                })
                .await
                .expect("spec");
            ids.push(spec.id);
        }
        (project.id, ids)
    }

    /// Run a compilation of `spec` bundling `additional`; its status and
    /// errors.
    async fn compile(
        pool: &PgPool,
        project: Uuid,
        spec: Uuid,
        additional: &[Uuid],
    ) -> (String, serde_json::Value) {
        let repo = CompilationsRepository::new(pool.clone());
        let compilation = repo
            .create(spec, Some(project), true, serde_json::json!(additional))
            .await
            .expect("compilation");
        run_compilation(pool, compilation.id).await;
        let done = repo
            .get(compilation.id)
            .await
            .expect("get")
            .expect("exists");
        (done.status, done.errors)
    }

    #[tokio::test]
    async fn a_compilation_that_fails_after_its_claim_is_failed() {
        let Some(pool) = pool().await else {
            eprintln!("skip: no database");
            return;
        };
        let (project, specs) = project_with_specs(&pool, &[("api.yaml", EMPTY_SPEC)]).await;
        sqlx::query("DELETE FROM spec_revisions WHERE spec_id = $1")
            .bind(specs[0])
            .execute(&pool)
            .await
            .expect("drop the revisions");

        let (status, errors) = compile(&pool, project, specs[0], &[]).await;
        assert_eq!(status, "failed", "{errors}");
        assert_eq!(errors[0]["code"], "E1000");
    }

    #[tokio::test]
    async fn a_spec_to_bundle_that_no_longer_exists_fails_the_compilation() {
        let Some(pool) = pool().await else {
            eprintln!("skip: no database");
            return;
        };
        let (project, specs) = project_with_specs(&pool, &[("api.yaml", EMPTY_SPEC)]).await;
        let (status, errors) = compile(&pool, project, specs[0], &[]).await;
        assert_eq!(status, "succeeded", "the spec compiles alone: {errors}");

        let gone = Uuid::new_v4();
        let (status, errors) = compile(&pool, project, specs[0], &[gone]).await;
        assert_eq!(status, "failed", "{errors}");
        assert!(
            errors[0]["message"]
                .as_str()
                .is_some_and(|m| m.contains(&gone.to_string())),
            "{errors}"
        );
    }

    #[tokio::test]
    async fn specs_with_the_same_file_name_are_refused_by_name() {
        let Some(pool) = pool().await else {
            eprintln!("skip: no database");
            return;
        };
        let other = EMPTY_SPEC.replace("title: t", "title: other");
        let (project, specs) =
            project_with_specs(&pool, &[("api.yaml", EMPTY_SPEC), ("api.yaml", &other)]).await;
        let (status, errors) = compile(&pool, project, specs[0], &specs[1..]).await;
        assert_eq!(status, "failed", "{errors}");
        assert_eq!(errors[0]["code"], "E1058", "{errors}");
    }
}
