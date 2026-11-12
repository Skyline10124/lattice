mod boundary_report;
mod ci;
mod crate_boundary;
mod docs_check;
mod harness_boundary;
#[cfg(feature = "model-metadata")]
mod model_metadata;
mod output;
mod path_norm;
mod repo_root;
mod rollout;
mod tui_mock;
mod workspace_metadata;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "LATTICE workspace automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Local CI replica (fmt, clippy, build, harness/crate boundaries, test).
    Ci(ci::CiArgs),
    /// 检查受控 crate 的 workspace 内部直接依赖白名单。
    CrateBoundary,
    /// 检查 Markdown 相对链接的目标文件与锚点。
    DocsCheck,
    /// 检查 lattice-system-harness 不依赖产品 crate，且测试专用 feature 不进入生产依赖图。
    HarnessBoundary,
    /// Generate the built-in model metadata snapshot from models.dev.
    #[cfg(feature = "model-metadata")]
    ModelMetadata,
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
        Commands::CrateBoundary => crate_boundary::run(),
        Commands::DocsCheck => docs_check::run(),
        Commands::HarnessBoundary => harness_boundary::run(),
        #[cfg(feature = "model-metadata")]
        Commands::ModelMetadata => model_metadata::run(),
        Commands::TuiMock(args) => tui_mock::run(args),
        Commands::Rollout(args) => rollout::run(args),
    }
}
