---
status: active
---

# 验证操作指南

本文规定动手验证的原则：用什么产物，在什么环境运行，怎样判读结果，怎样使用证据。具体操作见[手动操作卡片](scenarios/README.md#手动操作)；选择验证范围见[工程规范](engineering.md#验证)；测试分层与证据要求见[测试工程](testing.md)。

## 产物

| 原则 | 做法 |
| --- | --- |
| 宿主与 guest 分开构建 | 宿主用 `cargo build -p lattice-cli --bin lattice`；Source Tool guest 由 `cargo xtask tool-build` 构建。`cargo build --workspace` 会按宿主链接 guest 的 `cdylib`，所以宿主构建指定包与二进制 |
| 被测二进制与验证对象相符 | `LATTICE_BIN` 指向当前产物的绝对路径；`cargo test -p lattice-system-harness` 的工作目录是 crate 目录，相对路径会落在那里；worktree 中指向本 worktree 的产物。见[被测二进制](testing.md#被测二进制) |
| 测试与产品的 profile 分开 | `cargo test --release` 只切换测试自身的 profile；验证 release 产品时，先构建 release 产物并更新 `LATTICE_BIN` |
| 带 feature 的产物只用于对应场景 | 崩溃屏障场景使用带 `test-crash-barriers` 的产物；它会覆盖 `target/debug/lattice`，之后的普通验证先重新构建 |
| 工具链固定 | Rust、guest 目标与 encoder 的版本以 `rust-toolchain.toml`、[`EXPECTED_WASM_TOOLS`](../../xtask/src/ci.rs) 与 `tools/*/manifest.toml` 为准 |

## 环境与后台

- 后台在启动时按自己的环境确定配置目录、全局 agents 目录与 TLS 根，之后保持不变；`providers.toml` 与凭据在每次请求时重读。更换决定这些位置或 TLS 根的环境后，先执行 `lattice daemon stop` 再启动。规则见[配置来源与后台启动](../architecture/session/acceptance.md#配置来源与后台启动)。
- 找回与重连使用原用户、状态根与会话身份；状态根的解析见[私有状态根](../architecture/storage.md#私有状态根)。
- 崩溃注入与强制清理只在隔离的状态根上进行，例如 `lattice daemon serve --state-root <绝对路径>` 启动的后台。

| 命令 | 与后台的关系 |
| --- | --- |
| `run`、`session`、`tui`、`auth`、`models`、`audit`、`lancet status` | 连接当前状态根的后台；后台未运行时自动启动 |
| `daemon status`、`daemon stop` | 只连接已有后台；后台未运行时报 `not_running` |
| `daemon serve` | 在前台运行一个后台实例 |
| `lancet doctor` | 在本进程内自检，不连接后台 |

## 判读结果

命令的退出码与 Turn 的结局分开判读：

| 退出码 | 含义 |
| --- | --- |
| `0` | 命令成功。`run`、`session send` 与 `session turn --follow` 表示这一轮 Completed 且读取完整；纯查询只表示读取成功，业务结局看 `TurnSummary` |
| `1` | 执行、观察或读取失败：这一轮 Failed，已知 Completed 但观察或正文读取失败，回执为 `not_accepted` 或 `unknown`，以及下一行之外的后台错误（如 `Busy`、`Stopping`、`Transport`） |
| `2` | 使用错误或取消：参数错误，后台错误 `BadRequest`（含配置拒绝）、`NotFound`、`Conflict`，以及这一轮 Cancelled |

- 提交结果不明时，按第一条 `submit` 记录中的 TurnId 查询（`session turn`）。查到即已接受；在途就稍后再查；NotFound 才重新发送，重新发送会生成新的 TurnId。规则见[请求身份与找回](../architecture/session/recovery.md#请求身份与找回)。
- 过滤执行测试时，从输出确认目标测试实际运行；需要完整名称时加 `-- --exact`。

## 使用证据

- 只把与当前提交、产物身份相符的记录当作验证依据；下载 CI 产物后先核对对应提交。
- 读 harness bundle 时，依次区分主断言、命令终止与输出完整性、清理或扫描错误。只有失败的场景写 bundle，核对目录时间，确认属于本轮运行。
- 证据的保存与脱敏规则见[失败证据](testing.md#失败证据)，查看方法见[查看证据](scenarios/manual/evidence.md)。

## 门禁

三个门禁的差异如下，具体步骤以链接的文件为准：

| 门禁 | 触发 | 独有的检查 | 不包含 |
| --- | --- | --- | --- |
| 本地 [`xtask ci`](../../xtask/src/ci.rs) | 手动 | 逐 crate 测试带超时；最后运行 `tool-build --verify`，只有摘要漂移为警告 | `direct_crash_barriers`、`release_operator`、rollout |
| 普通 CI [`ci.yml`](../../.github/workflows/ci.yml) | `main` push 与所有 PR | 构建带 `test-crash-barriers` 的二进制并运行 `direct_crash_barriers`；只改 `.md` 的 PR 只运行 `docs-check` | encoder 安装、`tool-build --verify`、`release_operator`、rollout |
| [`full-gate`](../../.github/workflows/full-gate.yml) | 每日定时与手动 | 安装固定 encoder，运行 alpha rollout，并以 `if: always()` 运行 `tool-build --verify` | `docs-check`、`direct_crash_barriers` |

运行重型验证之前，核对并行的编译任务、内存与磁盘余量；资源限制按[工程规范](engineering.md#验证)执行。
