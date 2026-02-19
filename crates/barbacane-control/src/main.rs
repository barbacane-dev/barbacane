//! Barbacane control plane CLI.
//!
//! Provides `serve` and `seed-plugins` subcommands for running the control plane server
//! and seeding the plugin registry.

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod api;
mod compiler;
mod db;
mod error;
mod server;

#[derive(Parser, Debug)]
#[command(
    name = "barbacane-control",
    about = "Barbacane control plane CLI",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the control plane HTTP server.
    Serve {
        /// Listen address.
        #[arg(long, default_value = "127.0.0.1:9090")]
        listen: SocketAddr,

        /// PostgreSQL database URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Run database migrations on startup.
        #[arg(long, default_value_t = true)]
        migrate: bool,
    },

    /// Seed the plugin registry with built-in plugins.
    SeedPlugins {
        /// Path to the plugins directory.
        #[arg(long, default_value = "plugins")]
        plugins_dir: String,

        /// PostgreSQL database URL.
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,

        /// Force re-seed plugins that already exist (update metadata and binary).
        #[arg(long)]
        force: bool,

        /// Show detailed output.
        #[arg(long)]
        verbose: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve {
            listen,
            database_url,
            migrate,
        } => {
            // Initialize tracing
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("info".parse().expect("valid log directive")),
                )
                .init();

            // Run async server
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async {
                match run_server(listen, &database_url, migrate).await {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("error: {}", e);
                        ExitCode::from(1)
                    }
                }
            })
        }

        Command::SeedPlugins {
            plugins_dir,
            database_url,
            force,
            verbose,
        } => {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async {
                match seed_plugins(&plugins_dir, &database_url, force, verbose).await {
                    Ok(count) => {
                        println!("Seeded {} plugin(s) into the registry.", count);
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        ExitCode::from(1)
                    }
                }
            })
        }
    }
}

async fn run_server(listen: SocketAddr, database_url: &str, migrate: bool) -> anyhow::Result<()> {
    // Create database pool
    let pool = db::create_pool(database_url).await?;

    // Run migrations if requested
    if migrate {
        db::run_migrations(&pool).await?;
    }

    // Start server
    server::run(server::ServerConfig {
        listen_addr: listen,
        pool,
    })
    .await
}

/// Plugin manifest from plugin.toml
#[derive(Debug, serde::Deserialize)]
struct PluginManifest {
    plugin: PluginInfo,
    capabilities: Option<toml::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct PluginInfo {
    name: String,
    version: String,
    #[serde(rename = "type")]
    plugin_type: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    wasm: Option<String>,
}

async fn seed_plugins(
    plugins_dir: &str,
    database_url: &str,
    force: bool,
    verbose: bool,
) -> anyhow::Result<usize> {
    use sha2::{Digest, Sha256};

    let plugins_path = Path::new(plugins_dir);
    if !plugins_path.exists() {
        anyhow::bail!("Plugins directory not found: {}", plugins_dir);
    }

    // Create database pool and run migrations
    let pool = db::create_pool(database_url).await?;
    db::run_migrations(&pool).await?;

    let repo = db::PluginsRepository::new(pool);
    let mut seeded_count = 0;

    // Iterate over plugin directories
    for entry in std::fs::read_dir(plugins_path)? {
        let entry = entry?;
        let plugin_path = entry.path();

        if !plugin_path.is_dir() {
            continue;
        }

        let plugin_name = plugin_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // Check for plugin.toml
        let manifest_path = plugin_path.join("plugin.toml");
        if !manifest_path.exists() {
            if verbose {
                eprintln!("  Skipping {} - no plugin.toml", plugin_name);
            }
            continue;
        }

        // Parse plugin.toml
        let manifest_content = std::fs::read_to_string(&manifest_path)?;
        let manifest: PluginManifest = toml::from_str(&manifest_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}/plugin.toml: {}", plugin_name, e))?;

        // Determine WASM filename
        let wasm_filename = manifest
            .plugin
            .wasm
            .clone()
            .unwrap_or_else(|| format!("{}.wasm", plugin_name));
        let wasm_path = plugin_path.join(&wasm_filename);

        if !wasm_path.exists() {
            if verbose {
                eprintln!(
                    "  Skipping {} - WASM file not found: {}",
                    plugin_name, wasm_filename
                );
            }
            continue;
        }

        // Check if plugin already exists
        let already_exists = repo
            .exists(&manifest.plugin.name, &manifest.plugin.version)
            .await?;
        if already_exists && !force {
            if verbose {
                eprintln!(
                    "  Skipping {} v{} - already exists (use --force to update)",
                    manifest.plugin.name, manifest.plugin.version
                );
            }
            continue;
        }

        // Read WASM binary
        let wasm_binary = std::fs::read(&wasm_path)?;

        // Compute SHA256
        let mut hasher = Sha256::new();
        hasher.update(&wasm_binary);
        let sha256 = hex::encode(hasher.finalize());

        // Read config-schema.json if exists
        let schema_path = plugin_path.join("config-schema.json");
        let config_schema: serde_json::Value = if schema_path.exists() {
            let schema_content = std::fs::read_to_string(&schema_path)?;
            serde_json::from_str(&schema_content)?
        } else {
            serde_json::json!({})
        };

        // Convert capabilities to JSON
        let capabilities = manifest
            .capabilities
            .map(|c| serde_json::to_value(&c))
            .transpose()?
            .unwrap_or(serde_json::json!([]));

        // Get description from manifest or Cargo.toml
        let description = manifest.plugin.description.or_else(|| {
            let cargo_toml_path = plugin_path.join("Cargo.toml");
            if cargo_toml_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&cargo_toml_path) {
                    if let Ok(cargo) = toml::from_str::<toml::Value>(&content) {
                        return cargo
                            .get("package")
                            .and_then(|p| p.get("description"))
                            .and_then(|d| d.as_str())
                            .map(String::from);
                    }
                }
            }
            None
        });

        // Create plugin record
        let new_plugin = db::NewPlugin {
            name: manifest.plugin.name.clone(),
            version: manifest.plugin.version.clone(),
            plugin_type: manifest.plugin.plugin_type.clone(),
            description,
            capabilities,
            config_schema,
            wasm_binary,
            sha256,
        };

        if already_exists {
            repo.upsert(new_plugin).await?;
            if verbose {
                eprintln!(
                    "  Updated {} v{} ({})",
                    manifest.plugin.name, manifest.plugin.version, manifest.plugin.plugin_type
                );
            }
        } else {
            repo.create(new_plugin).await?;
            if verbose {
                eprintln!(
                    "  Registered {} v{} ({})",
                    manifest.plugin.name, manifest.plugin.version, manifest.plugin.plugin_type
                );
            }
        }
        seeded_count += 1;
    }

    Ok(seeded_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plugin_manifest_full() {
        let toml_content = r#"
[plugin]
name = "http-upstream"
version = "0.1.0"
type = "dispatcher"
description = "HTTP upstream reverse proxy dispatcher"
wasm = "http-upstream.wasm"

[capabilities]
host_functions = ["host_http_call", "host_log"]
"#;

        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        assert_eq!(manifest.plugin.name, "http-upstream");
        assert_eq!(manifest.plugin.version, "0.1.0");
        assert_eq!(manifest.plugin.plugin_type, "dispatcher");
        assert_eq!(
            manifest.plugin.description,
            Some("HTTP upstream reverse proxy dispatcher".to_string())
        );
        assert_eq!(manifest.plugin.wasm, Some("http-upstream.wasm".to_string()));
        assert!(manifest.capabilities.is_some());
    }

    #[test]
    fn test_parse_plugin_manifest_minimal() {
        let toml_content = r#"
[plugin]
name = "mock"
version = "0.1.0"
type = "dispatcher"
"#;

        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        assert_eq!(manifest.plugin.name, "mock");
        assert_eq!(manifest.plugin.version, "0.1.0");
        assert_eq!(manifest.plugin.plugin_type, "dispatcher");
        assert!(manifest.plugin.description.is_none());
        assert!(manifest.plugin.wasm.is_none());
        assert!(manifest.capabilities.is_none());
    }

    #[test]
    fn test_parse_plugin_manifest_middleware() {
        let toml_content = r#"
[plugin]
name = "rate-limit"
version = "0.1.0"
type = "middleware"
description = "Rate limiting middleware"
wasm = "rate-limit.wasm"

[capabilities]
rate_limit = true
log = true
"#;

        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        assert_eq!(manifest.plugin.name, "rate-limit");
        assert_eq!(manifest.plugin.plugin_type, "middleware");

        // Verify capabilities can be converted to JSON
        let capabilities = manifest
            .capabilities
            .map(|c| serde_json::to_value(&c))
            .transpose()
            .unwrap()
            .unwrap_or(serde_json::json!([]));

        assert!(capabilities.is_object());
        assert_eq!(capabilities["rate_limit"], true);
        assert_eq!(capabilities["log"], true);
    }

    #[test]
    fn test_parse_plugin_manifest_with_host_functions() {
        let toml_content = r#"
[plugin]
name = "jwt-auth"
version = "0.1.0"
type = "middleware"

[capabilities]
host_functions = ["host_verify_signature"]
"#;

        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        let capabilities = manifest
            .capabilities
            .map(|c| serde_json::to_value(&c))
            .transpose()
            .unwrap()
            .unwrap_or(serde_json::json!([]));

        assert!(capabilities["host_functions"].is_array());
        assert_eq!(capabilities["host_functions"][0], "host_verify_signature");
    }

    #[test]
    fn test_wasm_filename_default() {
        let toml_content = r#"
[plugin]
name = "my-plugin"
version = "1.0.0"
type = "middleware"
"#;

        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        // When wasm is not specified, it should default to {plugin_name}.wasm
        let wasm_filename = manifest
            .plugin
            .wasm
            .clone()
            .unwrap_or_else(|| format!("{}.wasm", "my-plugin"));

        assert_eq!(wasm_filename, "my-plugin.wasm");
    }

    #[test]
    fn test_wasm_filename_explicit() {
        let toml_content = r#"
[plugin]
name = "my-plugin"
version = "1.0.0"
type = "middleware"
wasm = "custom-name.wasm"
"#;

        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        let wasm_filename = manifest
            .plugin
            .wasm
            .clone()
            .unwrap_or_else(|| format!("{}.wasm", "my-plugin"));

        assert_eq!(wasm_filename, "custom-name.wasm");
    }
}
