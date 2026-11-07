---
status: active
---

# 验证操作指南

本文提供构建、运行、验证与查看证据的命令和结果判读。验证范围按[工程规范](engineering.md#验证)选择，测试分层、进程资源与证据要求见[测试工程](testing.md)。工具安装、产品构建、本地 CI 与 rollout 会消耗编译资源并写入构建产物，按实际验证需要选用。

## 安装与构建产物

宿主仅运行在 Linux 上（见[宿主平台](../architecture/platform.md#平台边界)）。当前工具链版本：Rust `1.96.0`（[`rust-toolchain.toml`](../../rust-toolchain.toml)），Source Tool guest 目标 `wasm32-unknown-unknown`，独立编码器 `wasm-tools` `1.254.0`（见 [`EXPECTED_WASM_TOOLS`](../../xtask/src/ci.rs)、[`full-gate.yml`](../../.github/workflows/full-gate.yml) 与 `tools/*/manifest.toml`）。

### 安装前置工具

在获准安装工具链的环境中安装 guest 目标与 Component 编码器：

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-tools --version 1.254.0 --locked
```

| 检查入口 | 检查项 | 结果判读 |
| --- | --- | --- |
| `cargo xtask ci` 工具链预检 | `rustc 1.96.0`、`wasm32-unknown-unknown`、`wasm-tools` | 缺省任一项或 `wasm-tools` 不可执行时失败；版本非 `1.254.0` 时输出摘要漂移警告并继续 |
| `cargo xtask rollout --channel alpha` 的 `capability_probes` | `rustc 1.96.0`、`wasm32-unknown-unknown`（门禁项）及宿主信息（记录项） | 门禁项不满足时失败；rollout 只构建宿主 CLI，`wasm-tools` 探测结果仅记入记录 |

### 构建宿主 CLI 与配置被测路径

从仓库根构建宿主二进制，并为外置 harness 指定绝对路径（自定义 target 目录时按实际位置设置）：

```sh
cargo build -p lattice-cli --bin lattice
export LATTICE_BIN="$(pwd)/target/debug/lattice"
export LATTICE_HARNESS_FAILURE_DIR="$(pwd)/target/harness-failures"
```

| 产物与变量 | 结果判读 |
| --- | --- |
| `target/debug/lattice` | 生成宿主 CLI 可执行文件 |
| `LATTICE_BIN` | harness 按测试进程工作目录解析并校验该文件，缺失或无效路径直接失败；`cargo test -p lattice-system-harness` 的工作目录是 crate 目录，需传绝对路径，在 git worktree 中指向当前 worktree 产物（见[被测二进制](testing.md#被测二进制)） |
| `LATTICE_HARNESS_FAILURE_DIR` | 外置场景失败 bundle 根目录；未设置时写入 `crates/lattice-system-harness/target/harness-failures/` |

### 构建并核验 Source Tool Component

构建 guest 模块并核验已提交 Component 的导入面、WIT world 与字节摘要：

```sh
cargo xtask tool-build --verify
```

| 结果 | 含义 |
| --- | --- |
| 退出 `0` 且无警告 | guest 构建、编码、`validate`、`wasi:*` 导入面与 WIT world 核验通过，摘要一致 |
| 退出 `0` 且提示摘要漂移 | 功能与导入面通过，编码字节与已提交 Component 存在摘要漂移（warn-only） |
| 非零退出 | guest 构建、编码、`validate`、导入面断言或 WIT world 核验失败 |

范围之外：宿主 CLI 通过 `cargo build -p lattice-cli --bin lattice` 构建；`cargo build --workspace` 会尝试按宿主链接 guest 的 `cdylib`，所以宿主构建指定包与二进制。Source Tool guest 使用 `wasm32-unknown-unknown` 与 `cdylib`，由 `cargo xtask tool-build` 单独构建（见[工具开发与操作](tool-development.md#开发来源与生成)）。

## 后台与重连操作

状态根解析、profile 查找与权限、后台环境冻结及每轮重读 `providers.toml` / `auth.json` 的规则见[私有状态根](../architecture/storage.md#私有状态根)、[配置来源与后台启动](../architecture/session/acceptance.md#配置来源与后台启动)、[配置安全边界](../architecture/session/acceptance.md#配置安全边界)、[会话的 profile 副本](../architecture/session/settings.md#会话的-profile-副本)和[费用估算](../architecture/llm/usage.md#费用估算)；更换决定配置目录、全局 `.lattice/agents` 或 TLS 根证书的环境后，先执行 `lattice daemon stop` 再启动新后台。TUI 启动时不检查 profile（见 [TUI 启动、提交与运行](../architecture/tui/README.md#启动提交与运行)）；启动时传入的 `--agent`（含新建会话与打开已有会话）在首个携带它的提交被接受后消费，未被接受时保留，后续提交继承会话副本。

| 命令入口 | 后台连接方式 |
| --- | --- |
| `lattice run`、`lattice session`、`lattice tui`、`lattice auth`、`lattice models`、`lattice audit`、`lattice lancet status` | 连接当前状态根后台；未运行时按需自启后台 |
| `lattice daemon status`、`lattice daemon stop` | 仅连接已有后台；未运行时报 `not_running`，退出 `1` |
| `lattice daemon serve` | 在前台运行后台实例 |
| `lattice lancet doctor` | 在当前进程内自检工作区与内置目录，不连接后台 |

范围之外：找回与重连使用原操作系统用户、状态根与会话身份；本节命令用于开发核验，系统级产品验收见[后台与重连场景](testing.md#后台与重连场景)。

### 提交与找回

首次提交省略 `--session` 并必填 `--agent`（`lattice run` 与 `lattice session send` 参数相同），后续默认继承会话副本；`--agent` 重建副本，`--model <provider>/<model>` 与 `--effort <off|minimal|low|medium|high|xhigh|max>` 在接受时写入副本，换模型时的档位回退与配置拒绝规则见[模型引用](../architecture/llm/models.md#模型引用)、[可用档位](../architecture/llm/reasoning.md#可用档位)、[会话的 profile 副本](../architecture/session/settings.md#会话的-profile-副本)和[配置拒绝的原因](../architecture/session/acceptance.md#配置拒绝的原因)。`gpt-6-luna` 等模型在自定义 Chat Completions provider 上带工具且省略档位时，provider 默认推理会触发 HTTP 400，需显式传 `--effort off` 或在 profile 写 `effort = "off"`。

#### 提交请求与覆盖模型档位

在目标项目根准备有效 profile（如 `reader`）与凭据，新建会话、续接会话、切换模型与档位，或核验不支持档位的拒绝：

```sh
PROMPT="概括这个项目的用途，并指出主要入口文件和目录。"
lattice --json session send "$PROMPT" --agent reader
lattice --json run "$PROMPT" --agent reader --model anthropic/claude-sonnet-4-6 --effort high

SESSION_ID="替换为首条 submit 记录中的 session_id"
lattice --json session send "继续检查测试入口。" --session "$SESSION_ID" \
  --model anthropic/claude-sonnet-4-6 --effort high
lattice --json run "继续。" --session "$SESSION_ID"
lattice --json run "重新读取 reader 配置。" --session "$SESSION_ID" --agent reader
lattice --json run "测试档位拒绝。" --session "$SESSION_ID" --effort max
```

CLI 先输出首条含 `turn_id` 与 `session_id` 的 `submit` 记录，收到回执后再输出第二条 `submit` 记录（见[实现模块与公开操作](../architecture/session/operations.md#实现模块与公开操作)）：

| 第二条 `submit` 的 `result` | 退出码 | 结果判读与下一步 |
| --- | --- | --- |
| `accepted` | 按本轮结局（`0` / `1` / `2`） | 接受事务已提交，选择已写入副本；继续观察，或断开后按 `turn_id` 查询 |
| `rejected`（`kind: "busy"`） | `1` | 同 Session 有操作运行；保留输入，待空闲后重发（后台不排队） |
| `rejected`（`kind: "bad_request"` / `"not_found"`） | `2` | 配置、模型档位或目标会话被拒绝；既有副本与会话树不变，provider 未收到请求 |
| `rejected`（`kind: "conflict"`） | `2` | `TurnId` 已使用或目标新建会话已存在；改用 `lattice session turn <TURN_ID>` 查询 |
| `not_accepted` 或 `unknown` | `1` | 写出前取消、确认未接受或回执不明；进程中断时按下文手动找回 |

#### 提交中断后找回

等待提交回执期间 CLI 中断时，用首条 `submit` 记录中的 `TurnId` 查询状态（规则见[请求身份与找回](../architecture/session/recovery.md#请求身份与找回)）：

```sh
TURN_ID="替换为首条 submit 记录中的 turn_id"
lattice --json session turn "$TURN_ID"
```

| 查询结果 | 退出码 | 含义与下一步 |
| --- | --- | --- |
| 返回 `TurnSummary` 与 `steps` | `0` | 已接受；按下一节继续观察或读取结果 |
| `{"status":"in_flight","turn_id":"..."}` | `0` | 后台仍在准入该请求；稍后再查，或加 `--follow` 等待准入完成 |
| `error`（`kind: "not_found"`） | `2` | 未被接受；可在原工作区重新执行 `session send` / `run`（生成新 `TurnId`） |

### 查询与继续观察

查询与重连读取持久事实，无需当前 profile 或凭据；JSONL 记录类型、慢观察者合并与后台重启流式文字边界见[实现模块与公开操作](../architecture/session/operations.md#实现模块与公开操作)和[观察与重连](../architecture/session/recovery.md#观察与重连)。

在新终端单次读取 Step 详情、持续跟随一轮，或在 TUI 中打开会话：

```sh
lattice --json session turn "$TURN_ID"
lattice --json session turn "$TURN_ID" --follow
lattice tui "$SESSION_ID"
```

| 入口 | 输出与结果判读 |
| --- | --- |
| `lattice [--json] session turn <TURN_ID>` | 按 `TurnId` 返回 `TurnSummary` 与全部已关闭及未关闭 Step（序号、Step / 尝试 / 槽位 `progress` 及结论），读取成功固定退出 `0` |
| `lattice --json session turn <TURN_ID> --follow` | 输出该轮 JSONL（`head`、按 `node` 覆盖的 `entry`、`text`、`connection`、`error`），结局在最后一步 `entry` 的 `TurnSummary` 中，退出码按结局映射（`0` / `1` / `2`） |
| `lattice tui <SESSION_ID>` 与 `/reconnect` | 观察会话视图；断连后用 `/reconnect` 重建视图，临时正文保留至 Step 正文装入后替换，正文读取失败时继续保留已知结局（见 [TUI 连接、结局与重载](../architecture/tui/README.md#连接结局与重载)） |

### 查询当前路径的用量

用量查询在同一读事务内返回当前路径逐轮明细与合计（`path_total`）及全会话所有分支合计（`session_total`），口径与格式见[当前路径的用量查询](../architecture/session/operations.md#当前路径的用量查询)和[费用估算](../architecture/llm/usage.md#费用估算)；TUI 本轮与上下文用量见 [statusline](../architecture/tui/layout.md#statusline)。

按 `SessionId`（或唯一前缀）读取用量与费用汇总：

```sh
lattice session usage "$SESSION_ID"
lattice --json session usage "$SESSION_ID"
lattice session usage "$SESSION_ID" --json
```

| 输出模式 | 结果判读 |
| --- | --- |
| 文本输出 | 逐轮列出计数、尝试数与费用（未报告显示 `-`，零显示 `0`，溢出附 `(overflow)`），末尾两行分别为“当前路径合计”与“全会话合计” |
| JSON 输出 | 返回 `{ session_id, position, turns, path_total, session_total }`；`rewind` 后 `path_total` 随当前路径变化，`session_total` 仍含原分支 |

### 会话退回与分叉

`rewind` 在会话绑定的项目工作区内将当前位置移到节点 `N`（改写首条用户消息用根节点 `0`），保留原分支且工作区文件与工作区历史库 `current` 保持原样，规则见[表、读取范围与 rewind](../architecture/session/README.md#表读取范围与-rewind)和[会话库与项目绑定](../architecture/session/concurrency.md#会话库与项目绑定)。

查看完整会话树，退回到目标节点或根节点 `0` 后从该位置分叉提交：

```sh
lattice session list
lattice session log "$SESSION_ID"
NODE_ID="2"  # 替换为 log 中实际存在的节点序号
lattice session rewind "$SESSION_ID" "$NODE_ID"
lattice session send "从这里换一种思路继续。" --session "$SESSION_ID"

lattice session rewind "$SESSION_ID" 0
lattice session send "改写后的第一条消息。" --session "$SESSION_ID" --agent reader
```

| 结果 | 退出码 | 含义 |
| --- | --- | --- |
| 输出 `current=<NODE_ID>` | `0` | 当前位置已移至 `N`；原分支保留在 `log` 中，原轮次仍可用 `session turn <TURN_ID>` 查询 |
| `Busy` | `1` | 同 Session 正由后台 Owner 执行，位置不变 |
| `BadRequest` / `NotFound` | `2` | 节点不存在、工作目录与绑定项目不一致或会话不存在，位置不变 |

### 请求取消

取消按会话头所示运行轮的 `TurnId` 发起，接管回执仅代表判定点已接管取消意图，持久结局另按 `TurnId` 查询；接管区间、退出交接与 5 秒回执等待预算见[取消与退出](../architecture/session/recovery.md#取消与退出)、[客户端意图与退出交接](../architecture/session/recovery.md#客户端意图与退出交接)和[输入与传输限制](../architecture/session/concurrency.md#输入与传输限制)，TUI 取消与退出键位见[键盘](../architecture/tui/interaction.md#键盘)。

按 `SessionId` 请求取消当前运行轮，或在提交/观察终端按 `Ctrl+C`：

```sh
lattice --json session cancel "$SESSION_ID"
```

| 操作 / `cancel` 记录状态 | 退出码 | 含义与下一步 |
| --- | --- | --- |
| `status: "taken"`（`owned: true`） | `0` | 后台已接管取消并独立收尾；用 `lattice session turn <TURN_ID>` 查持久结局 |
| `status: "not_taken"`（`owned: false`） | `0` | 判定点未接管（后台仍可能存在收尾工作）；按 `turn_id` 查实际结局 |
| `status: "none"`（`turn_id: null`） | `0` | 当前没有运行中的轮次 |
| `status: "unknown"`（带原 `turn_id` 与 `error`） | `1` | 取消回执丢失；按原 `turn_id` 查询，必要时再次取消 |
| 会话不存在（`NotFound`） | `2` | 未找到目标会话 |
| `run` / `send` / `--follow` 中首次 `Ctrl+C` | — | 按本轮 `TurnId` 请求取消并继续观察；等待提交回执满 5 秒时 [LocalTurn](../architecture/session/README.md#组件与接口) 报结果不明并继续等迟到回执，断连后按 `TurnId` 找回 |
| 第二次 `Ctrl+C` | `1` 或 `2` | 保留首次取消意图，结束本地等待或观察 |

### 执行者消失后继续

会话存在未关闭的 Turn（`unclosed_turn` 非空）且后台无存活 Owner（`owner=false`）时，原执行者已退出，旧 Turn 停在已写入进度并按持久记录派生结局（仅在内存中的取消意图派生为 `Failed`），规则见[执行者消失后的 Turn](../architecture/session/recovery.md#执行者消失后的-turn)。

核实无存活 Owner 后，直接向该会话发送新消息或执行 `rewind`：

```sh
lattice --json session list
lattice --json session send "继续检查项目的测试入口。" --session "$SESSION_ID"
```

| 结果 | 退出码 | 含义 |
| --- | --- | --- |
| 新提交 `accepted`（或 `rewind` 成功） | 按新操作结果 | 新请求分配新 `TurnId`；旧 Turn 持久记录不变，仍可用 `lattice session turn <OLD_TURN_ID>` 查询 |
| `Busy` | `1` | 后台仍有存活 Owner 执行该 Session；等待结束或执行 `session cancel` |

### 查询、停止与前台运行后台

`daemon stop` 关闭新准入、等待已接管操作收尾并等待 socket 撤下与 `owner.lock` 释放（回执后限时 30 秒），前端退出不触发自动停止，规则见[停止后台](../architecture/session/recovery.md#停止后台)；隔离场景前台启动后台时，进程组与状态清理遵循[超时与进程生命周期](testing.md#超时与进程生命周期)。

查询或停止当前后台，或在隔离状态根前台运行后台：

```sh
lattice --json daemon status
lattice --json daemon stop

# --state-root 可选；指定时须为绝对路径，产品在其下使用 lattice/：
lattice daemon serve --state-root /tmp/lattice-isolated-state
```

| 命令与返回 | 退出码 | 结果判读与下一步 |
| --- | --- | --- |
| `daemon status` / `stop`：`unconfirmed_operations == 0` | `0` | 输出 `epoch`、`stopping`、`owners`、`unconfirmed_operations`；`stop` 返回 `0` 后可立即在同一状态根启动新后台（不等待旧进程退出） |
| `daemon status` / `stop`：`unconfirmed_operations != 0` | `1` | 当前后台进程曾记录未完成收尾的操作计数（非持久进行中 Turn 数）；继续查询相关 Session |
| `daemon stop` 报 `stopping` 或连接中断 | `1` | 回执后 30 秒内锁未释放或断连；用 `lattice --json daemon status` 复核后台是否仍在收尾 |
| `daemon status` / `stop` 报 `not_running` | `1` | 当前状态根无运行中的后台 |
| `daemon serve` | `0` / `2`（相对路径） | 前台运行并继承启动进程环境（忽略工作区 `.env`）；崩溃注入与状态清理仅对隔离临时状态根执行 |

### 升级与输出字段适配

不兼容存储格式直接拒绝打开并列出路径，保持原状态不作自动迁移，规则见[身份、版本与打开](../architecture/storage.md#身份版本与打开)和[开发状态诊断与重建边界](tool-development.md#开发状态诊断与重建边界)；五项计数、费用与模型候选的现行定义见[用量换算](../architecture/llm/usage.md#用量换算)、[费用估算](../architecture/llm/usage.md#费用估算)、[关闭 Step 与 Turn](../architecture/session/steps.md#关闭-step-与-turn)和[模型候选](../architecture/llm/models.md#模型候选)。

升级前先用匹配旧后台的旧二进制停止后台（或手动结束旧后台进程），再用新二进制核验状态：

```sh
/path/to/old/lattice daemon stop
lattice --json daemon status
```

客户端与后台须来自相符构建：当前 Local IPC 标识为 `LTLOCAL=`（新旧 IPC 不互通，旧后台需用旧二进制停止），会话库 `lattice.redb` 格式版本为 `7`（拒绝格式 `6` 及旧 `sessions.git` / `invocations.git`）：

| 版本 / 格式现象 | 退出码 | 含义与处理动作 |
| --- | --- | --- |
| `incompatible_backend`（后台回写自身 `LTLOCAL*` 标识后断开） | `1` | 构建版本不匹配：后台较旧（如 `LTLOCAL<`）时按提示用旧二进制 `daemon stop`；后台较新时改用新二进制 |
| `transport` 错误并附停机提示（零字节断开） | `1` | 更早后台未回写标识即断开，或当前后台过载/崩溃断开；核对后台进程存活与版本 |
| 会话库拒绝格式 `6` 或旧 `sessions.git` / `invocations.git` | `1` | 保持原库不变并列出路径；存档或删除旧库须另获授权（见[开发状态诊断与重建边界](tool-development.md#开发状态诊断与重建边界)） |

外部脚本与解析器按以下对照适配输出字段：

| 接口位置 | 旧字段 / 行为 | 现行字段 / 行为 |
| --- | --- | --- |
| 尝试级 `usage`（`session turn --json`、JSONL、人类输出 `usage`） | 必填 `prompt_tokens`、`completion_tokens`、`total_tokens` | 五项可选计数 `input`、`cache_read`、`cache_write`、`output`、`reasoning` 与 `usage_kind`（旧记录不补 `usage_kind`） |
| 尝试级费用 | `estimated_cost_usd` | `cost: { usd, exact } \| null`（旧记录未知费用保持 `null` 不回填） |
| Turn 用量汇总 | 三项合计与估算费用 | 新增 `attempts`、`partial_attempts`；五项计数各含 `{ known, complete, overflowed }`（`complete` 仅表示无缺失尝试且未溢出，与单价无关）；`cost: { known: { usd, exact } \| null, unknown_attempts, overflowed }`（`unknown_attempts` 仅统计费用未知尝试）；`kind` 由尝试 `usage_kind` 与溢出决定 |
| `session usage --json` | `total` | `path_total` 与 `session_total`（含 `rewind` 保留分支）；`turns` 仍只列当前路径 |
| 提交、会话头与 `UserMessage` | 无模型/档位覆盖字段 | 提交带可选 `model`、`effort`；`head` 提供 `model`、`provider`、`effort`、`profile_default_effort`；`UserMessage` 保存本轮生效档位 |
| 模型候选与 `lattice models` 表 | 无档位数组与类型化可用性 | 查询为 `List { provider, ready_only }` 或 `Target`，含有序 `supported_efforts`（空为 `[]`）与 `availability`（`ready`、`unconfigured` 附 `requirement`、`invalid` 附 `reason`）；文本表 `EFFORTS` 空显示 `-`、无效显示 `config-error`（原因写往 stderr），`AUTH` 显示状态名，命令不因单个配置无效而失败 |

### 退出码与后续动作

命令退出码与 `TurnSummary.outcome` 分开判读（分派见 [`main.rs`](../../crates/lattice-cli/src/main.rs)、[`session.rs`](../../crates/lattice-cli/src/commands/session.rs)、[`run.rs`](../../crates/lattice-cli/src/commands/run.rs) 和 [`daemon.rs`](../../crates/lattice-cli/src/commands/daemon.rs)）：

```sh
lattice --json run "示例请求" --agent reader
echo "exit=$?"
```

| 退出码 | 产生条件 | 后续动作 |
| --- | --- | --- |
| `0` | `run` / `session send` / `session turn --follow` 结局为 `Completed` 且观察/正文读取完整；纯查询读取成功；`session cancel` 为 `taken` / `not_taken` / `none`；`daemon status` / `stop` 的 `unconfirmed_operations == 0` | 纯 `session turn` 退出 `0` 仅代表读取成功，业务结局读 `TurnSummary`；`session cancel` 后仍按 `turn_id` 查持久结局 |
| `1` | 结局为 `Failed`；已知 `Completed` 但观察中断或最终内容读取失败；提交回执为 `not_accepted` / `unknown`；`2` 一行之外的后台错误（如 `Busy`、`Stopping`、`Transport`）；`session cancel` 为 `unknown`；`daemon` 的 `unconfirmed_operations != 0` | 结合 `error` 与已知 `TurnSummary` 定位；最终内容读取失败退出 `1` 时已完成 Turn 保留可见终态，重连直接读取结果，无需重跑任务 |
| `2` | CLI 参数错误；后台错误 `BadRequest`（含配置拒绝）、`NotFound`、`Conflict`；`run` / `session send` / `session turn --follow` 结局为 `Cancelled`；`auth login` 取消 | 按错误信息修正参数、配置或工作目录；`Conflict` 时按原 `TurnId` 查询 |

## 凭据与 ChatGPT 登录

前端负责输入与浏览器交互，后台保存、按需刷新与撤销凭据（`auth status` 不刷新过期 token）；规则见[凭据命令](../architecture/llm/credentials.md#凭据命令)、[登录](../architecture/llm/credentials.md#登录)、[刷新](../architecture/llm/credentials.md#刷新)、[登出](../architecture/llm/credentials.md#登出)和[凭据命令与私有交换](../architecture/session/operations.md#凭据命令与私有交换)。

录入 API key、登录 ChatGPT、查看状态，或执行登出与移除：

```sh
lattice auth set anthropic
lattice auth login chatgpt
lattice --json auth status
lattice auth logout chatgpt
lattice --json auth remove chatgpt
```

| 命令 | 退出码 | 结果判读与下一步 |
| --- | --- | --- |
| `lattice [--json] auth set <provider>` | `0` / `2` | 从终端（无回显）或 stdin 读入 key 写入 `auth.json`（key 不放命令行参数） |
| `lattice [--json] auth login chatgpt` | `0` / `2`（取消） / `1` | 图形环境（设 `DISPLAY` 或 `WAYLAND_DISPLAY`）调用 `xdg-open`，无头或失败时向 stderr 显示授权 URL；支持本地回调或无回显粘贴回调 URL，`Ctrl+C` / EOF 取消。`--json` 只输出非敏感最终结果，`catalog` 为 `{"status":"refreshed","model_count":...}` 或 `{"status":"failed","warning":...}`（目录失败仍登录成功，文本模式向 stderr 输出警告）。中断时用 `auth status` 核验；登录后可用 `/model`、`lattice models --provider chatgpt` 或 `--model chatgpt/<模型>` |
| `lattice [--json] auth status` | `0` | 列出各 provider 的 `ready`、`unconfigured`（带 `requirement`）或 `invalid`；`chatgpt` 另含邮箱与 `expires_at`（token 过期仍显示 `ready`） |
| `lattice [--json] auth logout chatgpt` | `0` / `1` | 清理本地 token，保留 `client_id`、账号、host ID 与目录缓存；返回 `revocation`：`confirmed`、`unconfirmed`（可到 ChatGPT 设置断开）或 `not_needed` |
| `lattice [--json] auth remove <provider>` | `0` / `1` | 删除 key 或 `chatgpt` 的 token、注册、账号及目录缓存（保留 host ID），返回 `removed` 及 `chatgpt` 的 `revocation` |

范围之外：真实 SIWC 端到端互通依赖真实账号人工核验；本地自动化测试覆盖协议、回调与状态机边界。

## 模型元数据操作

24 小时自动刷新、1 分钟至 1 小时退避重试、`[metadata] auto_refresh`、合并优先级、列表过滤与缺目录补拉规则见[元数据的来源与合并](../architecture/llm/models.md#元数据的来源与合并)、[可用性判定](../architecture/llm/models.md#可用性判定)、[模型候选](../architecture/llm/models.md#模型候选)和[账号模型列表](../architecture/llm/credentials.md#账号模型列表)（其中 `cache/models.json` 以文件修改时间计算新鲜度，`cache/chatgpt-models.json` 以 `fetched_at` 计算）。

查看模型候选列表、仅列出已就绪候选，或立即刷新远程元数据与已登录账号目录：

```sh
lattice models --provider anthropic
lattice models --auth
lattice --json models --provider anthropic
lattice models refresh
lattice --json models refresh
```

| 命令 | 退出码 | 结果判读 |
| --- | --- | --- |
| `lattice models [--provider <id>]` | `0` / `2`（未注册 provider） | 输出 `MODEL`、`CONTEXT`、`OUTPUT`、`EFFORTS`、`PRICE`、`AUTH`：空档位显示 `-`（未知元数据、目录外或 deprecated 模型仍可按 `<provider>/<model>` 提交请求）；无效模型 `EFFORTS` 显示 `config-error`、`AUTH` 显示 `invalid` 并在 stderr 逐条输出原因 |
| `lattice models --auth [--provider <id>]` | `0` | 按 `ready_only: true` 仅保留 `ready` 候选，并跳过未就绪 provider 的警告 |
| `lattice --json models [...]` | `0` | 输出含 `models` 与 `warnings` 的 JSON 对象 |
| `lattice [--json] models refresh` | `0` / `1`（部分或全部失败） / `2`（带 `--provider` 或 `--auth`） | 分别刷新 `models.dev` 与已登录 ChatGPT 目录；全成功时 `--json` 输出 `{"type":"models_refresh","status":"refreshed"}`；部分失败保留已更新缓存并报告失败来源与已更新项 |

## 日常验证

按[工程规范](engineering.md#验证)从直接受影响的 crate 或目标选起；外置 harness 二进制解析、进程组与清理见[被测二进制](testing.md#被测二进制)和[超时与进程生命周期](testing.md#超时与进程生命周期)。

### 运行单元、集成与默认 harness 测试

执行目标 crate 测试、启用 `test-utils` 测试辅助特性、运行外置 harness 目标，或在跨组件改动时运行全工作区测试：

```sh
cargo test -p lancet-authority
cargo test -p lattice-api
cargo test -p lattice-agent --features test-utils

cargo test -p lattice-system-harness --test direct_tree_session
cargo test -p lattice-system-harness --test direct_local_turn
cargo test -p lattice-system-harness --test direct_operator
cargo test -p lattice-system-harness --test direct_background -- --list
cargo test -p lattice-system-harness --test direct_background
cargo test -p lattice-system-harness --test pty_tui_smoke

cargo test -p lattice-system-harness
cargo test --workspace
cargo test --workspace --release
```

| 检查项 | 结果判读 |
| --- | --- |
| 过滤表达式与通过数 | 核对实际通过数（`N passed` 且 `N > 0`；`0 passed` 未覆盖目标）；精确匹配完整测试名加 `-- --exact` |
| `--release` 与 `LATTICE_BIN` | `cargo test --workspace --release` 仅切换测试二进制自身的 profile；验证 release 产品时先执行 `cargo build --release -p lattice-cli --bin lattice` 并更新 `LATTICE_BIN` |

### 运行显式 feature 目标

运行 [`crates/lattice-system-harness/Cargo.toml`](../../crates/lattice-system-harness/Cargo.toml) 中的非默认目标（默认 `cargo test --workspace` 与 `cargo xtask ci` 不包含这两项）：

```sh
cargo test -p lattice-system-harness --features release-operator \
  --test release_operator release_operator_component_matrix -- --exact

cargo build -p lattice-cli --bin lattice --features test-crash-barriers
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --features crash-barriers \
  --test direct_crash_barriers
```

| 目标 | 结果判读与后续动作 |
| --- | --- |
| `release_operator` | `release_operator_component_matrix` 通过且 `1 passed` |
| `direct_crash_barriers` | 产品包用 `--features test-crash-barriers`，harness 包用 `--features crash-barriers`；该构建会覆盖 `target/debug/lattice`，恢复普通验证前重新执行无该 feature 的 `cargo build -p lattice-cli --bin lattice` |

## 本地 CI 与远程工作流

运行前核对并行编译任务、内存与磁盘余量；资源与门禁证据规则见[超时与进程生命周期](testing.md#超时与进程生命周期)和 [CI 与发布证据](testing.md#ci-与发布证据)。

纯文档修改运行链接检查；跨组件修改运行本地全量 CI（git worktree 中可用 `env -u LATTICE_BIN` 清除外部继承路径）：

```sh
CARGO_BUILD_JOBS=4 cargo xtask docs-check
cargo xtask ci
env -u LATTICE_BIN cargo xtask ci
```

| 命令 | 成功输出 | 失败判读 |
| --- | --- | --- |
| `cargo xtask docs-check` | `文档链接检查通过：<N> 个 Markdown 文件`（退出 `0`） | 逐行输出缺失文件或缺失锚点（非零退出） |
| `cargo xtask ci` | 末尾输出 `All checks passed.`（退出 `0`） | 任一步骤失败或单 crate 超过 `LATTICE_TEST_TIMEOUT_SECS`（默认 `300` 秒，默认 `CARGO_BUILD_JOBS=4`）时回收对应进程组并终止 |

三个门禁的差异如下，具体步骤以链接的文件为准：

| 门禁 | 触发 | 独有的检查 | 不包含 |
| --- | --- | --- | --- |
| 本地 [`xtask ci`](../../xtask/src/ci.rs) | 手动 | 逐 crate 测试带超时；最后运行 `tool-build --verify`，只有摘要漂移为警告 | `direct_crash_barriers`、`release_operator`、rollout |
| 普通 CI [`ci.yml`](../../.github/workflows/ci.yml) | `main` push 与所有 PR | 构建带 `test-crash-barriers` 的二进制并运行 `direct_crash_barriers`；只改 `.md` 的 PR 只运行 `docs-check` | encoder 安装、`tool-build --verify`、`release_operator`、rollout |
| [`full-gate`](../../.github/workflows/full-gate.yml) | 每日定时与手动 | 安装固定 encoder，运行 alpha rollout，并以 `if: always()` 运行 `tool-build --verify`，日志写入 `target/rollout/component-verify.log` | `docs-check`、`direct_crash_barriers` |

范围之外：失败时直接定位根因并保持共享编译缓存原状，并发参数仅用于适配本机资源上限。

## Alpha rollout

`rollout` 针对 `--locked --release` 二进制编排发布证据并写出版本化 JSON 记录（见 [`xtask/src/rollout.rs`](../../xtask/src/rollout.rs) 与 [CI 与发布证据](testing.md#ci-与发布证据)）。

在已跟踪文件干净的工作树中运行 alpha rollout，可按需覆盖主记录路径：

```sh
cargo xtask rollout --channel alpha
cargo xtask rollout --channel alpha --out target/rollout/custom-alpha.json
```

必需步骤初始为 `blocked`，执行后逐步原子写回记录；测试步骤另要求通过数非零（`passed > 0`）：

| 序号 | 步骤 ID | 执行内容 |
| --- | --- | --- |
| 1 | `clean_worktree` | 校验已跟踪文件无暂存或未暂存修改 |
| 2 | `capability_probes` | 校验 `rustc 1.96.0` 与 `wasm32-unknown-unknown`（门禁项），记录宿主能力 |
| 3 | `build_release_locked` | `cargo build --locked --release -p lattice-cli --bin lattice` |
| 4 | `artifact_identity` | 记录 `target/release/lattice` 的 SHA-256 与字节大小 |
| 5 | `harness_boundary` | `cargo xtask harness-boundary` |
| 6 | `lancet_boundary` | `cargo xtask lancet-boundary` |
| 7 | `direct_local` | 以 release 二进制运行 `cargo test -p lattice-system-harness` |
| 8 | `offline_correctness` | 运行 `lancet-protocol`、`lancet-runtime`、`lancet-authority`、`lancet-api`、`lattice-api`、`lattice-session` 测试 |
| 9 | `release_operator_matrix` | 以 release 二进制运行 `--features release-operator --test release_operator release_operator_component_matrix -- --exact` |
| 10 | `doctor` | 在隔离临时 `HOME` 清空环境下执行 `lattice --json lancet doctor`，保存 `target/rollout/evidence/doctor.json` |

| 退出码 | 记录 `verdict` | 结果判读 |
| --- | --- | --- |
| `0` | `pass` | 全部 10 个必需步骤实际运行且均为 `passed` |
| `1` | `fail` | 任一执行步骤为 `failed`（后续未运行步骤保持 `blocked`；`failed`、`blocked`、`skipped` 均判为 `fail`） |
| `2` | `fail` | 前提拒绝（如工作树存在已跟踪修改、Git 失败）或参数错误，`rejection` 写明原因 |

## 查看证据

失败 bundle 的占用锁、脱敏与保留规则见[失败证据](testing.md#失败证据)和[脱敏与完整性](testing.md#脱敏与完整性)，CI 与发布证据的提交绑定规则见 [CI 与发布证据](testing.md#ci-与发布证据)。

查看最近一次 rollout 的结论与各步骤状态：

```sh
jq '{commit_sha, verdict, rejection, steps: [.steps[] | {id, status}]}' target/rollout/alpha-latest.json
```

| 证据种类 | 本地位置 / 远程 Actions Artifact | 核验要点 |
| --- | --- | --- |
| 单跑 `lattice-system-harness` 失败 bundle | `crates/lattice-system-harness/target/harness-failures/<scenario>/` | 含脱敏 stdout/stderr、command/world 与工作区前后 diff（多步场景按序号编号）；依次排查主断言、命令终止与输出完整性、清理或扫描错误。成功场景不写 bundle，核对目录时间确认属于本轮运行 |
| 本地 CI 与显式目录失败 bundle | `target/harness-failures/<scenario>/` | 判读同上；普通 CI 失败时上传 `harness-failures-ubuntu-latest`，`full-gate` 失败时上传 `harness-failures-full-gate` |
| rollout 场景失败 bundle | `target/rollout/harness-failures/<scenario>/` | 判读同上，路径记入 `RolloutRecord.failure_artifacts` |
| rollout 主记录与最近记录 | `target/rollout/rollout-alpha-<yyyymmddTHHMMSSZ>.json`（`--out` 可改主位置）与 `target/rollout/alpha-latest.json` | `full-gate` 始终上传 `rollout-alpha-record`（`RolloutRecord` 当前 `schema_version` 为 `1`）；核对 `commit_sha`、`artifact`（路径、`sha256`、`size_bytes`）、起止时间、`probes`、`steps`、`verdict` 与 `rejection`，仅将提交和产物身份匹配的记录用作当前验证依据 |
| doctor 与 Component 校验日志 | `target/rollout/evidence/doctor.json`（及 `evidence_paths`）、`target/rollout/component-verify.log` | 核对 `doctor.json` 自检输出，以及 `component-verify.log` 中的摘要漂移警告或硬错误 |

范围之外：旧 Git ledger 性能材料仅供历史解读，见 [InvocationStore 历史诊断指南](invocation-store-diagnostics.md)；`.githooks/pre-commit` 仅拦截误暂存的被忽略文件（不运行测试），安装 Git hook 遵循 [Git 操作规范](../governance/git-operations.md)；本仓库验证入口统一为 Cargo 与 `cargo xtask`（不设覆盖率百分比门禁或 `cargo-nextest` 入口）。
