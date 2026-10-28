---
status: proposed
---

# ADR 0011：IM 网关在本地后台内运行，后台核心与 IPC 传输解耦

- 状态：Proposed（草案，尚未获维护者批准）
- 日期：2026-10-28
- 依赖：[ADR 0008](0008-backend-api-commands-view-queries.md)、[ADR 0010](0010-protocol-adapters-and-configured-providers.md)
- 实施跟踪：GitHub #332（两步：先合 `lattice-api` 纯重构 PR，再做 IM 网关）

## 背景

以下事实以 `main` 上 `ef51cfd`（2026-10-28）为准。

- 维护者希望离开终端时，也能从手机经 IM 与 lattice 对话。仓库里目前没有任何 IM 代码，v1 只接 Telegram 私聊。
- 现有前端只有 CLI 与 TUI。它们只依赖 `lattice-api` 的 `LocalClient`，经私有 Unix socket（`local/ipc.rs`，`LTLOCAL5`）与后台通信（ADR 0008）。后台入口是 `local::serve()`（`local/server.rs:960`），其中的 `Backend` 不对外公开。
- `Backend` 的观察与提交路径目前直接面向 socket：
  - `watch()` 直接写 `UnixStream`，并用 `readable()` 判断对端是否关闭（`server.rs:748–857`）；
  - `claim()`/`submit()` 接收 `submitter: Option<UnixStream>`，`check_submitter` 用 `MSG_PEEK` 判断提交者是否存活（`:429`、`:460`、`:890`）；
  - 轮次句柄只有客户端一侧的 `LocalTurn`（`local/operation.rs:23`）；
  - `Stop` 只置 `admission.stopping`（`:605`），不推送 `changed`，进程内的组件无法订阅停机。
  因此，后台内的组件要提交或观察轮次，只能自己连回自己的 socket。
- 后台的准入上限如下：owner 32（`:207–209`）、连接 64（`:974`）、订阅 32（`:132`）。停机流程是：`stopping` 后新提交被准入拒为 `Stopping`（`:201–206`）；`serve()` 等 `owners == 0`（`:982–985`），再 `closed.send_replace(true)`（`:986`），最后清理实例（`:990`）。
- ADR 0010 已决定配置与凭据由后台读取。在此之上已落地的部分：后台经 `lattice_llm::auth::AuthStore` 读写 `~/.lattice/auth.json`（`local/auth.rs`），文件安全检查来自 PR #325。尚未落地的部分：#315 仍 open，提交路径仍要求客户端传入 `FrozenConfiguration`（`local/config.rs:22` 的 `from_env`）。
- `lattice-llm` 是 `lattice-api` 依赖的向外适配：`lattice-api` 依赖它，它不认识 `lattice-api`。
- 根 [`AGENTS.md`](../../AGENTS.md) 要求深模块、KISS/YAGNI，不为单一实现建抽象。
- Telegram Bot API 的相关事实（10.3，2026-10-28 核对 <https://core.telegram.org/bots/api>）：
  - `getUpdates` 长轮询与 webhook 互斥。以 `offset` 确认更新，未确认的更新最多保留 24 小时。
  - `sendMessage` 的正文上限为 4096 字符。
  - `sendMessageDraft` 只能发往私聊，是约 30 秒的临时预览；定稿后必须再用 `sendMessage` 发出完整消息。`draft_id` 不能为 0，同一 `draft_id` 的更新带动画。传 `can_stop` 会显示停止按钮，用户按下后机器人收到 `stopped_message_generation` 更新。
- 参考 [AstrBot](https://github.com/AstrBotDevs/AstrBot/tree/cd405f2a77f3e3eae5a8c747596daaa7655c85b2)（`cd405f2`）与 [MaiBot](https://github.com/Mai-with-u/MaiBot/tree/062d45cf8bd4272ac57d7ec03fcf405e5a458211)（`062d45c`）及其 QQ 适配器 [MaiBot-Napcat-Adapter](https://github.com/Mai-with-u/MaiBot-Napcat-Adapter/tree/d87203e2eff1835895ee34196b17a571dc6bbe24)（`d87203e`），均为 2026-10-27 只读调研时的固定提交：
  - 两者都把平台适配器放在机器人主进程内，或放在主进程监督的子进程内，不交给外部去管。
  - 它们反复踩到的坑有四类：白名单为空时默认放行；去重只存在内存里，重启后重复处理（[AstrBot #8314](https://github.com/AstrBotDevs/AstrBot/issues/8314)）；投递结果不确定时自动重发，造成重复发言（[MaiBot-Napcat-Adapter #100](https://github.com/Mai-with-u/MaiBot-Napcat-Adapter/issues/100)）；token 随 URL 进入日志（[AstrBot #10027](https://github.com/AstrBotDevs/AstrBot/issues/10027)）。另外 [MaiBot #2092](https://github.com/Mai-with-u/MaiBot/issues/2092) 说明，进程活着不等于适配器可用。

把 IM 当作 CLI/TUI 那样的前端，这个前提不成立。IM 没有自己的界面，用户也不在本机，真正的前端是 IM 平台。网关做的事是常驻、连外部服务、使用凭据，和 `lattice-llm` 一样，是后台向外的适配。如果把它放在后台之外，就要另做进程监督、另行处理 token 的读取，还要在 `serve()` 里为它唯一一个用户加停机钩子。这与 ADR 0010“凭据只由后台读取”的方向相反。

## 决策

### 1. IM 网关是后台内的向外适配，不是前端

IM 网关在本地后台进程内运行，与后台同起同停。它和 `lattice-llm` 处在同一层，是后台面向外部服务的适配，与 CLI/TUI 不是一类。CLI/TUI 仍经 `LocalClient` 与后台通信；IM 网关不经 socket，在进程内直接调用后台核心（第 3 节）。

不提供 `lattice im serve`，不另出二进制，也不提供 systemd 服务。IM 只在后台运行时可用。

### 2. crate 与依赖方向

- 新建库 crate `lattice-im`，依赖方向比照 `lattice-llm`：`lattice-api` 依赖 `lattice-im`，`lattice-im` 永远不依赖 `lattice-api`，也不认识 `Backend`。
- 网关编排放在 `lattice-api/src/local/im.rs`，与 `server.rs` 同级。它负责把平台事件接到后台核心、做重启恢复、管理 IM 生命周期。
- `lattice-im` 只放平台适配和与后台无关的逻辑，只用下层 crate 的类型：`SessionId`/`TurnId` 来自 `lancet-protocol`，`NodeId` 来自 `lattice-session`。另外依赖 `net-guard` 与 `reqwest`。
- `lattice-cli` 不依赖 `lattice-im`。`lattice im` 子命令只经 `LocalClient` 发请求（第 5 节）。

### 3. 后台核心与 IPC 解耦：先行的纯重构

在做 IM 之前，先单独合入一个 `lattice-api` 纯重构 PR，目的是让 `Backend` 核心不再绑定传输方式，IPC 只作为一层薄适配。这个 PR 不改变任何行为：IPC 协议、版本号、请求与响应的语义都不变，以现有的 `server_cancel_tests.rs`、`client_tests.rs` 兜底。重构分五项：

1. **观察流。** 从 `watch()` 中拆出核心的 `observe(session_id)`，它产出 `ViewUpdate` 流。generation 检查和 owner/诊断头的修补留在核心里；socket 适配层只负责写帧，对端关闭时丢弃这条流。
2. **提交者存活信号。** 把 `submitter: Option<UnixStream>` 换成与传输无关的存活信号。socket 一侧继续用 `MSG_PEEK` 判断；进程内的调用方恒为存活。
3. **进程内轮次句柄。** 在后台一侧提供进程内的轮次句柄，负责接受回执、更新流和取消。它没有传输丢失的问题，比 `LocalTurn` 简单，但仍要处理 `Acceptance::ConfirmationUnknown`。
4. **停机通知。** 置 `stopping` 时同时推送 `changed`，进程内的组件可以订阅停机。
5. **按 TurnId 取消。** 把 `dispatch` 中现有的 `Request::Cancel { turn_id }` 逻辑（`server.rs:664–676`：若该轮次有准入 owner，就置其 `execution_cancel`；若没有，就到会话存储中确认该轮次存在）移进核心的 `cancel(turn_id)`。IPC 适配层、进程内轮次句柄的取消，以及 IM 对重启后恢复的轮次（它们没有句柄）的取消，都调用这一个入口。这只是挪位置，行为不变。

名称与签名由重构 PR 设计，本文只约束职责。这个 PR **先于** #315 合并，#315 在它之上变基。#315 要把 `FrozenConfiguration` 移出提交路径，会与第 2、3 项重叠，冲突由 #315 一方解决。

### 4. 生命周期

- **启动。** `serve()` 在 `Backend::new`（`:964`）之后启动 IM 任务。如果 token 不存在或白名单为空，就不启动 IM，后台照常运行。
- **上限。** 进程内调用不占连接数和 watch 订阅数的上限，但提交仍要经过准入 `claim`，受 owner 32 的上限约束（超出时回 Busy），这个额度与 CLI/TUI 共用。
- **停机。** IM 经 `changed` 感知 `stopping` 后停止拉取更新。已登记的轮次照常把终稿投递完再退出；在此期间 IM 的新提交会被准入拒为 `Stopping`。`serve()` 在 `owners == 0` 之后、`closed` 之前等待 IM 任务退出，然后才清理实例。IM 不建立连接，所以 accept 循环何时退出与它无关。
- **失败重启。** IM 任务失败只影响后台内部，由 `im.rs` 内的监督循环带退避重启，不拖垮后台。
- **状态。** IM 的状态归入后台状态（`lattice daemon status` / `DaemonStatus`）上报，不另设状态入口。健康状态看平台是否可用，而不是任务是否还活着。

### 5. 命令行

`lattice im auth set|remove|status <platform>` 和 `lattice auth` 一样，通过 `LocalClient` 交给后台 `dispatch` 执行，由后台读写凭据。key 只从不回显的终端或标准输入读取，不接受命令行参数。为此要在私有 IPC 中加上对应的 `Request`/`Response` 变体和客户端方法。`lattice im` 只放管理子命令。

### 6. 凭据

- IM 凭据集中存放在 `~/.lattice/im/auth.json`，按平台分键（v1 只有 `telegram`），由 `lattice-im` 中一个与平台无关的凭据模块读写。token 不进配置文件，也不经环境变量、命令行参数或 `.env` 传入。
- 安全规则沿用 PR #325：
  - 检查文件属主；
  - 目录必须是 0700，组或其他用户有任何权限位就拒绝，不自动修改；文件必须是 0600；
  - 拒绝符号链接；
  - 写入时先写临时文件再改名，保证原子性；
  - 秘密类型的 `Debug` 输出 `[REDACTED]`，不进入错误信息或日志。
  这套实现复制进 IM 的凭据模块，不从 `lattice-llm/src/auth/file.rs` 中抽出共享，以免让两者产生依赖。
- 不并入 `~/.lattice/auth.json`。那个文件的格式和读写都属于 `lattice-llm` 私有，放进去就等于把 IM 的数据结构塞进模型接入层。

### 7. `lattice-im` 的模块与依赖规则

| 模块 | 职责 | 依赖规则 |
|---|---|---|
| `policy` | 身份键、白名单授权、命令解析、Busy 判定 | 只依赖 `std` |
| `ledger` | 入站去重台账的规则与落盘 | 规则函数不依赖外部 crate |
| `reply` | 按平台上限切分回复 | 纯逻辑 |
| `stream` | 流式预览的累积与节流 | 纯逻辑，外加 `NodeId` |
| `telegram` | Bot API 客户端、offset、`TelegramIo` 的生产实现 | `reqwest`、`net-guard` |
| `config` | 白名单、workspace、agent、状态路径、回复上限 | `serde`/`toml` |
| `credentials` | `im/auth.json` | 见第 6 节 |

- `SenderKey`（谁发的，只用于授权）、`ChatKey`（哪段对话，用于会话绑定、回复目标和 Busy）、`InboundId`（哪条入站事件，用于去重）都是不透明键，序列化时带平台前缀，如 `tg:123`。Telegram 的 `i64` 只出现在 `telegram` 模块里，在边界处换成这些键。`policy` 不接触协议 id；协议 id 只出现在 `ledger` 与 `local/im.rs`。
- 唯一的 trait 是 `TelegramIo`。没有真实 Bot 就无法做闭环测试，而 `commit` 的替身可以断言确认时机。不建 `Platform` trait，也不给后台核心、存储、时钟加 trait：测试直接用进程内核心加临时 state root。
- `policy`、`reply`、`stream` 不得依赖 `telegram`，`lattice-im` 内任何模块都不得依赖 `lattice-api`。

### 8. 去重台账与平台确认

- `TurnId` 照常随机生成，不从 `InboundId` 派生。台账记录 `InboundId → TurnId` 及其相位（Registered / Accepted / Delivered），放在 state root 下的 `im/ledger`，不进 `lattice.redb`。台账跨平台通用。
- 处理顺序：
  1. 先登记 Registered，再经核心提交；
  2. 后台 Accepted 或明确拒绝后，更新相位，并让平台确认这条事件（`TelegramIo::commit`），不必等投递结束；
  3. 结果为 `ConfirmationUnknown` 时，不确认、不换新 `TurnId`，按原 `TurnId` 向后台查询；已用过的 `TurnId` 后台回 `Conflict(TurnIdUsed)`，所以禁止盲目重发。
  重启后，对未完成的条目按 `TurnId` 查询：已接受的继续跟随并补投递；不存在的告知用户“未被执行”。
- offset 是 Telegram 私有的状态，只存在于 `telegram` 模块的内存中，不落盘，台账和网关都看不到。只有连续前缀都已确认，offset 才前移。重启后不带 offset 拉取，未确认的更新由 Telegram 重投，重复的由台账识别。
- GC：只清理 Delivered 时间超过平台最长重投窗口的条目（Telegram 取略大于 24 小时，作为 `telegram` 的平台常量交给网关）。`ChatKey → SessionId` 的绑定不是去重数据，不做 GC。

### 9. 授权、Busy 与拒绝

- v1 只处理私聊。发送者白名单按数字 ID 配置，名单为空时拒绝所有人（fail-closed）。
- 被拒绝的事件（白名单外、非私聊）一律静默确认：只计数，不回复。不确认的话，这条事件会卡住 offset 的连续前缀。
- Busy 按 `ChatKey` 判定，多个聊天可以并行。IM 本地的 Busy 只表示“这个聊天仍持有一个未结束的轮次”，用来先挡掉重复提交，以及决定 `/cancel` 取消谁。能否提交以后台准入为准：直接提交，准入回 Busy（同一会话上已有其他操作）就回复“正在忙”。不先查 owner 再提交，因为查询与提交之间 TUI 可能抢先。

### 10. v1 聊天命令

- `/new`：为这个聊天新建会话，并重新绑定。
- `/cancel`：取消这个聊天正在进行的轮次。持有进程内句柄时经句柄取消；重启后恢复跟随的轮次没有句柄，按台账中的 `TurnId` 调用核心的 `cancel(turn_id)`。两条路径的效果相同。按下草稿上的停止按钮，效果与 `/cancel` 相同。
- `/status`：只读，回复当前绑定的会话（短 id 或 `none`）、运行中还是空闲（看 `session_head().owner`）、后台是否在停机。不报 online/offline，因为 IM 只随后台运行。
- `/usage`：只读，只统计当前会话，按模型列出输入与输出 token。用量未知的尝试计为“部分”，不当作 0。缓存读写、命中率和费用几列在 #315 L3（用量细分与费用估算）落地后补上；届时改用后台的用量查询，IM 不自建统计。

`/status` 与 `/usage` 不提交轮次、不占 Busy、不写台账，但同样要过白名单，回复后确认。其余文本作为一轮提交到绑定的会话，没有绑定会话时先新建一个。

### 11. 回复与流式

- 流式预览用 `sendMessageDraft`，带 `can_stop`。每轮用一个进程内的非零 `draft_id`；收到 `stopped_message_generation` 时，把它编成该聊天的一条入站事件，按 `/cancel` 处理，同样过白名单、走台账和确认。累积与节流放在 `stream` 模块：停顿时在草稿过期前重推；遇到 FLOOD_WAIT 时按返回的秒数退避，只保留最新文本。
- 定稿取自最终 Step 的正文，经 `reply` 按 4096 字符切分后，用 `sendMessage` 串行发出。不拿预览的累积结果当定稿，因为 #312（流式增量与完整消息可能不一致）还没解决。轮次没有正常完成时，附一句简短的结局说明，诊断信息只给类别。
- 投递结果不确定时不自动重发正文。

### 12. 工具：v1 不执行

v1 不执行任何工具：配置所指 agent 的 `[tools].enabled` 必须为空，否则 IM 拒绝启动。这是一个已知的空白，原因是后台的工具授权还没有建起来。允许哪些工具、是否按发送者区分权限、确认交互怎么走，等后台工具授权落地后另行决定。

### 13. 与 #315 的衔接

#315 落地前，`im.rs` 像 CLI/TUI 一样，用 `FrozenConfiguration::from_env` 冻结配置后交给核心提交，零工具检查也沿同一条读配置路径在启动时做。在后台内调用时，读到的是后台的启动环境。

这明确是一个**临时例外**：它违反 ADR 0008 第 10 节和 `local/config.rs:21` 文档注释中“后台不得调用”的规则。只允许 `im.rs` 这一处调用，并在调用处注明是例外，同时引用 #315。

收尾：#315 落地后，删掉 IM 的 `from_env` 调用和这个参数，完全恢复“后台不得调用 `from_env`”的规则；零工具检查改挂到后台给出的配置或拒绝信号上，不为此另建端口。

### 14. 传输与秘密

- 只用 `getUpdates` 长轮询，端点固定为 `api.telegram.org`，不开放 base_url，不支持代理。出站请求经 `net-guard` 校验。
- `reqwest` 的错误在 Display 之前去掉 URL（`without_url`），确保 token 不进错误信息或日志；输出同样要经过 #283 的脱敏。
- 与后台一样只支持 Linux。

### 15. 不在 v1 范围内

- 群组与话题。
- webhook。
- 代理。
- 工具（见第 12 节）。
- 其他 IM 平台：身份键带平台前缀，台账与凭据都按平台分键，这三处留了扩展余地，但不预先抽平台接口。
- 后台自动启动（开机或登录时拉起后台）：没有讨论，本文不处理。

### 16. 验证方式

- 重构 PR：现有后台与客户端测试全部不改、全部通过；IPC 协议的字节格式不变。
- `lattice-im` 单测：
  - `policy`：白名单为空、非私聊、命令解析、Busy；
  - `ledger`：相位判定，用 tempfile 验证原子落盘；
  - `reply`：4096 切分；
  - `stream`：累积、attempt 变化时清空、节流与重推；
  - `credentials`：0700/0600、属主、符号链接、原子写、`Debug` 脱敏；
  - `telegram`：offset 只按连续前缀前移，任何错误的 Display 都不含 token。
- `local/im_tests.rs`：假 `TelegramIo` 加进程内核心加临时 state root，覆盖以下情况：
  - Fresh 事件、重复的 `InboundId`；
  - `ConfirmationUnknown` 不盲目重发；
  - Accepted 后才确认；
  - Busy 与 `Stopping` 拒绝；
  - `/status`、`/usage` 只读；
  - 停机时等终稿投递完再关闭；
  - 重启后对恢复的轮次按 `TurnId` 取消；
  - 未配置时不启动；
  - 启用了工具时拒绝启动。

## 与 ADR 0008、ADR 0010 的关系

- **ADR 0008。** IM 网关不是 0008 意义上的客户端：它不走 `LocalClient`，也不占连接。但它遵守 0008 定下的命令、观察与查询语义：提交、取消、Accepted 与 `ConfirmationUnknown`、`TurnId` 只用一次、观察以会话树为准。第 3 节的重构只调整后台内部结构，0008 的 IPC 协议保持不变。新增的 `lattice im auth` 请求是私有 IPC 上的一个新命令，和 0010 新增 `lattice auth` 的做法相同。
- **ADR 0010。** IM 网关在后台内运行，正好符合 0010“配置与凭据由后台读取，客户端不经手”的原则：token 只由后台读取，`lattice im auth` 经后台写入。IM 凭据单独存文件、不并入 `auth.json`，是因为后者属于 `lattice-llm` 私有，这与 0010 不冲突。在 #315 之前，IM 在后台内调用 `from_env`，是对 ADR 0008 第 10 节“后台不得调用”的明确临时例外（第 13 节）。#315 落地后移除，规则完全恢复。

## 后果

**收益**

- 定位准确：IM 网关与 `lattice-llm` 同层，作为后台的向外适配，不必为它另做进程监督和 token 传递。
- 后台核心与传输解耦后，IPC 只是薄适配层，以后进程内的调用方不必再为 socket 写变通代码。
- 生命周期简单：随后台启动，没有配置就不启动，停机时把终稿投递完，失败时在内部重启，状态随后台一起上报。
- 凭据只由后台读取，安全规则与 PR #325 相同。
- 有持久化台账、不盲目重发、白名单默认拒绝，避开了参考项目反复出现的重复发言和误放行。
- `lattice-im` 中的纯逻辑模块只依赖 `std`，可以直接做单测；全 crate 只有一个 trait。

**代价**

- 有意放宽了先前“并行开发期间 IM 不碰其他模块”的约定：要先改 `lattice-api` 的核心。维护者的意见是：“如果一定需要，那就改……再不清理，只会慢慢地变成技术债。”重构 PR 动的是后台核心，回归风险只能靠现有测试兜住。
- #315 要在重构 PR 之上变基，并承担两者重叠部分的冲突。
- IM 与 CLI/TUI 共用 owner 32 的上限，IM 并发多时，本地前端可能先收到 Busy。
- 后台多了一个向外的网络连接和一份凭据，攻击面变大；IM 出故障时，后台要靠监督循环把它隔离开。
- IM 只在后台运行时可用，而谁来启动后台本文没有决定。后台连同 IM 停机超过 24 小时，未确认的 Telegram 更新会被平台丢弃。
- v1 不执行工具、不支持群组和代理，能力有限。
- `/usage` 在 #315 L3 之前只有输入和输出两列。
- 凭据安全检查的实现与 `lattice-llm` 各存一份。
- #315 之前，后台内有一处违反“后台不得调用 `from_env`”的临时例外（第 13 节）。这留下了一项收尾工作，必须随 #315 清掉。

## 否决的方案

| 方案 | 否决理由 |
|---|---|
| 单独的 `lattice im serve` 进程，由后台监督 | 要另做进程监督，token 要传给外部进程，停机排空要跨进程协调。真正的前端是 IM 平台，没有必要再分出一个进程。 |
| IM 作为 TUI 那样的客户端 crate，由 `lattice-cli` 组合 | 把向外适配误当作前端。要为唯一一个用户在 `serve()` 里加停机钩子，`lattice im auth` 也走不了后台 `dispatch`。 |
| 后台内经回环 `LocalClient` 连自己的 socket | 绕过而不是解决核心与 socket 耦合的问题；还会白占连接与订阅额度，序列化开销和传输丢失的处理也都白做。 |
| 每个平台一个凭据文件 | 文件和权限检查分散。一个按平台分键的文件配一个通用模块就够了。 |
| 把 token 并入后台的 `~/.lattice/auth.json` | 该文件的格式和读写属于 `lattice-llm` 私有，并入会让模型接入层认识 IM。 |
| 平台插件 trait 或注册表 | v1 只有 Telegram，属于单一实现抽象（AGENTS、YAGNI）。 |
| 由 `InboundId` 派生 `TurnId`，或把去重放进 `lattice.redb` | `TurnId` 是严格的 UUID v4；去重只是平台消息的短期备忘，不属于会话事实。 |
| 持久化 offset | 重投由台账识别即可；offset 是 Telegram 的私有状态，不应进入台账。 |
| 先查 owner 再提交 | 查询与提交之间存在竞态，应以准入回执为准。 |
| webhook、代理、`editMessageText` 流式编辑 | 本地单用户不需要开放入站端口；代理会带来 token 发往第三方的风险；私聊有 `sendMessageDraft`。 |
| token 经环境变量或命令行参数传入 | 会泄漏到进程表、shell 历史和子进程中，与 ADR 0010 冲突。 |

## 不在本文决定的事项

- IM 侧的工具执行与授权（第 12 节），以及群组、话题、其他平台、后台自动启动。
- 后台提交接口是否加客户端幂等键、是否由会话树去重：等出现第二个需要幂等提交的调用方再议。

以下各点实现时核对：

1. 重构 PR 中核心接口的名称与签名，以及进程内轮次句柄如何处理 `ConfirmationUnknown`。
2. `serve()` 等待 IM 退出的确切位置（`:982–985` 之后、`:986` 之前）。
3. IM 状态在 `DaemonStatus`（`local/types.rs:432`）中的字段形状。
4. `lattice im auth` 的 `Request`/`Response` 变体形状，以及按 ADR 0008 的版本规则是否需要提升 IPC 版本（当前为 `LTLOCAL5`）。
5. 核心 `cancel(turn_id)` 的名称与签名。
6. `/usage` 在进程内用核心的哪个入口取每轮用量。
7. IM 配置文件（白名单、workspace、agent）放在哪里，用什么格式。
8. `im_tests` 中模型输出的测试替身怎么做。
