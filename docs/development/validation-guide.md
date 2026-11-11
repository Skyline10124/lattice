---
status: active
---

# 验证操作指南

本文规定动手验证的原则：用什么产物，在什么环境运行，怎样判读结果，怎样使用证据。具体操作见[手动操作卡片](scenarios/README.md#手动操作)；选择验证范围见[工程规范](engineering.md#验证)；测试分层与证据要求见[测试工程](testing.md)。

## 产物

| 原则 | 做法 |
| --- | --- |
| 构建宿主 CLI | 用 `cargo build -p lattice-cli --bin lattice` 构建被测宿主二进制 |
| 被测二进制与验证对象相符 | `LATTICE_BIN` 指向当前产物的绝对路径；`cargo test -p lattice-system-harness` 的工作目录是 crate 目录，相对路径会落在那里；worktree 中指向本 worktree 的产物。见[被测二进制](testing.md#被测二进制) |
| 测试与产品的 profile 分开 | `cargo test --release` 只切换测试自身的 profile；验证 release 产品时，先构建 release 产物并更新 `LATTICE_BIN` |
| 带 feature 的产物只用于对应场景 | 假工具与崩溃屏障场景使用带 `test-fake-tools`、`test-crash-barriers` 的产物（`cargo xtask ci` 第二轮也会用它覆盖 `target/debug/lattice`）；之后做普通验证前先重新构建无 feature 的二进制 |
| 工具链固定 | Rust 工具链版本以 [`rust-toolchain.toml`](../../rust-toolchain.toml) 为准 |

## 环境与后台

- 后台在启动时按自己的环境确定配置目录、全局 agents 目录与 TLS 根（带 `test-fake-tools` 的测试构建还会读取逗号分隔的 `LATTICE_TEST_TOOLS` 注册假工具），之后保持不变；`providers.toml` 与凭据在每次请求时重读。更换决定这些位置、TLS 根或假工具名单的环境后，先执行 `lattice daemon stop` 再启动。规则见[配置来源与后台启动](../architecture/session/acceptance.md#配置来源与后台启动)。
- 找回与重连使用原用户、状态根与会话身份；状态根的解析见[私有状态根](../architecture/storage.md#私有状态根)。
- 崩溃注入与强制清理只在隔离的状态根上进行，例如 `lattice daemon serve --state-root <绝对路径>` 启动的后台。

| 命令 | 与后台的关系 |
| --- | --- |
| `run`、`session`、`tui`、`auth`、`models` | 连接当前状态根的后台；后台未运行时自动启动 |
| `daemon status`、`daemon stop` | 只连接已有后台；后台未运行时报 `not_running` |
| `daemon serve` | 在前台运行一个后台实例 |

## 判读结果

命令的退出码与 Turn 的结局分开判读：

| 退出码 | 含义 |
| --- | --- |
| `0` | 命令成功。`run`、`session send` 与 `session turn --follow` 表示这一轮 Completed 且读取完整；纯查询只表示读取成功，业务结局看 `TurnSummary` |
| `1` | 执行、观察或读取失败：这一轮 Failed，已知 Completed 但观察或正文读取失败，回执为 `not_accepted` 或 `unknown`，以及下一行之外的后台错误（如 `Busy`、`Stopping`、`Transport`） |
| `2` | 使用错误或取消：参数错误，后台错误 `BadRequest`（含配置拒绝）、`NotFound`、`Conflict`，以及这一轮 Cancelled |

- `configure` 只发一次：结果不明时，CLI 输出 `configure` 的 `unknown` 记录与 `submit` 的 `not_accepted` 回执并退出 `1`（提交尚未发出），由用户重跑命令；TUI 显示警告，实际生效的设置以 `watch` 推送的会话头为准。规则见 [configure 的找回](../architecture/session/recovery.md#configure-的找回)。
- `submit` 的接受结果不明时，按第一条 `submit` 记录中的 TurnId 查询（`session turn`）。查到即已接受；在途就稍后再查；NotFound 才重新发送，重新发送会生成新的 TurnId。规则见[请求身份与找回](../architecture/session/recovery.md#请求身份与找回)。
- 过滤执行测试时，从输出确认目标测试实际运行；需要完整名称时加 `-- --exact`。

## 使用证据

- 只把与当前提交、产物身份相符的记录当作验证依据；下载 CI 产物后先核对对应提交。
- 读 harness bundle 时，依次区分主断言、命令终止与输出完整性、清理或扫描错误。只有失败的场景写 bundle，核对目录时间，确认属于本轮运行。
- 证据的保存与脱敏规则见[失败证据](testing.md#失败证据)，查看方法见[查看证据](scenarios/manual/evidence.md)。

## 门禁

三个门禁都执行相同的[两轮验证与带 feature 的 clippy](testing.md#验证集与显式-feature)，差异如下（具体步骤以链接的文件为准）：

| 门禁 | 触发 | 独有的检查 | 不包含 |
| --- | --- | --- | --- |
| 本地 [`xtask ci`](../../xtask/src/ci.rs) | 手动 | 工具链预检；逐 crate 测试带超时 | rollout |
| 普通 CI [`ci.yml`](../../.github/workflows/ci.yml) | `main` push 与所有 PR | 只改 `.md` 的 PR 只运行 `docs-check` | rollout |
| [`full-gate`](../../.github/workflows/full-gate.yml) | 每日定时与手动 | 运行无测试 feature 的 alpha rollout | `docs-check` |

运行重型验证之前，核对并行的编译任务、内存与磁盘余量；资源限制按[工程规范](engineering.md#验证)执行。
