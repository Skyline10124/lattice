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

以下是当前工作分支的公开 CLI 用法，相关[合同修订待批准](../README.md#工作分支修订状态)，操作示例不代表产品验收。提交新请求时，在目标项目根准备有效 profile 与凭据。找回和重连时使用原用户、状态根及会话身份；显式重试原请求还须保持原工作区和输入。状态根按[私有状态根](../architecture/storage.md#私有状态根)解析。

Session 与 TUI 命令会按需自启后台；`daemon status`、`daemon stop` 只联系已有后台，不自启。纯查询可建立连接所需运行资源，领域数据保持只读。

### 提交与找回

先创建会话，记下输出的 Session ID：

```sh
lattice session new --agent reader --project-branch main
```

将下面的 Session ID 替换为创建结果。首次提交只生成一次 TurnId，并在终端外保存 SessionId、TurnId、原输入及工作区位置，以便关闭终端后找回。

```sh
SESSION_ID="替换为已创建的会话 ID"
read -r TURN_ID < /proc/sys/kernel/random/uuid
PROMPT="概括这个项目的用途，并指出主要入口文件和目录。"
printf 'session=%s turn=%s\n' "$SESSION_ID" "$TURN_ID"
lattice --json session send "$SESSION_ID" "$PROMPT" --turn-id "$TURN_ID"
```

`lattice run "$SESSION_ID" "$PROMPT" --turn-id "$TURN_ID"` 使用同一发送入口。未指定 `--turn-id` 时，CLI 在发请求前生成并输出身份；JSONL 的 `submission` 记录供保存身份，独立 `acceptance` 回执说明后台是否接受。

| 提交回复 | 下一步 |
| --- | --- |
| 已确认接受 | 继续观察，或断开后按原身份查询 |
| Busy | 保存未接受的输入，待该 Session 空闲后发送；后台不排队 |
| 接受确认未知 | 保存原身份，先查询事实；需要确认时显式重发原请求 |
| 找回已有请求 | 使用返回的原 Turn，核对 pending 或已关闭结果 |

需要显式确认原请求时，恢复已保存的变量，在同一工作区再次执行同一条发送命令：

```sh
lattice --json session send "$SESSION_ID" "$PROMPT" --turn-id "$TURN_ID"
```

重试时沿用保存的 TurnId，不重新运行生成 ID 的一行。后台核验原输入和项目绑定后找回已有请求，不加载新配置、不重跑模型或 Tool，并可补必要持久确认屏障。同请求仍 pending 时，这次找回只确认原接受；崩溃遗留日志在后续新提交或会话写操作前自动关闭。输入或绑定不符返回 Conflict。身份与确认规则见[会话合同](../architecture/session.md#请求身份与确认)。

### 查询与继续观察

在新的终端中恢复保存的身份，然后选择一次读取、持续观察或 TUI：

```sh
lattice --json session turn "$SESSION_ID" "$TURN_ID"
lattice --json session turn "$SESSION_ID" "$TURN_ID" --follow
lattice tui "$SESSION_ID"
```

这些读取无需当前 profile/provider 凭据。`pending` 表示 Turn 尚未持久关闭，可读取已保存的进展；已关闭结果给出 outcome 与对应提交。后台重启后，观察已关闭结果仍由后台按 session+turn 核验固定 pin 并补必要持久屏障，成功即交付完成确认，无需重发原请求；核验失败保留身份并报告真实存储诊断。

`--follow` 中的 `gap` 是观察事件不连续通知。随后 `snapshot` 和 `transcript` 给出重新固定的状态与对话，消费方用它们替换对应临时输出。`transcript.object_id` 可指向 TurnCommit 或 pending journal，`state` 区分已关闭结果与 pending；所读消息不都是已提交 Turn。缺失旧 token 不逐个重放。

TUI 中用 `/reconnect` 重新观察。当前模型段的新后缀会继续显示，缺失前文另有提示；完成状态先显示，持久正文独立加载。呈现方式见 [TUI 连接、确认与重载](../architecture/tui.md#连接确认与重载)。单次查询保持纯只读；`--follow` 与重连走订阅，可由后台对无 Owner 的已关闭结果补持久屏障。两者都不自动恢复旧 pending。

### 取消、恢复与停止

#### 请求取消

要取消指定请求，使用原身份：

```sh
lattice --json session cancel "$SESSION_ID" "$TURN_ID"
```

`owned: true` 表示后台在判定点已接管本次取消，之后独立处理和收尾；`owned: false` 表示该判定点未接管，不表示没有后台工作。按原 TurnId 查询结果，接管回复本身不是持久 Cancelled；未知回执保留原身份，必要时再次明确取消。接管区间、明确拒绝后的新准入与持久事实边界见[会话取消规则](../architecture/session.md#取消与退出)。

CLI 提交/观察期间第一次 Ctrl+C 请求取消并继续观察，再次 Ctrl+C 结束等待或观察；完成已知而只剩正文时，Ctrl+C 放弃读取。首次取消若仍在等提交回执，本客户端的具体交互句柄 [LocalTurn](../architecture/session.md#组件与接口) 在五秒后交付提交未知并重新固定同身份只读观察，不自动重发配置；有 Live 时继续观察，无 Owner 时 CLI 可在固定读取后退出。此次未知交接后，迟到的本地提交传输错误只保留未知诊断，独立完成及正文读取继续。

TUI 的 Esc/Ctrl+C 取消、Ctrl+D 或 `/quit` 退出按[键位条件](../architecture/tui.md#键盘与历史)执行，两端共用 LocalTurn 的[回执等待预算](../architecture/session.md#输入与传输限制)。`not_sent` / `not_accepted` 只说明本客户端停止后续配置，不作为该 TurnId 全局未执行的判断；已知接受、接管和完成结果继续有效。

#### 关闭崩溃遗留的 Turn

查询看到崩溃遗留 pending 且无 live Owner 时，直接发送下一条新消息。CLI 为新任务生成新的 TurnId，后台先关闭旧 Turn，再接受并执行这条消息：

```sh
lattice --json session send "$SESSION_ID" "继续检查项目的测试入口。"
```

checkout、branch、reset、set-agent 和 rewind 也由后台先关闭旧 Turn，再执行用户选择的操作。同一 Session 正在执行时，新写操作返回 Busy。旧 Turn 的关闭无需当前 profile/provider；发送新消息时，客户端在旧任务关闭后按需加载新执行配置。

关闭遵循 closure-only，只依据旧持久事实收尾，不重放旧模型请求或重分发旧工具。完成后按旧 TurnId 查询原消息和结果；新请求的接受回执与结果属于新 Turn。取消的内存意图、已保存取消原因与已记录关闭决定分别处理，具体窗口见[自动关闭崩溃遗留任务](../architecture/session.md#自动关闭崩溃遗留任务)。

关闭失败时，本次新输入或写操作停止，后台保留实际 pending 或已可见的最终 pin，并按已核验的旧 Turn 保留恢复诊断。断连后用旧 ID 执行 `session turn --follow` 仍可取得这份错误；后续新提交或写操作从原持久停点重新核验。

#### 查询或停止已有后台

```sh
lattice --json daemon status
lattice --json daemon stop
```

stop 先关闭新准入，再等待已接管工作及必要收尾。`unconfirmed_operations` 是当前后台进程记录的未确认操作计数，不是持久 pending 数；非零时命令返回失败，继续查询相关 Session。stop 连接丢失时，用 status 核验后台是否仍在运行。最后一个前端退出不会自动 stop。

#### 隔离场景中的显式启动

需要显式运行后台时，使用 `lattice daemon serve --state-root <绝对状态根>`。参数指 XDG 状态根，产品在其下使用 `lattice/`。serve 前台运行且不加载工作区 dotenv；独立进程组、状态归属与清理遵循[测试合同](testing.md#超时与进程生命周期)，不在开发者真实状态上注入崩溃或执行清理。

客户端与后台应来自相符构建，当前 Local IPC 标识为 `LTLOCAL2`。身份或协议不兼容时保留错误，交由用户处理；不自动终止已有后台或迁移状态。当前请求找回及对话读取沿用既有 Pending v3、三库、refs、永久 pin、committed schema 与摘要，按原完整性规则读取，重连不要求重建数据。

### 退出码与失败后的动作

| 退出码 | 本地命令的含义 |
| --- | --- |
| `0` | 该命令取得对应成功结果；纯 `session turn` 表示查询和对话读取成功，不表示业务 outcome 成功，也不单独证明完成确认 |
| `1` | 执行、观察、持久确认或内容读取等失败；结合结构化错误和固定结果定位，后台可能已保存结果或仍在收尾 |
| `2` | 使用/前提错误，包括 BadRequest、NotFound 和提交身份冲突；修正输入或确认原请求身份 |

`session send` / `run` 与 `session turn --follow` 按所选 Turn 的固定 outcome 及观察/读取错误确定退出；Failed、Cancelled、Interrupted 不作为成功执行。纯查询的成功只说明读取成功，取消命令成功也只说明取得取消回复。命令级结果与 Turn outcome 分开解释。

即使已收到完成回执，内容读取失败仍退出 `1` 并保留可见终态。重连读取该结果，不因这个退出码改写已完成 Turn 或重新执行任务。参数与退出分类的静态来源为 [CLI 分派](../../crates/lattice-cli/src/main.rs)、[Session 命令](../../crates/lattice-cli/src/commands/session.rs)、[提交和观察](../../crates/lattice-cli/src/commands/run.rs)及[后台命令](../../crates/lattice-cli/src/commands/daemon.rs)。

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
