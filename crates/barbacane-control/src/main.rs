//! Barbacane control plane CLI.
//!
//! Provides `compile` and `validate` subcommands for spec compilation,
//! plus spec/artifact/plugin management when connected to a control plane server.

use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use barbacane_compiler::{compile, ARTIFACT_VERSION, COMPILER_VERSION};
use barbacane_spec_parser::parse_spec_file;

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
    /// Compile one or more specs into a .bca artifact.
    Compile {
        /// One or more spec files.
        #[arg(long, required = true, num_args = 1..)]
        specs: Vec<String>,

        /// Output artifact path.
        #[arg(short = 'o', long, default_value = "artifact.bca")]
        output: String,

        /// Enable production checks (reject http:// upstreams).
        #[arg(long, default_value_t = true)]
        production: bool,

        /// Disable production-only checks.
        #[arg(long)]
        development: bool,

        /// Show detailed compilation output.
        #[arg(long)]
        verbose: bool,
    },

    /// Validate specs without full compilation (no plugin resolution).
    Validate {
        /// One or more spec files.
        #[arg(long, required = true, num_args = 1..)]
        specs: Vec<String>,

        /// Show detailed output.
        #[arg(long)]
        verbose: bool,
    },

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Compile {
            specs,
            output,
            verbose,
            development: _,
            production: _,
        } => {
            if verbose {
                eprintln!(
                    "barbacane-control {} (artifact version {})",
                    COMPILER_VERSION, ARTIFACT_VERSION
                );
                eprintln!("Compiling {} spec(s)...", specs.len());
            }

            let spec_paths: Vec<&Path> = specs.iter().map(Path::new).collect();

            // Check all spec files exist
            for path in &spec_paths {
                if !path.exists() {
                    eprintln!("error: spec file not found: {}", path.display());
                    return ExitCode::from(3);
                }
            }

            let output_path = Path::new(&output);

            match compile(&spec_paths, output_path) {
                Ok(manifest) => {
                    if verbose {
                        eprintln!("Compiled {} route(s)", manifest.routes_count);
                        for spec in &manifest.source_specs {
                            eprintln!("  - {} ({} {})", spec.file, spec.spec_type, spec.version);
                        }
                    }
                    println!("Artifact written to: {}", output);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    // Exit codes per SPEC-001:
                    // 1 = validation error
                    // 2 = plugin resolution error
                    // 3 = I/O error
                    match e {
                        barbacane_compiler::CompileError::Parse(_)
                        | barbacane_compiler::CompileError::RoutingConflict(_)
                        | barbacane_compiler::CompileError::MissingDispatch(_)
                        | barbacane_compiler::CompileError::PlaintextUpstream(_)
                        | barbacane_compiler::CompileError::UndeclaredPlugin(_) => {
                            ExitCode::from(1)
                        }
                        barbacane_compiler::CompileError::ManifestError(_)
                        | barbacane_compiler::CompileError::PluginResolution(_) => {
                            ExitCode::from(2)
                        }
                        barbacane_compiler::CompileError::Io(_) => ExitCode::from(3),
                        barbacane_compiler::CompileError::Json(_) => ExitCode::from(1),
                    }
                }
            }
        }

        Command::Validate { specs, verbose } => {
            if verbose {
                eprintln!("Validating {} spec(s)...", specs.len());
            }

            let mut has_errors = false;

            for spec_path in &specs {
                let path = Path::new(spec_path);

                if !path.exists() {
                    eprintln!("error: spec file not found: {}", path.display());
                    has_errors = true;
                    continue;
                }

                match parse_spec_file(path) {
                    Ok(spec) => {
                        if verbose {
                            eprintln!(
                                "  {} - OK ({} {}, {} operations)",
                                spec_path,
                                match spec.format {
                                    barbacane_spec_parser::SpecFormat::OpenApi => "openapi",
                                    barbacane_spec_parser::SpecFormat::AsyncApi => "asyncapi",
                                },
                                spec.version,
                                spec.operations.len()
                            );
                        }

                        // Check for missing dispatchers
                        for op in &spec.operations {
                            if op.dispatch.is_none() {
                                eprintln!(
                                    "error[E1020]: operation has no x-barbacane-dispatch: {} {} in '{}'",
                                    op.method, op.path, spec_path
                                );
                                has_errors = true;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {} - {}", spec_path, e);
                        has_errors = true;
                    }
                }
            }

            if has_errors {
                ExitCode::from(1)
            } else {
                if !verbose {
                    println!("All specs valid.");
                }
                ExitCode::SUCCESS
            }
        }

        Command::Serve {
            listen,
            database_url,
            migrate,
        } => {
            // Initialize tracing
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("info".parse().unwrap()),
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
