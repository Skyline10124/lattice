---
status: active
---

# 验证操作指南

## 目的与范围

本指南承接旧 `docs/TESTING.md` 的工具安装、构建产物定位、运行配方和证据查看。测试归属、最小充分验证、显式 feature 和失败证据要求见[测试工程合同](testing.md)。以下命令按需要选择，不是每次修改都执行的清单；安装、产品构建、全量 CI 和 rollout 均有实际资源及写入副作用。

## 安装与构建产物

LATTICE 仅支持 Linux 宿主。当前 [Rust pin](../../rust-toolchain.toml)为 `1.96.0`，Source Tool guest 目标为 `wasm32-unknown-unknown`。Component 编码器是独立 CLI，不由项目 Cargo 依赖自动安装。版本以 [`EXPECTED_WASM_TOOLS`](../../xtask/src/ci.rs)、[full-gate 安装器](../../.github/workflows/full-gate.yml)及 `tools/*/manifest.toml` 的 toolchain 指纹为准，当前为 `1.254.0`。

需要安装时，在已获准环境中执行：

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-tools --version 1.254.0 --locked
```

`xtask ci` 预检要求 Rust pin、wasm target 及可用的 encoder。encoder 缺失或不可执行是失败；版本不符会警告可能发生摘要漂移。rollout 构建宿主 CLI，不重新编码 Components，encoder 探测只记录事实。

从仓库根构建被测宿主：

```sh
cargo build -p lattice-cli --bin lattice
export LATTICE_BIN="$(pwd)/target/debug/lattice"
export LATTICE_HARNESS_FAILURE_DIR="$(pwd)/target/harness-failures"
```

若配置了自定义 target 目录，按实际产物位置设置变量。harness 从测试进程 cwd 解析 `LATTICE_BIN`，确认是文件后规范化为绝对路径；缺失变量或无效路径不会回退 PATH。`cargo test -p lattice-system-harness` 的 cwd 是 crate 目录，不能把仓库根相对路径直接传给它。worktree 中还须防止继承主 checkout 的旧产物路径。

Source Tool guest 与 Linux 宿主分开构建：

```sh
cargo xtask tool-build --verify
```

不要用不加区分的 `cargo build --workspace` 代替宿主构建，它会尝试 host-link guest cdylib。也不要为使宿主链接通过而更改 guest 的 crate-type。Tool 生成、构建与完整性规则见[工具开发指南](tool-development.md#开发来源与生成)。

## 后台与重连操作

以下是当前工作分支的公开 CLI 用法；相关[合同修订待批准](../README.md#工作分支修订状态)，操作示例不代表产品验收。提交新请求前在目标项目根准备有效 profile 与凭据；找回请求时保持原工作区、用户及 `XDG_STATE_HOME`。Session 与 TUI 命令会按需自启后台；`daemon status`、`daemon stop` 只联系已有后台，不自启。纯查询可创建连接所需运行资源，但不创建领域库、补确认屏障或恢复 Turn。

### 提交与找回

先创建会话，记下输出的 Session ID：

```sh
lattice session new --agent reader --project-branch main
```

以下变量中的 Session ID 须替换为上一步结果。首次提交前生成一次 TurnId，另行保存该 ID、Session ID、原输入和工作区。确认未知后重试不要重新执行生成 ID 的一行。

```sh
SESSION_ID="替换为已创建的会话 ID"
read -r TURN_ID < /proc/sys/kernel/random/uuid
PROMPT="概括这个项目的用途，并指出主要入口文件和目录。"
printf 'session=%s turn=%s\n' "$SESSION_ID" "$TURN_ID"
lattice --json session send "$SESSION_ID" "$PROMPT" --turn-id "$TURN_ID"
```

`lattice run "$SESSION_ID" "$PROMPT" --turn-id "$TURN_ID"` 是同一发送入口。未提供 `--turn-id` 时，前端生成 ID 并在发请求前输出 `submission` 记录；该记录只说明客户端身份，接受须看独立回执。不同请求忙时返回 Busy，保留原任务；不排队。

重连先查询状态与固定正文，按需继续观察：

```sh
lattice --json session turn "$SESSION_ID" "$TURN_ID"
lattice --json session turn "$SESSION_ID" "$TURN_ID" --follow
lattice tui "$SESSION_ID"
```

这些操作不要求当前 profile/provider 凭据。`--follow` 的 Gap 表示未取得连续增量，后续 snapshot/transcript 是替换视图。JSONL 中 `transcript.object_id` 可以是 TurnCommit 或 pending journal OID，`state` 区分 pending 与已关闭结果；不能把所有正文对象解释成已提交 Turn。

后台重启后，结果可见与完成确认分别报告。需要显式确认原请求时，在同一工作区以原 SessionId、TurnId 和完全相同的输入重发上述 `session send`。找回已有请求不加载新配置、不重跑模型或 Tool，可补必要持久确认屏障；pending 的同请求找回不负责关闭旧日志，关闭使用独立 `recover`。输入或项目绑定不符返回 Conflict。

### 取消、恢复与停止

```sh
lattice --json session cancel "$SESSION_ID" "$TURN_ID"
lattice --json session recover "$SESSION_ID"
lattice --json daemon status
lattice --json daemon stop
```

取消输出 `owned: true` 表示后台已接管取消意图，`owned: false` 表示无对应 live 所有者；二者都不能代替持久 Cancelled。回执未知时按原 ID 查询或再次明确取消。CLI 的 Ctrl+C 请求取消，再次 Ctrl+C 结束观察；TUI 的 Esc/Ctrl+C 请求取消，Ctrl+D、`/quit` 只退出。已经发出的取消在退出前等待确认，未确认会提示未知。

`recover` 需要会话执行所有权，忙时拒绝；它不依赖当前 profile/provider，只按持久事实关闭旧 Turn。新 prompt 若先遇到旧 pending，也只报告恢复的旧 Turn，本次输入未接受。持久写入失败时保留实际 pending 或已可见的最终 pin，不删除状态；本后台按已核验的旧 Turn 保存恢复诊断，断连后用其原 ID 执行只读 `session turn --follow` 仍可取得这份错误。

stop 先关闭新准入，再等已接管工作及必要收尾。`unconfirmed_operations` 是当前后台进程记录的未确认操作计数，不是持久 pending 数；非零时命令返回失败并需查询相关 Session。stop 连接丢失不证明已停止，重新用 status 核验。最后一个前端退出不会自动 stop。

执行命令的 `0` 表示取得对应成功结果，`1` 包含执行、观察、持久确认或正文读取失败，`2` 表示使用/前提错误；以结构化错误和固定结果分别判断。纯 Turn 查询的 `0` 只表示查询及正文读取成功。即使已收到完成回执，正文读取失败仍退出 `1`，保留可见终态，待重连读取；不能据退出码把已完成 Turn 改成失败或重新执行。

需要在隔离场景显式运行后台时，使用 `lattice daemon serve --state-root <绝对状态根>`，该参数指 XDG 状态根，产品在其下使用 `lattice/`。前台运行 serve 不加载工作区 dotenv；测试中的独立进程组和清理归属按[测试合同](testing.md#超时与进程生命周期)处理，不在开发者真实状态上注入崩溃或清理。

当前 Local IPC 使用 `LTLOCAL2`，客户端与后台应来自相符构建；协议或身份不兼容会拒绝连接，不自动杀后台或迁移旧状态。请求身份与正文读取复用原 Pending v3、三库布局、refs、永久 pin、committed schema 和摘要；既有记录仍按原完整性规则读取，不需重建。Rust 直接调用者的接口变化以源码为准，不提供读取后台环境的兼容回退。

## 日常验证

先选择直接受影响的 crate 或目标：

```sh
cargo test -p lancet-authority
cargo test -p lattice-api
cargo test -p lattice-system-harness --test direct_local_turn
cargo test -p lattice-system-harness --test direct_operator
cargo test -p lattice-system-harness --test direct_background -- --list
cargo test -p lattice-system-harness --test direct_background
cargo test -p lattice-system-harness --test pty_tui_smoke
```

上述 harness 目标需要当前被测二进制，并由场景持有独立后台与清理职责。只迭代 harness 时可运行 `cargo test -p lattice-system-harness`；确需完整默认套件时运行 `cargo test --workspace`。使用过滤字符串时检查实际匹配的测试和通过数，零测试不算完成验证；需要完整名称时使用 `-- --exact`。

crate 共享测试辅助接口可能要求 `test-utils`，例如：

```sh
cargo test -p lattice-agent --features test-utils
```

release 模式的测试使用 `cargo test --workspace --release`。测试自身的 profile 与被测 `LATTICE_BIN` 是两回事；需要验证 release 产品时应先构建并显式指向对应产物。

### 显式 feature

[harness manifest](../../crates/lattice-system-harness/Cargo.toml)维护非默认目标。普通 `cargo test --workspace` 和本地 CI 不等于已运行这些目标。

```sh
cargo test -p lattice-system-harness --features release-operator \
  --test release_operator release_operator_component_matrix -- --exact
```

崩溃屏障测试还需要带产品屏障的专用二进制。先构建后显式设置路径，避免拿普通二进制运行故障场景：

```sh
cargo build -p lattice-cli --bin lattice --features test-crash-barriers
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --features crash-barriers \
  --test direct_crash_barriers --test direct_rewind_crash_barriers
```

该构建会覆盖通常的 debug 产物，后续普通验证须重新确认产物配置。测试 feature 与产品 feature 的名字不同，不能混用。

## 本地 CI 与远程工作流

```sh
cargo xtask ci
# 当前 worktree 不应继承旧的 LATTICE_BIN 时：
env -u LATTICE_BIN cargo xtask ci
```

[`xtask ci`](../../xtask/src/ci.rs)依次执行工具链预检、fmt、clippy、debug CLI 构建、harness boundary、LANCET boundary、codegen check、逐 crate 测试及 Component verify。测试按依赖顺序运行，默认 `CARGO_BUILD_JOBS=4`、每 crate `LATTICE_TEST_TIMEOUT_SECS=300`；超时回收对应进程组。控制资源不能替代问题定位，运行前核对其他编译任务、内存和磁盘，不清理他人缓存来制造通过条件。

[普通 CI](../../.github/workflows/ci.yml)在 main push 与 PR 上运行 fmt、clippy、debug CLI 构建、默认 workspace 测试，然后检查两个依赖边界与 codegen。它没有安装 encoder 或运行 Component verify。默认测试不包含上述 required-feature 目标。

[full-gate](../../.github/workflows/full-gate.yml)由手动或定时触发，另安装固定 encoder，执行 alpha rollout 和 Component verify。verify 以 `if: always()` 运行，日志保存到 `target/rollout/component-verify.log`；仅摘要漂移为 warning，其他完整性失败仍使步骤失败。不能因步骤名称含 `warn-only` 将整个步骤视为非阻塞。

## Alpha rollout

```sh
cargo xtask rollout --channel alpha
# 指定主要记录位置：
cargo xtask rollout --channel alpha --out <record-path>
```

rollout 是发布证据编排，不是人工体验层。当前 [`rollout.rs`](../../xtask/src/rollout.rs)的必需步骤为：

1. `clean_worktree`：拒绝已跟踪文件的暂存或未暂存修改。
2. `capability_probes`：核验 Rust 与 wasm target，记录宿主能力。
3. `build_release_locked`：构建 `--locked --release` 的 lattice-cli。
4. `artifact_identity`：记录产物 SHA-256 与大小。
5. `harness_boundary`。
6. `lancet_boundary`。
7. `direct_local`：以 release 产品运行默认 harness。
8. `offline_correctness`：运行协议、运行时、策略、审计、API 和 Session 等指定 crate 测试。
9. `release_operator_matrix`：运行精确命名的 feature-gated Component 审计场景。
10. `doctor`：在隔离临时工作区保存 `lattice --json lancet doctor` 证据。

步骤初始为 blocked，执行后逐步原子更新记录。全部必需步骤实际 passed 才能 pass，failed、blocked、skipped 都不是成功，测试步骤还须有非零通过数。退出码 `0` 为通过，`1` 为执行后失败，`2` 为前提拒绝或使用错误。

## 查看证据

| 证据 | 位置及用途 |
| --- | --- |
| 本地默认 harness 失败 | `<harness crate>/target/harness-failures/<scenario>/`，以 crate 目录固定定位 |
| 本地 CI 与普通 CI 失败 | `target/harness-failures/` |
| rollout 场景失败 | `target/rollout/harness-failures/` |
| rollout 主记录 | `target/rollout/rollout-alpha-<yyyymmddTHHMMSSZ>.json`，`--out` 可改主要位置 |
| rollout 最近记录 | `target/rollout/alpha-latest.json` |
| doctor 与其他 rollout 证据 | `target/rollout/evidence/` 及记录中的 `evidence_paths` |
| Component 校验日志 | `target/rollout/component-verify.log` |
| InvocationStore 性能材料 | [诊断指南](invocation-store-diagnostics.md)，不是 rollout 或产品接线的前提 |

rollout 记录的 schema、字段与序列化由 `RolloutRecord` 维护，当前 schema version 为 1。检查 `commit_sha`、产物路径/hash/大小、开始结束时间、probes、各步骤命令/状态/耗时/证据、verdict 与 rejection，不仅看文件存在。

harness bundle 保留脱敏 stdout/stderr、command/world 和工作区前后差异；多步骤场景按序号保留命令材料。先区分主断言、命令终止与输出完整性、清理或扫描错误，再定位原因。成功场景不写失败 bundle，不能把旧目录当成本轮失败。占用锁、脱敏与历史证据保留遵循[测试工程合同](testing.md#失败证据)。

普通 CI 在失败时上传 `harness-failures-ubuntu-latest`；full-gate 总是上传 `rollout-alpha-record`，失败时另上传 `harness-failures-full-gate`。下载后核对对应提交和产物身份，不能把旧成功记录用作当前工作区验证。

`.githooks/pre-commit` 只阻止误暂存忽略文件，不运行测试。安装 hook 属于 Git 配置写操作，遵循 [Git 授权](../governance/git-operations.md)，不作为本文配方自动执行。仓库没有以覆盖率百分比或 nextest 取代上述流程的现行入口。
