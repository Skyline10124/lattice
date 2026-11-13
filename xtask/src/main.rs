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

use clap::{ArgAction, Parser, Subcommand};

/// 顶层帮助模板：用中文标题替换 clap 默认的 `Usage`、`Commands` 与 `Options`。
const HELP_TEMPLATE: &str =
    "{about-with-newline}\n用法：{usage}\n\n命令：\n{subcommands}\n\n选项：\n{options}";
/// 子命令帮助模板：子命令只有选项一节。
const SUBCOMMAND_HELP_TEMPLATE: &str = "{about-with-newline}\n用法：{usage}\n\n选项：\n{options}";

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "LATTICE 仓库的开发自动化任务。",
    help_template = HELP_TEMPLATE,
    subcommand_value_name = "命令",
    disable_help_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// 显示帮助。
    #[arg(
        short = 'h',
        long = "help",
        action = ArgAction::Help,
        global = true,
        display_order = 1000
    )]
    help: Option<bool>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 在本地复现 CI：fmt、clippy、构建、harness 与 crate 两条边界检查、测试。
    #[command(help_template = SUBCOMMAND_HELP_TEMPLATE)]
    Ci(ci::CiArgs),
    /// 检查受控 crate 的内部直接依赖与普通依赖闭包。
    #[command(help_template = SUBCOMMAND_HELP_TEMPLATE)]
    CrateBoundary,
    /// 检查 Markdown 相对链接的目标文件与锚点。
    #[command(help_template = SUBCOMMAND_HELP_TEMPLATE)]
    DocsCheck,
    /// 检查 lattice-system-harness 不依赖产品 crate，且测试专用 feature 不进入生产依赖图。
    #[command(help_template = SUBCOMMAND_HELP_TEMPLATE)]
    HarnessBoundary,
    /// 从 models.dev 生成内置的模型元数据快照。
    #[cfg(feature = "model-metadata")]
    #[command(help_template = SUBCOMMAND_HELP_TEMPLATE)]
    ModelMetadata,
    /// 用确定性的本地假 provider 启动真实的 TUI。
    #[command(
        help_template = SUBCOMMAND_HELP_TEMPLATE,
        override_usage = "xtask tui-mock [--scenario <场景>]"
    )]
    TuiMock(tui_mock::TuiMockArgs),
    /// alpha 发布门禁：检查工作树干净、锁定依赖的 release 构建、取证步骤，并写出带版本号的 JSON 记录。
    #[command(
        help_template = SUBCOMMAND_HELP_TEMPLATE,
        override_usage = "xtask rollout --channel <通道> [--out <路径>]"
    )]
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
