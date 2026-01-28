//! Barbacane control plane CLI.
//!
//! Provides `compile` and `validate` subcommands for spec compilation,
//! plus spec/artifact/plugin management when connected to a control plane server.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "barbacane-control", about = "Barbacane control plane CLI")]
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
        #[arg(long, default_value = "artifact.bca")]
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

    /// Validate specs without full compilation.
    Validate {
        /// One or more spec files.
        #[arg(long, required = true, num_args = 1..)]
        specs: Vec<String>,

        /// Show detailed output.
        #[arg(long)]
        verbose: bool,
    },
}

fn main() {
    let _cli = Cli::parse();
    eprintln!("barbacane-control: not yet implemented (M1)");
    std::process::exit(1);
}
