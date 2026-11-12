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
    /// 在本地复现 CI：fmt、clippy、构建、harness 与 crate 两条边界检查、测试。
    Ci(ci::CiArgs),
    /// 检查受控 crate 的内部直接依赖白名单。
    CrateBoundary,
    /// 检查 Markdown 相对链接的目标文件与锚点。
    DocsCheck,
    /// 检查 lattice-system-harness 不依赖产品 crate，且测试专用 feature 不进入生产依赖图。
    HarnessBoundary,
    /// 从 models.dev 生成内置的模型元数据快照。
    #[cfg(feature = "model-metadata")]
    ModelMetadata,
    /// 用确定性的本地假 provider 启动真实的 TUI。
    TuiMock(tui_mock::TuiMockArgs),
    /// alpha 发布门禁：检查工作树干净、锁定依赖的 release 构建、取证步骤，并写出带版本号的 JSON 记录。
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
