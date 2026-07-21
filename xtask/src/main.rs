mod boundary_report;
mod ci;
mod harness_boundary;
mod lancet_boundary;
mod output;
mod path_norm;
mod repo_root;
mod scan_rs_tree;
mod tool_codegen;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "LATTICE workspace automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Local CI replica (fmt, clippy, build, harness boundary, test).
    Ci(ci::CiArgs),
    /// Verify lattice-system-harness has no product-crate dependencies.
    HarnessBoundary,
    /// LANCET architecture boundary gate.
    LancetBoundary,
    /// Generate (or check) Source Tool artifacts from manifests.
    ToolCodegen(tool_codegen::ToolCodegenArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Ci(args) => ci::run(args),
        Commands::HarnessBoundary => harness_boundary::run(),
        Commands::LancetBoundary => lancet_boundary::run(),
        Commands::ToolCodegen(args) => tool_codegen::run(args),
    }
}
