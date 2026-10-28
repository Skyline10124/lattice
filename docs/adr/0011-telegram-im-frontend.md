---
status: proposed
---

# ADR 0011：IM 前端以本地后台客户端接入，首个平台为 Telegram

- 状态：Proposed（草案，尚未获维护者批准）
- 日期：2026-10-27
- 修订：2026-10-28 按讨论稿 `im-layering-sketch.md` 的维护者裁决同步：取消代理、v1 不执行工具、加入 `/status`、确定去重台账、offset、观察路径、Busy、入口与台账 GC；2026-10-29 补入流式回复（`sendMessageDraft`）、`/usage` 与集中凭据
- 依赖：[ADR 0008](0008-backend-api-commands-view-queries.md)（命令、视图与查询）、[ADR 0010](0010-protocol-adapters-and-configured-providers.md)（配置与凭据由后台读取）
- 实施跟踪：#332

## 背景

以下事实按 `main` 的 `36f2184`（2026-10-27 04:09 UTC+8）核对，2026-10-28 对引用到的接口在 `33a66fa` 上复核。

- 仓库没有 IM 模块，也没有 IM 相关的 Issue 或 PR。现有前端只有 `lattice-cli` 与 `lattice-tui`，二者经 `lattice-api` 的本地后台客户端访问会话。
- ADR 0008 把本地后台 API 分为命令（`submit`、`cancel`、`rewind`）、视图（`watch`）与查询（`history`、`turn`、`list`/`log`/`audit` 等）。SessionId 与 TurnId 由客户端生成，`submit` 只接受新请求，接受结果未知时按 TurnId 查询找回；重复使用已用过的 TurnId 会得到 `TurnIdUsed` 冲突。
- 客户端公开类型在 `crates/lattice-api/src/local/`：
  - `client.rs`：`LocalClient`（`status`、`ensure_running`、`submit`、`cancel`、`cancel_session`、`session_head`、`watch`、`turn`、`wait_for_turn` 等）与 `LocalWatch`。
  - `operation.rs`：`LocalTurn`（`start_turn`、`follow_turn`、`observe`、`cancel`、`reconnect`、`next`、`detach`）。CLI 的 `lattice run` 用 `start_turn` 提交、用 `follow_turn` 找回后继续观察（`crates/lattice-cli/src/commands/run.rs`）。
  - `types.rs`：`TurnRequest`、`SubmitSession::{New, Existing}`、`TurnLookup::{Accepted, InFlight}`、`SessionHead`（含 `turn_id` 与 `owner`）、`DaemonStatus`、`CancelStatus`、`LocalErrorKind::Busy` 等。
  - `mod.rs`：`new_session_id`、`new_turn_id`（TurnId 是严格的 UUID v4）。
- 会话库是状态根下的 `lattice.redb`，由后台独占（`crates/lattice-api/src/stores.rs`）。CLI、TUI 连接同一后台，同一会话上的并发由后台准入拒绝为 Busy。
- **ADR 0010 的客户端接口尚未落地。** `LocalClient::submit` 与 `start_turn` 仍要求提交端传入 `FrozenConfiguration`，CLI 与 TUI 用 `FrozenConfiguration::from_env` 冻结配置。实施跟踪 #315 仍为 open。
- 工具由 agent profile 的 `[tools].enabled` 选择（`crates/lattice-profile/src/schema.rs` 的 `ToolsSection`，默认空列表；`docs/development/tool-development.md`）。空列表合法：`capability_admission_policy_without_enabled_tools_admits_nothing` 断言此时不准入任何工具。默认模板启用了 `session__todo_write`（`defaults.rs`）。
- 后台的工具授权（按调用方或发送者区分、交互确认）尚未建立；`docs/governance/agent-permissions.md` 只约束开发用 Code Agent，不定义产品运行时权限。
- `net-guard` 提供 `validate_base_url` 与 `resolve_public_socket_addrs`；`lattice-llm` 先校验、再固定地址，并对 reqwest 显式 `no_proxy()`。
- #283（open）登记了产品内各自实现的脱敏。Telegram Bot API 把 token 放在 URL 路径里，reqwest 的 `Error::without_url` 可以去掉错误里的 URL。
- 根 [`AGENTS.md`](../../AGENTS.md) 要求 KISS/YAGNI 与深模块，不为拆分制造透传层和只有一个实现却没有实际用途的抽象。现有 crate 都按职责平铺模块，测试 TUI 时用真实 `LocalClient` 连接测试内的假 IPC 后台（`crates/lattice-tui/src/app_loop_tests.rs` 的 `Peer`），没有后台 trait。
- 仓库只支持 Linux 主机（`docs/architecture/platform.md`）。
- Telegram Bot API 的相关事实按 2026-10-27 的 <https://core.telegram.org/bots/api>（10.3）与 <https://core.telegram.org/bots/faq> 核对：
  - `getUpdates` 长轮询与 webhook 互斥；未取走的更新最多保留 24 小时。
  - `getUpdates` 以 `offset` 确认：用大于某条 `update_id` 的 offset 再次调用，才算确认它及之前的更新。
  - `sendMessage` 正文为实体解析后 1–4096 字符；FAQ 建议单个 chat 每秒不超过一条消息。
  - `sendMessageDraft` 只用于私聊，在生成过程中向用户推送草稿。草稿是约 30 秒的临时预览，定稿须另用 `sendMessage` 发送。正文 0–4096 字符，`draft_id` 非零。
  - 带 `can_stop` 时草稿显示停止按钮；用户按下后 bot 收到 `stopped_message_generation` 更新（`MessageGenerationStopped`，含 `chat` 与 `draft_id`，不含发送者）。
- 以下两条于 2026-10-29 在 `main` 的 `ef51cfd` 上核对：
  - `lattice-llm` 的凭据文件是 `~/.lattice/auth.json`，权限检查与原子替换在 `crates/lattice-llm/src/auth/file.rs`，属 `lattice-llm` 私有。
  - `LocalClient::turn` 找回的 `TurnDetail` 带本轮 `UsageSummary`（只有 prompt 与 completion）；模型写在该轮用户消息节点的 `provider` 与 `model` 上。

## 决策

### 1. 新增库 crate `crates/lattice-im`，作为本地后台的又一个前端

- IM 与 CLI、TUI 同层，只经 `lattice-api` 的 `local` 客户端访问会话，不依赖 `lattice-session`、`lancet-*` 或 `lattice-llm`，不读写会话库。
- IM 连接与 CLI、TUI 相同的后台、状态根与 `lattice.redb`，会话在三者之间共享。同一会话的 Busy 由后台准入跨前端统一判定，IM 不另建一套。
- 首个平台是 Telegram。v1 不支持代理：端点固定 `https://api.telegram.org`，reqwest 显式 `no_proxy()`，不读代理环境变量，也不提供代理配置。

### 2. 分层是依赖规则，不是目录

- 单个 crate，按职责平铺模块：`policy`（身份键、白名单、命令解析、Busy 判定）、`ledger`（去重台账与会话绑定）、`reply`（回复切分）、`gateway`（最小闭环与重启恢复）、`telegram`（Bot API）、`backend`（对 `LocalClient`/`LocalTurn` 的用法封装）、`config`、`auth`（IM 凭据，见第 11 节）。
- 纯逻辑模块（`policy`、`reply`，以及 `ledger` 的规则部分）不依赖 tokio、reqwest、`net-guard` 或 `lattice-api`，直接单元测试。`policy` 只依赖 `std`，不含 SessionId、TurnId 等协议身份；这些只出现在 `gateway`、`ledger`、`backend`。
- 只有 `TelegramIo` 一个 trait，用于测试替身：没有真实 bot 时测试最小闭环与确认时机。不设平台、后台、存储、时钟或身份生成的 trait。后台按 TUI 的做法测试：真实 `LocalClient` 加假 IPC 后台。
- v1 不预建“平台”trait 或插件点。第二个平台出现时，再从两份实现中提取共同部分。

### 3. 平台无关的身份键

- `SenderKey`（谁发的，只用于白名单）、`ChatKey`（哪段对话：会话绑定、回复目标、Busy 粒度）、`InboundId`（哪条入站事件：去重键）是不透明新类型，序列化为带平台前缀的字符串，例如 `tg:123`。
- Telegram 的 `update_id`、`chat.id`、`from.id` 只出现在 `telegram` 模块，在边界换成上述键。纯逻辑模块与磁盘上的台账都不依赖整数形态，以后加平台不需要迁移数据。
- `gateway` 与 `ledger` 对 `InboundId` 只问：见过没有、对应哪个 TurnId、处于哪一相位。它怎样生成、何时怎样向平台确认，全归平台模块。

### 4. 最小闭环与幂等

```text
poll ──► 白名单 ──► 命令解析 ──► 台账分类(InboundId)
                                   │ 新消息
                                   ▼
               生成随机 TurnId，台账登记 InboundId → (ChatKey, SessionId, TurnId)
                                   ▼
               start_turn（LocalTurn）──► Accepted / 明确拒绝 ──► commit(InboundId)
                                   ▼
               观察到这一轮结束 ──► sendMessage（按 4096 切分）
```

- **先登记，再提交。** TurnId 照常随机生成，不从 `InboundId` 派生（TurnId 是严格 UUID v4）。台账先持久记下 `InboundId → TurnId`，再调用 `start_turn`。
- **接受或明确拒绝之后才确认平台。** `gateway` 在后台 Accepted 或明确拒绝后，对这条事件调用一次 `TelegramIo::commit`；不必等回复投递完。接受结果未知时不确认、不换 TurnId 重发，按原 TurnId 用 `LocalClient::turn` 找回。
- **重复投递。** 已登记的 `InboundId` 再次到达时，按原 TurnId 找回或继续观察，不新提交。
- **台账位置。** 跨平台通用，存放在状态根下的 `im/ledger`，不进 `lattice.redb`。权限 0600，不跟随符号链接，先写临时文件并 fsync 再改名替换（与第 11 节 IM 凭据文件的写入方式一致），不含凭据。
- 后续备选：在后台提交接口上加客户端幂等键，由会话树去重。等出现第二个需要幂等提交的前端再议，v1 不做。

### 5. Telegram 的确认与 offset

- offset 是 Telegram 的私有状态，只在 `telegram` 模块内，v1 只在内存，不落盘，也不进台账。重启后不带 offset 调用 `getUpdates`，Telegram 从最早未确认的更新重投，重复的由台账识别。
- `commit` 只标记该事件已确认。只有从当前 offset 起形成连续的已确认前缀时，才推进 offset；中间有尚未确认的事件就等待。
- 限制：IM 停机超过 24 小时，未确认的更新会被 Telegram 丢弃。
- 其他投递形态：需要快速应答、可能重投的推送型平台在收到时即确认，去重完全靠台账；没有确认机制的平台只靠台账。台账与恢复逻辑不随平台变化。

### 6. 授权

- 白名单只用 `SenderKey`（Telegram 为数值 `from.id`，不用用户名）。名单为空时拒绝全部。v1 只接受私聊。
- 被拒的消息（不在名单、非私聊）不提交、不回复、只计数，并静默确认；不确认会在 offset 的连续前缀上形成堵塞。
- `stopped_message_generation` 不带发送者，只有 `chat` 与 `draft_id`，白名单在此间接成立：草稿只发往白名单内的私聊，停止事件须对上内存中一个进行中轮次的 `draft_id`（第 10 节），对不上的只确认。

### 7. 工具：v1 不执行任何工具

- IM 所用 agent 的 `[tools].enabled` 必须为空；启用了任何工具，IM 拒绝启动。
- 这是有意保留的暂时空白：后台工具授权建立之前，从手机发起的消息不能执行任何工具，包括只读工具。届时再决定哪些工具可用、是否按发送者区分权限、IM 上的确认交互（见“不在本文决定的事项”）。
- profile 副本随会话固定（ADR 0010），所以检查放在启动时，对 IM 使用的 agent 做；需要立即生效时在聊天中用 `/new` 开新会话。

### 8. 命令

| 命令 | 行为 |
| --- | --- |
| `/new` | 清除该 chat 的会话绑定，下一条消息用 `SubmitSession::New` |
| `/cancel` | 持有这一轮的 `LocalTurn` 时用 `LocalTurn::cancel`；IM 重启后没有句柄时用 `LocalClient::cancel_session`。按 `CancelStatus` 回复 Taken、NotTaken、Unknown。取消不回滚已发生的效果 |
| `/status` | 只读，见下 |
| `/usage` | 只读，该 chat 当前绑定会话的用量，见下 |

`/status`：

- 后台是否在线用 `LocalClient::status`，失败即报离线；不调用 `ensure_running`，查询状态不拉起后台。
- 会话取台账中该 chat 的绑定，没有则报 none；有绑定且后台在线时读 `LocalClient::session_head`，`owner` 为真报运行中并附 TurnId，否则报空闲。
- 仍须通过白名单；不受 Busy 限制，执行中也可以查；不提交、不写台账条目；回复后立即确认这条事件。重投时再答一次即可。

`/usage`：

- 只统计该 chat 当前绑定的会话，没有绑定则报 none；不跨会话汇总。
- 按模型（`<provider>/<model>`）每行一项，列为：输入、缓存写入、缓存读取、输出、缓存命中率、费用。
- v1 只填输入与输出，取自该会话各轮的 `UsageSummary`，按该轮的模型归行；用量不完整（`Partial`）时注明。缓存写入、缓存读取、命中率与费用四列留空，待 #315 L3 落地后补上（第 12 节）。
- 其余规则同 `/status`：须通过白名单，不受 Busy 限制，不提交、不写台账条目，回复后立即确认。

### 9. 会话映射、观察与 Busy

- 每个 `ChatKey` 同时只有一个当前会话，绑定 `ChatKey → SessionId` 存在 IM 台账中，会话库不记录任何平台标识。`workspace` 与 `agent` 取 IM 配置中固定的值，v1 不能从聊天中切换。
- 新消息用 `LocalClient::start_turn`，由 `LocalTurn` 管理提交、观察、取消意图与 Unknown 重查，与 CLI 同路。重启恢复时，台账中已被接受的轮次用 `follow_turn` 继续观察到结束；`InFlight` 稍后重查；查不到则视为未被接受，回复“该消息未被执行”，不重新提交。
- Busy 按 `ChatKey` 判定，多个 chat 可以并行。IM 本地的 Busy 是“该 chat 仍持有未结束的 `LocalTurn`”，用于 `/cancel` 与本地先行拦截。能否提交以后台准入为准：直接 `start_turn`，回执为 Busy（含 TUI 或 CLI 在同一会话上的操作）时回复“正在忙，可发 /cancel”，不排队、不自动重试。不在提交前先查 `owner`，因为查询与提交之间别的前端可能抢先。
- v1 不为每个 chat 常驻 `watch` 来预判忙闲。
- 维护者指出“先提交、被拒再处理”的写法不顺手；以后可以考虑让 `lattice-api` 更直接地向前端暴露准入结果，不在 v1 范围。

### 10. 回复

- **流式预览。** 执行中用 `sendMessageDraft` 推送助手正文的草稿，按纯文本发送。v1 只接受私聊（第 6 节），满足它只用于私聊的限制。
  - 每轮取一个非零 `draft_id`，IM 在内存中记 `draft_id → LocalTurn`，不落盘、不进台账。
  - 草稿约 30 秒失效：正文没有变化时也在失效前刷新。刷新有节流，遵守单 chat 约每秒一条。
  - 发送时带 `can_stop`。收到 `stopped_message_generation` 时按 `chat` 与 `draft_id` 找到这一轮，用 `LocalTurn::cancel` 取消，按 `CancelStatus` 回复，同 `/cancel`。找不到对应的轮（已结束或 IM 重启过）时不取消，只确认。这条更新不提交、不写台账条目，处理后立即确认。
- **最终回复。** 这一轮结束后照常用 `sendMessage` 发送，草稿不作为定稿。取最后一步的助手正文与 `TurnSummary` 结局，按纯文本发送（不设 `parse_mode`）。超过 4096 字符时按段落、其次按行、最后硬切成多条，按顺序发送并遵守单 chat 约每秒一条。上限作为参数交给切分逻辑。
- 结局不是 Completed 时附一行说明（Cancelled/Failed 与诊断摘要），诊断经过脱敏。
- 不用 `editMessageText` 流式编辑（见“否决的方案”）。

### 11. 传输与 Bot token

- v1 用 `getUpdates` 长轮询，不用 webhook：只需要出站 HTTPS，不引入公网入站端点。用 `net_guard::validate_base_url` 校验、`resolve_public_socket_addrs` 解析并固定地址；`allowed_updates` 只订阅 `message` 与 `stopped_message_generation`。
- **凭据集中存放。** 所有 IM 平台的密钥都在一个文件 `~/.lattice/im/auth.json` 中，按平台分段（键如 `telegram`）。由 `lattice-im` 中一个通用的 IM 凭据模块（`auth`）管理；加平台只加一个键，不写按平台的存储代码。
- 该文件不并入 `lattice-llm` 的 `~/.lattice/auth.json`，后者是 `lattice-llm` 私有的。
- 权限检查（0600、属主、不跟随符号链接）与原子写入（临时文件、fsync、改名替换）从 `lattice-llm` 复制一份到 IM，不从 `lattice-llm` 抽出公用：并行开发期间 IM 不改动其他模块。
- token 不作为命令行参数，不放进环境变量（`ensure_running` 拉起的后台会继承 IM 的环境），不从工作区 `.env` 或项目 `.lattice/` 读取，不进入台账、日志、诊断或任何回复。录入方式仿照 `lattice auth set`，写入 `im/auth.json` 的对应平台段。
- 每个传输错误先 `without_url()`，再按已知值替换 token，然后才能进入日志或诊断。归属随 #283 的裁决而定。

### 12. 与 #315 并行

- IM 不等 #315。#315 落地前，`backend` 与 CLI、TUI 一样用 `FrozenConfiguration::from_env` 交给 `start_turn`，零工具检查在 IM 启动时沿同一读配置路径进行。
- #315 落地后删掉该参数，零工具检查改挂到后台给出的配置或拒绝信号上，细节届时再定。
- `/usage` 的缓存写入、缓存读取、命中率与费用四列在 #315 L3 落地后补上（第 8 节）。
- **提议（未经维护者确认）**：IM 与 #315 谁后合并，谁负责对齐。

### 13. 入口

- 作为 `lattice-cli` 的子命令启动，名称暂定 `lattice im`。`lattice-im` 保持库 crate，`lattice-cli` 只做参数解析与启动，不另出二进制。

### 14. 台账清理

- 只清理 Delivered 条目，且只清已超过平台最长重投窗口的条目。Telegram 取略大于 24 小时，该时长是 `telegram` 模块提供的平台常量，`ledger` 不认识 Telegram。
- Registered、Accepted 一律保留。不在确认后立刻删除：Telegram 要到下一次带 offset 的 `getUpdates` 才真正确认，其间崩溃会重投。
- `ChatKey → SessionId` 绑定不属去重数据，不清理。v1 只在启动时清理一次。

### 15. v1 不做的事

- 代理。
- 工具执行。
- 群组、频道、话题分会话；多用户隔离工作区。
- Telegram 以外的平台；webhook。
- 图片、文件、语音输入与附件回复。
- 聊天内切换工作区或 agent、`rewind`、历史翻页、排队提交。

## 与 ADR 0008、ADR 0010 的关系

- **ADR 0008**：本 ADR 只是它的又一个客户端，不新增命令、视图或查询，不改 IPC 协议。第 4、8、9、10 节依赖它的“客户端生成身份、按 TurnId 找回与取消”。
- **ADR 0010**：第 11 节沿用它的凭据不外泄与项目配置不可信原则；IM 平台凭据不是模型凭据，不进它的 `auth.json`，另存于 `~/.lattice/im/auth.json`。第 12 节说明 #315 落地前后的过渡，IM 不在 #315 之后保留由客户端读取配置的路径。
- 重连语义取 ADR 0008 的视图与 ADR 0007 的“执行者消失后的 Turn”，不采用已被取代的 ADR 0006 第 8、10 节。本 ADR 不修订其他 ADR。

## 后果

**收益**

- 后台与会话合同不变，IM 复用找回、取消、重连与 Busy 准入的既有语义；会话可在 IM、TUI、CLI 之间接续。
- 先登记再确认，Telegram 重投与 IM 崩溃都不会产生重复 Turn；台账与平台确认解耦，加平台不改台账与恢复逻辑。
- 长轮询只需要出站，不引入公网入站面。
- 长任务在手机端可看到流式预览，并可用草稿上的停止按钮取消；`/usage` 可看当前会话按模型的用量。
- IM 凭据集中在一个文件，加平台不加存储代码；IM 不改 `lattice-llm`，可与其他工作并行。
- 白名单为空即全拒，且 v1 不执行任何工具，手机上的消息不能修改工作区，也不能访问网络或读取工作区。

**代价**

- 新增一个 crate、一份 IM 台账和一个 IM 凭据文件（`~/.lattice/im/auth.json`）。
- v1 的 IM 只能对话，不能用工具，实用性明显低于 TUI，要等后台工具授权补上。
- 不支持代理，在无法直连 Telegram 的网络中 IM 不可用。
- offset 不落盘，停机超过 24 小时会丢失未确认消息。
- #315 落地前，IM 多出一处由客户端读取配置的路径，落地时需要删除。
- 草稿须在约 30 秒内刷新，长任务的请求量随之增加；停止按钮只在 IM 持有该轮时有效。
- 权限检查与原子写入在 `lattice-llm` 与 IM 各有一份，修改时须两处同步。
- #315 L3 落地前，`/usage` 只有输入与输出两列有值。

## 否决的方案

| 方案 | 否决理由 |
| --- | --- |
| v1 用 webhook | 需要公网 HTTPS 入站端点与证书，仓库没有入站安全合同，`net-guard` 也不覆盖入站 |
| 把 Telegram 轮询放进后台进程 | 扩大后台的职责与凭据面；ADR 0008 已把前端定位为客户端 |
| 直接依赖 `lattice-session` 读写会话库 | 绕过后台的实例锁与准入 |
| 按领域层、应用层、适配层分目录，并为后台、存储、时钟、身份生成设 trait | 每个 trait 只有一个实现，违反根 `AGENTS.md`；仓库按职责平铺模块，后台已有假 IPC 的测试做法 |
| 平台无关的 IM trait 与插件机制 | 只有一个平台，没有第二个实例可以验证 |
| 由 `InboundId` 派生 TurnId | TurnId 是严格 UUID v4；派生规则会把平台身份带进会话树 |
| 把去重放进 `lattice.redb` 或后台 | 需要修改后台提交接口；v1 用 IM 自己的台账即可，后台幂等键留作后续备选 |
| offset 写入台账或落盘 | 平台私有状态混入通用台账；Telegram 服务端已记录确认位置 |
| 提交前先查 `owner` 判断 Busy | 查询与提交之间别的前端可能抢先，最终仍要处理后台的 Busy |
| v1 只启用只读工具 | 后台工具授权尚未建立，客户端过滤会形成第二套权限真相 |
| 用 `editMessageText` 流式编辑 | 受单 chat 速率限制，长输出要换消息，容易触发 429；私聊流式改用 `sendMessageDraft` |
| 用用户名做白名单 | 用户名可以修改或转让 |
| bot token 用环境变量或命令行参数 | 环境变量会被自动拉起的后台继承，命令行参数会进入进程列表 |
| IM 凭据并入 `lattice-llm` 的 `auth.json` | 该文件是 `lattice-llm` 私有的；IM 不依赖 `lattice-llm` |
| 每个平台一个凭据文件或一套存储代码 | 加平台就要加存储代码；按平台分段的单文件即可 |
| 把权限检查与原子写入从 `lattice-llm` 抽成公用模块 | 并行开发期间要改动其他模块；v1 复制一份 |
| 支持代理 | 维护者裁决 v1 不做 |

## 不在本文决定的事项

- **工具授权（暂时空白）。** 后台工具授权建立后再决定：允许哪些工具（是否含只读工具）；是否按 `SenderKey` 区分权限；IM 上的确认或拒绝交互怎样进行。
- **IM 配置的位置与键名。** 白名单、workspace、agent 等配置放在哪里、键名与格式，未定。台账位置已在第 4 节确定。
- **子命令名。** 暂定 `lattice im`，未确认；是否以 systemd user service 常驻，未定。
- **#315 的合并对齐约定。** 第 12 节“谁后合并谁对齐”是提议，待维护者确认。
- **话题与群组。** 是否按 `message_thread_id` 分会话，群组怎样授权，v1 不支持，后续方向未定。
- **草稿超长。** 草稿正文超过 4096 字符时预览哪一段，实现时定。
- **`/usage` 的取数。** v1 按轮的用量与模型从哪个客户端接口读取（`TurnDetail` 不带模型，模型在用户消息节点上），实现时核对。
- **IM 凭据的录入命令。** 子命令名随 `lattice im` 一并待定。
- **回复内容的取舍。** 最终回复是否附工具摘要（工具启用后）与本轮用量（会话用量见 `/usage`），诊断显示到什么程度。
- **ADR 编号。** `docs/adr` 中没有 0009，本文取 0011；0009 是否预留，未核实。

## 验证方式

- 单元测试：
  - 白名单为空拒绝、非私聊拒绝、命令解析（含 `/status`、`/usage`）、Busy 判定；
  - 台账相位分类，状态文件的权限与原子写入，清理只删超过窗口的 Delivered；
  - 4096 字符切分；
  - `/usage` 按模型分行，v1 只填输入与输出，其余四列留空，`Partial` 时注明；
  - IM 凭据文件：权限与属主检查、拒绝符号链接、原子写入，按平台键读写；
  - Telegram 更新到身份键的转换（带 `tg:` 前缀）；乱序 `commit` 只推进连续前缀的 offset；
  - token 脱敏：传输错误去掉 URL 后不含 token。
  - 草稿刷新：正文不变时也在 30 秒内刷新，刷新节流不超过单 chat 约每秒一条。
- 启动检查：agent 启用了任何工具时拒绝启动。
- 集成测试：假 `TelegramIo` 加真实 `LocalClient` 与假 IPC 后台（按 TUI 的做法），覆盖：
  - 普通消息 → Accepted → commit → 最终回复；
  - 重复 `InboundId` 不重复提交；接受结果未知时不 commit、不重发，按 TurnId 找回；
  - 重启恢复：Accepted 用 `follow_turn`，查不到回复“未被执行”；
  - 后台回 Busy 时的回复；`/cancel` 的 Taken 与 NotTaken；
  - `/status` 不提交、执行中可查、后台离线时不调用 `ensure_running`、回复后 commit；`/usage` 只统计当前绑定会话；
  - 流式：草稿带 `can_stop`，结束后另发 `sendMessage` 定稿；`stopped_message_generation` 映射为 `LocalTurn::cancel` 并 commit，`draft_id` 找不到时不取消。
