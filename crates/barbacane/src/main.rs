//! Barbacane data plane.
//!
//! Loads a compiled `.bca` artifact at startup and processes HTTP requests
//! through the pipeline: route → validate → middleware → dispatch → respond.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "barbacane", about = "Barbacane API gateway data plane")]
struct Cli {
    /// Path to the .bca artifact file.
    #[arg(long)]
    artifact: String,

    /// Listen address.
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen: String,

    /// Enable development mode (verbose errors, detailed logs).
    #[arg(long)]
    dev: bool,

    /// Log level.
    #[arg(long, default_value = "info")]
    log_level: String,
}

#[tokio::main]
async fn main() {
    let _cli = Cli::parse();
    eprintln!("barbacane: not yet implemented (M1)");
    std::process::exit(1);
}
