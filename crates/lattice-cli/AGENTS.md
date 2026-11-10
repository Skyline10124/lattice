# lattice-cli 工作指引

维护 `lattice` 二进制入口、命令行解析、文本与 JSONL 渲染及退出码，经 `lattice-api::local::LocalClient` 与 `lattice-tui` 为用户和自动化脚本提供命令行界面。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/main.rs` | `lattice` 入口、`clap` 命令定义、参数解析错误脱敏、日志初始化与进程退出码 |
| `src/commands/mod.rs` | 子命令模块导出 |
| `src/commands/run.rs` | 请求身份预生成与输出、`LocalTurn` 提交/观察/信号取消驱动、七种流式 JSONL 记录与轮末人类摘要渲染及 `0`/`1`/`2` 退出码计算 |
| `src/commands/session.rs` | `session` 子命令（`send`、`rewind`、`list`、`log`、`turn`、`usage`、`cancel`）参数处理、后台调用与用量费用渲染 |
| `src/commands/daemon.rs` | `daemon serve`、`status`、`stop` 入口与私有状态根解析 |
| `src/commands/auth.rs`、`src/commands/auth/` | `auth` 子命令、无回显终端或标准输入读取密钥、SIWC 登录浏览器与粘贴交互（`login.rs`）、可唤醒输入线程（`login/input.rs`）及信号下终端模式恢复（`terminal.rs`） |
| `src/commands/models.rs` | `models` 候选列表渲染、配置问题 stderr 提示与 `models refresh` |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [会话与 Turn](../../docs/architecture/session/README.md)、[实现模块与公开操作](../../docs/architecture/session/operations.md#实现模块与公开操作)、[工程规范](../../docs/development/engineering.md) |
| 提交、配置拒绝与 profile 选择 | [Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#请求接受)、[配置拒绝的原因](../../docs/architecture/session/acceptance.md#配置拒绝的原因)、[会话的 profile 副本](../../docs/architecture/session/settings.md#会话的-profile-副本) |
| 找回、重连、取消与后台启停 | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md)、[本地后台与访问边界](../../docs/architecture/storage.md#本地后台与访问边界) |
| `auth` 与 `models` 子命令 | [凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md#凭据命令)、[模型引用与元数据](../../docs/architecture/llm/models.md#模型候选)、[provider 配置](../../docs/architecture/llm/providers.md#endpoint-的来源) |
| `tui` 分派 | [TUI 交互与视觉](../../docs/architecture/tui/README.md#组件接线) |
| 场景运行与验收步骤 | [测试工程](../../docs/development/testing.md)、[验证指南](../../docs/development/validation-guide.md) |

## 约束

- CLI 保持配置与环境读取中立，把会话、凭据与模型操作经 `LocalClient` 交给后台执行；自动拉起后台时直接沿用 `LocalClient` 启动路径，见[配置来源与后台启动](../../docs/architecture/session/acceptance.md#配置来源与后台启动)与 [endpoint 的来源](../../docs/architecture/llm/providers.md#endpoint-的来源)。
- API key 只从无回显终端或标准输入读取，并在命令行解析错误处做脱敏拦截；SIWC 登录只输出授权 URL 与最终结果，使敏感材料隔离于命令行参数、日志、错误与标准输出，见[凭据命令](../../docs/architecture/llm/credentials.md#凭据命令)。
- `--model` 与 `--effort` 在前端只校验 `<provider>/<model>` 与档位名语法；提交前先生成并输出请求身份（新会话同时生成 `SessionId`），携带可选 `--agent`、`--model`、`--effort` 单次提交，找回与历史读取统一走后台公开只读查询，见[会话的 profile 副本](../../docs/architecture/session/settings.md#会话的-profile-副本)与[请求身份与找回](../../docs/architecture/session/recovery.md#请求身份与找回)。
- 流式命令的 `--json` stdout 只输出当前轮的 `submit`、`cancel`、`head`、`entry`、`text`、`connection`、`error` 七种 JSONL 记录；人类可读模式在轮末打印一次结局摘要，面向用户的等级等字段使用稳定的小写词，见[实现模块与公开操作](../../docs/architecture/session/operations.md#实现模块与公开操作)。
- CLI 消费 `LocalClient` 的观察 idle 诊断、`LocalTurn` 的回执等待结果与 `ConfigRejection` 已脱敏的单行 `message`，精确映射 `0`/`1`/`2` 退出码（配置拒绝原样输出 `message` 并以 `2` 退出），见[配置拒绝的原因](../../docs/architecture/session/acceptance.md#配置拒绝的原因)与[输入与传输限制](../../docs/architecture/session/concurrency.md#输入与传输限制)。
- 本 crate 之外：全屏交互界面由 `lattice-tui` 负责；profile、provider 配置、模型元数据与凭据的读取管理、连接分帧及持久 Turn 执行由 `lattice-api` 和 `lattice-llm` 负责；全系统黑盒与 release 验收由 `lattice-system-harness` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-cli` |
| 修改子命令、参数冲突或公开帮助面 | `cargo test -p lattice-cli`（常规标志类型由 `clap` 结构保证，单测锁定参数冲突与脱敏），构建 `lattice` 后运行 `cargo test -p lattice-system-harness --test fast_cli_help` |
| 修改流前拒绝、正文读取或退出码映射 | 在 `cargo test -p lattice-cli` 中同时核对 `pre-stream` 错误分类、正文读取失败与 `TurnOutcome` 的精确 `0`/`1`/`2` 退出码 |
| 修改环境与配置来源隔离 | 构建 `lattice` 后运行 `cargo test -p lattice-system-harness --test fast_env_boundary` |
| 修改 Turn 流式渲染或交互流程 | `cargo test -p lattice-api`，并按影响运行对应的 `lattice-system-harness` 场景 |
