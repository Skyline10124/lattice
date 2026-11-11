# lattice-system-harness 工作指引

驱动已构建的 `lattice` 二进制执行全系统黑盒验收场景并提供本地 `tui_mock` 调试环境，被开发验收、`xtask ci`、`xtask rollout` 与 `xtask tui-mock` 使用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出场景世界、进程驱动、fake 服务、预算与断言辅助接口 |
| `src/world.rs`、`src/daemon.rs` | `ScenarioWorld` 临时工作区与 `HOME`/`XDG_STATE_HOME` 隔离、环境变量白名单、后台归属登记、有序 `stop` 与进程组回收 |
| `src/driver.rs`、`src/cli.rs` | `LATTICE_BIN` 路径解析、独立进程组命令执行、超时终止、`CommandOutcome` 采集与快速 CLI 辅助 |
| `src/scenario.rs` | 场景分层与单命令、整场景时长预算计算 |
| `src/fake_llm.rs`、`src/provider.rs`、`src/tls.rs` | 五协议 TLS + HTTP/2 `FakeLlmServer`、`ResponseGate`、工具 schema 校验、临时 CA 签发及经 `providers.toml` 与 `lattice auth set` 的正式配置接入 |
| `src/session_send.rs` | `lattice --json session send` JSONL 解析（含 `configure`、`submit` 等公开记录，保留未知有效记录为 `Unknown` 并单独记录畸形行）与 `0`/`1`/`2` 退出码核验 |
| `src/pty_tui.rs` | 基于 `portable-pty` 的真实 TUI 终端冒烟驱动 |
| `src/artifacts.rs`、`src/assert.rs` | 失败产物目录独占锁占用、秘密字面脱敏、工作区快照对比与 `FailureReport` 生成 |
| `src/tui_mock.rs`、`src/bin/tui_mock.rs` | 启动本地确定性 fake provider 并拉起真实 TUI 的人工调试入口 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [测试工程](../../docs/development/testing.md)、[宿主平台](../../docs/architecture/platform.md)、[工程规范](../../docs/development/engineering.md) |
| 被测二进制准备与运行命令 | [验证指南](../../docs/development/validation-guide.md)、[运行测试](../../docs/development/scenarios/manual/run-tests.md) |
| 新增或修改产品场景 | [场景卡片](../../docs/development/scenarios/README.md)：每个产品场景对应一张自动场景卡片 |
| 具体场景断言（会话、模型、TUI、工具） | 对应领域的架构合同（如[会话与 Turn](../../docs/architecture/session/README.md)、[模型接入](../../docs/architecture/llm/README.md)、[Dock 与输入区](../../docs/architecture/tui/dock.md#选择面板)、[移除旧 LANCET 执行器 ADR](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md)） |

## 约束

- `Cargo.toml` 只引入通用测试、网络、TLS 与终端库，保持零 `lattice-*` 产品 crate 依赖，由 `cargo xtask harness-boundary` 守卫黑盒边界。
- 场景只通过 `LATTICE_BIN`（推荐传入绝对路径）调用已构建产品，经公开 CLI、JSONL 与公开 Local IPC 核验行为。
- 目标平台固定为 Linux，环境隔离、环境变量白名单、`ScenarioWorld` 后台提前登记与前后台进程组清理、`ResponseGate`、预算、失败产物占用与脱敏均按[测试工程](../../docs/development/testing.md)执行；运行环境异常（含 PTY 环境错误）直接计为测试失败。
- `FakeLlmServer` 经场景私有 `HOME` 下的 `providers.toml` 与 `lattice auth set` 接入正式配置入口；验证变量未设置（如 `fast_env_boundary`）时调用 `unset_env` 移除变量，确保真正测试缺失态而非空字符串。
- `src/` 只承载跨场景通用驱动，Direct 共用辅助放在 `tests/support/mod.rs`，场景专用 fixture 与语义断言保留在各自 `tests/*.rs` 中。需要工具循环的场景以 `#[cfg(feature = "fake-tools")]` 标记，配合 `--features test-fake-tools` 构建的 `lattice` 二进制，并在后台首次启动前经 `ScenarioWorld::set_env("LATTICE_TEST_TOOLS", ...)` 设置逗号分隔的工具名单。
- 本 crate 之外：未公开的 redb 表编码、值版本、纯关闭规划与内部状态机分支由 `lattice-session`、`lattice-llm`、`lattice-api` 等对应产品 crate 的单元或集成测试负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 修改 `src/` 通用驱动或解析器 | `cargo test -p lattice-system-harness --lib` 与 `cargo xtask harness-boundary` |
| 修改 CLI 帮助、环境变量或参数场景 | 先构建 `lattice`，再运行对应的 `cargo test -p lattice-system-harness --test fast_<场景>` |
| 修改后台重连、会话树或 provider 默认场景 | 先构建无测试 feature 的 `lattice`，再按影响范围运行对应的 `cargo test -p lattice-system-harness --test direct_<场景>` |
| 修改工具循环场景或 `tui_mock` 工具分支 | 先构建 `cargo build -p lattice-cli --bin lattice --features test-fake-tools`，再运行对应的 `cargo test -p lattice-system-harness --features fake-tools --test direct_<场景>` |
| 修改 TUI 冒烟驱动或 `pty_tui_smoke` | 默认非工具子集：构建无测试 feature 的 `lattice` 后运行 `cargo test -p lattice-system-harness --test pty_tui_smoke`；含假工具块与面板重试的全集：构建 `--features test-fake-tools` 的 `lattice` 后运行 `cargo test -p lattice-system-harness --features fake-tools --test pty_tui_smoke` |
| 修改崩溃窗口场景 | 先构建带 `--features test-fake-tools,test-crash-barriers` 的 `lattice`，运行 `cargo test -p lattice-system-harness --features fake-tools,crash-barriers --test direct_crash_barriers`，并在报告中注明 |
