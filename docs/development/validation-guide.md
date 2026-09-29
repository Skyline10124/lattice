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
export LATTICE_HARNESS_SUCCESS_DIR="$(pwd)/target/harness-success"
```

若配置了自定义 target 目录，按实际产物位置设置变量。harness 从测试进程 cwd 解析 `LATTICE_BIN`，确认是文件后规范化为绝对路径；缺失变量或无效路径不会回退 PATH。`cargo test -p lattice-system-harness` 的 cwd 是 crate 目录，不能把仓库根相对路径直接传给它。worktree 中还须防止继承主 checkout 的旧产物路径。

Source Tool guest 与 Linux 宿主分开构建：

```sh
cargo xtask tool-build --verify
```

不要用不加区分的 `cargo build --workspace` 代替宿主构建，它会尝试 host-link guest cdylib。也不要为使宿主链接通过而更改 guest 的 crate-type。Tool 生成、构建与完整性规则见[工具开发指南](tool-development.md#开发来源与生成)。

## 后台与重连操作

以下是公开 CLI 的后台与重连用法，操作示例不代表产品验收。提交新请求时，在目标项目根准备有效 profile 与凭据。找回和重连时使用原用户、状态根及会话身份；显式重试原请求还须保持原工作区和输入。状态根按[私有状态根](../architecture/storage.md#私有状态根)解析。

Session 与 TUI 命令会按需自启后台；`daemon status`、`daemon stop` 只联系已有后台，不自启。纯查询可建立连接所需运行资源，领域数据保持只读。

### 提交与找回

在目标项目根准备 `reader` 配置。首次提交不带 `--session`，必须指定 `--agent`；接受事务同时创建 Session 与首个 Turn。先生成并在终端外保存一次 TurnId，同时保存原输入和工作区位置：

```sh
read -r TURN_ID < /proc/sys/kernel/random/uuid
PROMPT="概括这个项目的用途，并指出主要入口文件和目录。"
printf 'turn=%s\n' "$TURN_ID"
lattice --json session send "$PROMPT" --agent reader --turn-id "$TURN_ID"
```

保存接受回执中的 SessionId。首次提交前它为空；JSONL 的 `submission` 记录保留原请求，`acceptance` 回执给出接受或找回结果及 SessionId。向已有 Session 发送新请求时，使用新的 TurnId：

```sh
SESSION_ID="替换为接受回执中的会话 ID"
read -r NEXT_TURN_ID < /proc/sys/kernel/random/uuid
lattice --json session send "继续检查测试入口。" --session "$SESSION_ID" --turn-id "$NEXT_TURN_ID"
```

顶层 `lattice run` 的参数与 `session send` 相同。已有 Session 默认沿用当前位置所属 Turn 的 agent，也可用 `--agent` 覆盖；当前位置为根节点 `0` 时必须指定。未指定 `--turn-id` 时，CLI 在发请求前生成并输出身份。

| 提交回复 | 下一步 |
| --- | --- |
| 已确认接受 | 继续观察，或断开后按原 TurnId 查询 |
| Busy | 保存未接受的输入，待该 Session 空闲后发送；后台不排队 |
| 接受确认未知 | 保存原身份，先查询事实；需要找回时显式重发原请求 |
| 找回已有请求 | 使用返回的原 Turn 与 Session，核对进行中状态或已关闭结果 |

例如首次提交的回执未知时，恢复原变量，在同一工作区再次执行原命令：

```sh
lattice --json session send "$PROMPT" --agent reader --turn-id "$TURN_ID"
```

不要重新运行生成 TurnId 的一行，也不要更换原输入、工作区或原有 `--session`。后台先按全局 TurnId 核验原输入及项目绑定；同请求返回原 Turn 与 Session，不加载新配置、不重跑模型或 Tool，不会多建 Session。身份或输入不符返回 Conflict。原 Turn 仍在进行时，找回只衔接观察；崩溃遗留 Turn 由后续新提交或 `rewind` 自动关闭。详见[请求身份与找回](../architecture/session.md#请求身份与找回)。

### 查询与继续观察

在新的终端恢复原 TurnId，然后选择一次读取或持续观察：

```sh
lattice --json session turn "$TURN_ID"
lattice --json session turn "$TURN_ID" --follow
```

`session turn` 按全局 TurnId 找到所属 Session，给出 TurnSummary 和全部 Step 记录，包括每次尝试、`end`、原因与节点序号；无节点的 Stop 明确显示为无。Turn 仍进行时还列 ActiveStep 与槽位进度。该详情不是按路径读取对话。已知 SessionId 时，可用 `lattice tui "$SESSION_ID"` 打开界面。

查询与已有会话重连无需当前 profile/provider 凭据。已关闭的 Turn 可读即已持久，没有额外完成确认或补屏障步骤；进行中的 Turn 即使当前无 Owner，读取也不替它关闭。

`--follow` 与 TUI 重连按“快照 + 按序事件 + 结束”观察；慢订阅落出事件窗口时由快照中的段列表重新同步。JSONL 的 `snapshot` 表示状态，`history` 给出从根到快照中当前位置的对话；不是 Git OID、journal 或另一份执行账本。历史分页只读取快照已有的节点。后台重启后未保存的半截流式文字无法找回。

TUI 中用 `/reconnect` 重新观察。临时正文保留到对应 Step 节点的正文实际装入后才替换；结局先到、正文挂起或失败时继续保留已知结局。呈现见 [TUI 连接、结局与重载](../architecture/tui.md#连接结局与重载)。

### 会话退回与分叉

用 `log` 查看完整树，再把当前位置移到选中的节点序号 N 本身：

```sh
lattice session list
lattice session log "$SESSION_ID"
NODE_ID="2"  # 替换为 log 中实际存在的目标节点
lattice session rewind "$SESSION_ID" "$NODE_ID"
lattice session send "从这里换一种思路继续。" --session "$SESSION_ID"
```

新消息以 N 为父，原分支仍在 `log` 中；`session turn` 始终能按原 TurnId 查看其完整 Step 记录。`rewind` 不恢复文件，不移动工作区历史 current。正在由 Owner 执行的同一 Session 返回 Busy，不存在的序号拒绝且位置不变。

改写某条用户消息时退回到它的父节点，改写第一条消息使用根节点 `0`：

```sh
lattice session rewind "$SESSION_ID" 0
lattice session send "改写后的第一条消息。" --session "$SESSION_ID" --agent reader
```

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

查询看到崩溃遗留的进行中 Turn 且无 live Owner 时，直接发送下一条新消息。CLI 为新任务生成新的 TurnId，后台先关闭旧 Turn，再接受并执行这条消息：

```sh
lattice --json session send "继续检查项目的测试入口。" --session "$SESSION_ID"
```

`rewind` 同样先关闭旧 Turn，再移动会话位置。同一 Session 正在由 Owner 执行时，新提交和退回返回 Busy。旧 Turn 的关闭无需当前 profile/provider；发送新消息时，客户端在旧任务关闭后按需加载新执行配置。

关闭遵循 closure-only，只依据旧持久事实收尾，不重放旧模型请求或重分发旧工具。完成后按旧 TurnId 查询结局与 Step 记录；新请求的接受回执与结果属于新 Turn。仅内存取消意图不等于持久取消，具体窗口见[自动关闭崩溃遗留任务](../architecture/session.md#自动关闭崩溃遗留任务)。

关闭失败时，本次新输入或退回停止，保留实际持久状态与恢复诊断；commit 错误可能已经持久，不能宣称必然回滚。后台先停止全库写入及新的 provider/Tool 启动，排空后关闭重开，以重读事实为准。用旧 TurnId 查询诊断；再次写操作前仍须成功完成 closure-only，不能通过重新执行旧任务绕过错误。

#### 查询或停止已有后台

```sh
lattice --json daemon status
lattice --json daemon stop
```

stop 先关闭新准入，再等待已接管工作及必要收尾。`unconfirmed_operations` 是当前后台进程记录的未完成收尾操作计数，不是持久进行中 Turn 的数量；非零时命令返回失败，继续查询相关 Session。stop 连接丢失时，用 status 核验后台是否仍在运行。最后一个前端退出不会自动 stop。

#### 隔离场景中的显式启动

需要显式运行后台时，使用 `lattice daemon serve --state-root <绝对状态根>`。参数指 XDG 状态根，产品在其下使用 `lattice/`。serve 前台运行且不加载工作区 dotenv；独立进程组、状态归属与清理遵循[测试合同](testing.md#超时与进程生命周期)，不在开发者真实状态上注入崩溃或执行清理。

客户端与后台应来自相符构建，当前 Local IPC 标识为 `LTLOCAL3`。身份或协议不兼容时保留错误，交由用户处理；不自动终止已有后台或迁移状态。旧 `sessions.git` / `invocations.git` 在打开会话库时被拒绝并列出路径，不提供迁移或兼容读取；处理状态须另获授权，见[开发状态诊断与重建边界](tool-development.md#开发状态诊断与重建边界)。

### 退出码与失败后的动作

| 退出码 | 本地命令的含义 |
| --- | --- |
| `0` | 对应命令成功；纯 `session turn` 表示 Step 详情读取成功，不表示业务 outcome 成功 |
| `1` | 执行、观察或内容读取等失败；结合结构化错误和已知结局定位，后台可能已保存结果或仍在收尾 |
| `2` | 使用/前提错误，如 BadRequest、NotFound、身份冲突；提交或持续观察也用它表示 Cancelled |

`session send` / `run` 与 `session turn --follow` 按 TurnSummary 及观察/读取诊断确定退出：成功 Completed 为 `0`，Failed 为 `1`，Cancelled 为 `2`。纯查询成功只说明读取成功，取消命令成功只说明取得取消回复。命令结果与 Turn outcome 分开解释。

即使已知 Completed，最终内容读取失败仍退出 `1` 并保留可见终态。重连读取该结果，不因这个退出码改写已完成 Turn 或重新执行任务。参数与退出分类的静态来源为 [CLI 分派](../../crates/lattice-cli/src/main.rs)、[Session 命令](../../crates/lattice-cli/src/commands/session.rs)、[提交和观察](../../crates/lattice-cli/src/commands/run.rs)及[后台命令](../../crates/lattice-cli/src/commands/daemon.rs)。

## 日常验证

先选择直接受影响的 crate 或目标：

```sh
cargo test -p lancet-authority
cargo test -p lattice-api
cargo test -p lattice-system-harness --test direct_tree_session
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
  --test direct_crash_barriers
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
cargo xtask rollout --channel alpha --out target/rollout/custom-alpha.json
```

rollout 是发布证据编排，不是人工体验层。当前 [`rollout.rs`](../../xtask/src/rollout.rs)的必需步骤为：

1. `clean_worktree`：拒绝已跟踪文件的暂存或未暂存修改。
2. `capability_probes`：核验 Rust 与 wasm target，记录宿主能力。
3. `build_release_locked`：构建 `--locked --release` 的 lattice-cli。
4. `artifact_identity`：记录产物 SHA-256 与大小。
5. `harness_boundary`。
6. `lancet_boundary`。
7. `direct_local`：以 release 产品运行默认 harness。
8. `offline_correctness`：运行协议、运行时、策略、API 和 Session 等指定 crate 测试。
9. `release_operator_matrix`：运行精确命名的 feature-gated Component 审计场景。
10. `doctor`：在隔离临时工作区保存 `lattice --json lancet doctor` 证据。

步骤初始为 blocked，执行后逐步原子更新记录。全部必需步骤实际 passed 才能 pass，failed、blocked、skipped 都不是成功，测试步骤还须有非零通过数。退出码 `0` 为通过，`1` 为执行后失败，`2` 为前提拒绝或使用错误。

## 查看证据

| 证据 | 位置及用途 |
| --- | --- |
| 本地默认 harness 失败 | `<harness crate>/target/harness-failures/<scenario>/`，以 crate 目录固定定位 |
| 本地 CI 与普通 CI 失败 | `target/harness-failures/` |
| 已采集的会话场景成功证据 | `LATTICE_HARNESS_SUCCESS_DIR` 指定目录，默认 `<harness crate>/target/harness-success/`；包含状态、树形 log、turn JSON、audit、参数与复现信息，不与失败目录混用 |
| rollout 场景失败 | `target/rollout/harness-failures/` |
| rollout 主记录 | `target/rollout/rollout-alpha-<yyyymmddTHHMMSSZ>.json`，`--out` 可改主要位置 |
| rollout 最近记录 | `target/rollout/alpha-latest.json` |
| doctor 与其他 rollout 证据 | `target/rollout/evidence/` 及记录中的 `evidence_paths` |
| Component 校验日志 | `target/rollout/component-verify.log` |
| 历史 InvocationStore 性能材料 | [历史诊断指南](invocation-store-diagnostics.md)，仅用于旧 Git ledger 证据解读，不是 redb 或 rollout 门禁 |

rollout 记录的 schema、字段与序列化由 `RolloutRecord` 维护，当前 schema version 为 1。检查 `commit_sha`、产物路径/hash/大小、开始结束时间、probes、各步骤命令/状态/耗时/证据、verdict 与 rejection，不仅看文件存在。

harness bundle 保留脱敏 stdout/stderr、command/world 和工作区前后差异；多步骤场景按序号保留命令材料。先区分主断言、命令终止与输出完整性、清理或扫描错误，再定位原因。成功场景不写失败 bundle，不能把旧目录当成本轮失败。占用锁、脱敏与历史证据保留遵循[测试工程合同](testing.md#失败证据)。

普通 CI 在失败时上传 `harness-failures-ubuntu-latest`；full-gate 总是上传 `rollout-alpha-record`，失败时另上传 `harness-failures-full-gate`。下载后核对对应提交和产物身份，不能把旧成功记录用作当前工作区验证。

`.githooks/pre-commit` 只阻止误暂存忽略文件，不运行测试。安装 hook 属于 Git 配置写操作，遵循 [Git 授权](../governance/git-operations.md)，不作为本文配方自动执行。仓库没有以覆盖率百分比或 nextest 取代上述流程的现行入口。
