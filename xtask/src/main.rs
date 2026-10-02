mod boundary_report;
mod ci;
mod ci_plan;
mod harness_boundary;
mod lancet_boundary;
mod output;
mod path_norm;
mod repo_root;
mod rollout;
mod tool_build;
mod tool_codegen;
mod tui_mock;

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
    /// Select the packages a PR's changes can affect, as `$GITHUB_OUTPUT` lines.
    CiPlan(ci_plan::CiPlanArgs),
    /// Verify lattice-system-harness has no product-crate dependencies.
    HarnessBoundary,
    /// LANCET architecture boundary gate.
    LancetBoundary,
    /// Generate (or check) Source Tool artifacts from manifests.
    ToolCodegen(tool_codegen::ToolCodegenArgs),
    /// Build a Source Tool's WASM Component and pin its artifact hash.
    ToolBuild(tool_build::ToolBuildArgs),
    /// Launch the real TUI against a deterministic local fake provider.
    TuiMock(tui_mock::TuiMockArgs),
    /// Alpha rollout gate: clean-worktree check, locked release build,
    /// evidence steps, versioned JSON record.
    Rollout(rollout::RolloutArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Ci(args) => ci::run(args),
        Commands::CiPlan(args) => ci_plan::run(args),
        Commands::HarnessBoundary => harness_boundary::run(),
        Commands::LancetBoundary => lancet_boundary::run(),
        Commands::ToolCodegen(args) => tool_codegen::run(args),
        Commands::ToolBuild(args) => tool_build::run(args),
        Commands::TuiMock(args) => tui_mock::run(args),
        Commands::Rollout(args) => rollout::run(args),
    }
}
