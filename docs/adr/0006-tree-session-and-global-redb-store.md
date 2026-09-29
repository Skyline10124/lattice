---
status: accepted
---

# ADR 0006：tree-session 与全局 redb 会话存储

- 状态：Accepted（2026-09-26 维护者批准；2026-09-29 重新批准槽位结局两维修订；记录见 #223。2026-09-30 维护者裁决修订验证产物要求，记录见 #230）
- 日期：2026-09-26（裁决日期 2026-09-25）
- 修订：部分取代 ADR 0005，范围见[与 ADR 0005 的关系](#与-adr-0005-的关系)；取代 ADR 0003 的“Rewind”一节
- 依赖：ADR 0005 未被取代的部分
- 实施跟踪：GitHub #223（路线图 #217 R2）

## 背景

ADR 0005 把会话、进行中的 Turn 与调用事实放在三个私有 Git 仓库中：全局 `sessions.git`，以及每项目的 `invocations.git` 与 `workspace-history.git`。Git 提供内容寻址和只追加历史，但一次状态变化要写对象、CAS 更新引用、再补 fsync 屏障，多个引用之间没有事务。为此系统需要一整套协调结构：

- 进行中的 Turn 先写不可变的 PendingTurn journal，结束时再整体发布 TurnCommit；为发布中断另设 TurnClosing 阶段、永久结果 pin 和逐项拒绝检查。
- 调用账本是独立的只追加 ledger，靠周期 anchor checkpoint 控制冷启动成本；跨库时序靠 5 段式 per-binding 顺序和候选修订流程维持。
- 保存层为防引用间不一致累积了大量相互印证的检查。#217 [评论](https://github.com/Skyline10124/lattice/issues/217#issuecomment-5798632309)列出 48+2 处只在人为构造条件下起作用的冗余检查。

这些结构不来自会话语义，而来自"多个引用无法原子更新"。同时会话本身是线性的：`checkout`、`reset`、命名分支和 `diff` 直接暴露 Git 表示，用户要理解 OID 才能回到旧位置。

R1（#218 / PR #219）已经把执行交给单实例本地后台，存储层只剩一个写入进程。这使嵌入式事务库成为可行选项：一个库文件、一个进程持有、多表单事务提交。

## 决策

### 1. 存储作用域与布局

LATTICE 后台是全局会话网关，本身没有项目概念；Session 对项目只是一个绑定字段。R2 后状态根只有两种存储：

| 存储 | 作用域 | 内容 |
| --- | --- | --- |
| `lattice.redb` | 全局一份 | Session、树节点、Step 记录、Turn 记录、调用槽位与索引 |
| `projects/<ProjectStoreKey>/workspace-history.git` | 每项目一份 | LANCET 的项目全局文件变更历史，记录语义不变；不再为 Turn 保存 checkpoint |

- `sessions.git` 与 `projects/<key>/invocations.git` 删除，不换成项目级表。
- DispatchStarted、调用终态与 Cell evidence 是协调器视角的事实（"已把调用交出去""收到了结果"），落在进行中步骤的调用槽位上。
- 操作员查询 `lattice audit`、`audit --tail`、`audit replay` 语义保留，改读全局库并按项目绑定过滤；`--tail` 游标为全局库内的单调变更序号。
- 调用槽位与 Step 同事务写入，归 `lattice-session`。操作员查询由 `lattice-session` 提供只读接口、经 `lancet-api` 对外；“操作员读取不依赖 Tool runtime”的约束改为约束 `lattice-session`。`lancet-audit` crate 删除，它与 `lattice-session` 之间的两条边界检查随之删除；工作区唯一的 XDG 测试 guard `ScopedXdgStateHome` 迁出。
- 将来工具执行拆到独立 workspace node 时，node 侧是否另存接收与执行事实，按届时的恢复保证决定，本阶段不预建。

redb 的相关保证：写事务使用 `Durability::Immediate` 时，`commit` 返回即持久；同一时刻只有一个写事务；打开时对库文件取独占锁，另一进程打开得到 `DatabaseAlreadyOpen`。平台不支持文件锁时 redb 只告警并继续，所以单进程持有仍由 R1 的后台实例锁保证，不依赖 redb 的锁。

使用条件：redb ≥ 4.3.0；持久化文件后端；默认独占写模式，不启用 `experimental-multiprocess`；所有写事务使用 `Durability::Immediate`，不使用 `Durability::None`。要求 4.3.0 是因为 4.2.0 存在已知恢复缺陷：`check_integrity()` 修复过的库在提交后不久崩溃，恢复时可能静默回退该提交（4.3.0 CHANGELOG 已修复）。第 10 节"完成确认"依赖这些条件。

### 2. 旧数据

旧 `sessions.git` 与 `invocations.git` 不迁移，也不只读保留。后台发现旧库时打开失败，错误指出路径，由人删除或存档。旧 raw OID 输入一律无效。

项目仍在快速迭代，本机是唯一用户，旧数据没有保留义务；R1 已对旧账本和 pending 快照采用同样的拒绝方式。#208 讨论的 checkout/reset/diff 对 OID 的准入，随这些命令删除而失去对象。

### 3. 会话树

会话历史是一棵只追加的树。

- **节点**两种：`UserMessage` 是一条用户输入；`Step` 是一次完整模型回复，连同它发出的全部 `(ToolCall, ToolResult)` 对。没有工具调用的回复是 0 对的 Step。
- 配对靠构造保证：一个 Step 只有在全部调用都有结果（含第 7 节的类型化终态）后才写入树，所以树上任何时刻不存在有调用、无结果的节点。
- 节点记录父节点和所属 `turn_id`，不记录 Turn 结局。
- **根节点 `0`** 在 Session 建立时写入，没有内容、不属于任何 Turn，代表空对话；树始终单根，节点 `0` 不发给 provider。没有它，第一条消息之前就没有可以退回的位置。
- **NodeId** 是每 Session 单调递增的 `u64`，写节点时分配，第一条 UserMessage 为 `1`，父节点序号恒小于子节点。跨 Session 引用写作 `(SessionId, seq)`。用户可以直接输入序号。
- **当前位置**是 Session 记录中的一个 NodeId，始终有值。新节点以当前位置为父，写入后当前位置前移到新节点。
- 连续两个 `UserMessage` 是合法结构（例如取消了一个尚无 Step 的 Turn 后再发送）；provider 投影按适配器要求合并或原样传递。

`TurnId` 仍是 UUID v4，承担提交去重与找回，与 NodeId 分工不同。

### 4. 会话操作

唯一的移动操作是 `rewind <seq>`：把当前位置移到树上任意已有节点本身，之后 `send` 的新节点以它为父，原分支保留在树上。改写某条用户消息就是退回到它的父节点再发送，第一条消息即 `rewind 0`。会话层只保留这一种语义，不提供 `send --edit` 之类的第二入口；顺手的编辑交互由前端组合这两步实现。

- 不保留分支名，不设 `branch` 命令。`log` 以树形展示结构。
- rewind 只切换对话位置，不回退工作区文件。原有的文件 Rewind（按目标 Turn 的 checkpoint 撤销其后全部 LATTICE 变更后移动会话引用）连同 `RewindCoordinator`、`lancet-api` 的恢复规划与执行接口、`lancet-runtime` 的恢复执行和 Rewind 审计链一并删除。
- Turn checkpoint 一并删除。它唯一的功能读取方是文件 Rewind，其余只在 `log` 中显示。Turn 记录不再保存工作区历史位置；关闭 Turn（含自动关闭）只写会话库，不取工作区操作锁、不读 `workspace-history.git`，所以工作区历史库出错不会阻止 Turn 关闭。WorkspaceHistory 照旧逐次记录变更的前后内容，每条记录带 `TurnId` 与 `InvocationId`，`current` 只前进；会话与历史的对应由这两个身份承载。文件恢复如需提供，从中推导位置，另行裁决设计。
- `lattice session` 子命令只剩 `send`、`rewind`、`list`、`log`、`turn`、`cancel`。删除 `new`、`status`、`checkout`、`reset`、`diff` 与独立 `agent` 命令。顶层 `lattice run` 保持现状。
- `session send` 不带 `--session` 时创建新 Session：项目绑定从当前目录解析，Session 记录与第一个 Turn 在同一接受事务中写入，`--agent` 必填。带 `--session` 时向已有 Session 提交。同一 `--turn-id` 重发按 Turn 记录找回原请求及其 Session，首次发送确认不明时重发不会多建 Session。
- Session 记录只保存身份、项目绑定、当前位置、进行中 Turn 与时间；不再保存 `project_target_branch` 与 `label`。前者只被校验、保存和显示，没有行为依赖。TUI 不带 Session 启动时，在首次提交时创建。
- `list` 每行显示 SessionId、项目绑定、当前节点序号、节点数、是否有进行中 Turn 与更新时间。`log <session>` 开头显示当前位置、当前 agent 与进行中 Turn，随后以树形列出全部节点（含不在当前分叉上的节点），标出当前位置，并在每个 Turn 的首节点注明 TurnId、结局与 notice。`turn` 负责单个 Turn 的详情与断线后续观察；详情（含 `--json`）按步骤序号列出全部 Step 记录：各尝试终态、`end` 的类型与原因、对应节点序号（未写节点时为无）。这使 Step 记录与无节点的 `Stop` 能经公开 CLI 在 E2E 中断言。
- 不设 Session 级"当前 agent"字段。agent 是 Turn 冻结配置的一部分，当前 agent 即当前位置所属 Turn 的 agent；`send --agent` 覆盖。rewind 到旧节点后自然回到当时的 agent。当前位置为根节点 `0` 时没有当前 agent，`send` 必须带 `--agent`。
- Session 有由 Owner 执行的进行中 Turn 时（判定看整个 Turn，不只看是否存在 ActiveStep），不同请求的 `send` 与 `rewind` 返回 Busy，与 R1 的同 Session 排他一致。同 `--turn-id` 重发找回原请求；无 Owner 的崩溃遗留 Turn 按第 8 节先自动关闭。

### 5. 三层记录与 Turn 生命周期

| 层 | 键 | 内容 | 写入与修改 |
| --- | --- | --- | --- |
| 树节点 | `(SessionId, seq)` | UserMessage 或 Step 的对话内容、父节点、`turn_id` | 只追加 |
| Step 记录 | `(TurnId, 步骤序号)` | `attempts`：每次 provider 请求的终态、usage 与诊断；`end`：`Continue(node) \| Finish(node) \| Stop { node: Option<NodeId>, cause }` | 进行中即 ActiveStep，就地更新；关闭后不再修改 |
| Turn 记录 | `TurnId` | Session、用户节点、冻结配置摘要、开始时间；关闭时补 `outcome`、`reason`、`notice`、usage 汇总与结束时间 | 接受时创建，关闭时补全，之后不再修改 |

Turn 的正常流程：

1. **接受**：同一事务写入 `UserMessage` 节点、未关闭的 Turn 记录，并把 Session 的当前位置与进行中 Turn 指向它们；新建 Session 时 Session 记录也在这个事务中创建。这个提交是请求接受的持久边界，承接 ADR 0005 的 `TurnStarted`。同 `TurnId` 重试按 Turn 记录找回原请求。
2. **请求模型**：写入 ActiveStep，状态 `Requesting { attempt }`。请求以停止结束时，单独提交追加到 `attempts` 的 `Stopped` 终态（不再重试时同一事务关闭 Step 与 Turn）。
3. **收到完整回复**：`Completed` 终态不单独提交，与下一个状态变化同事务写入，所以不存在“回复已保存、尚未登记也未关闭”的持久状态。无调用时，同一事务追加 `Completed`、写 Step 节点、以 `Finish` 关闭 Step 记录，并关闭 Turn（第 6 步）。有调用时，同一事务追加 `Completed`、在 ActiveStep 记下完整回复，为每个调用建立槽位并分配 InvocationId，状态转为执行。
4. **执行调用**：每个调用执行前单独提交槽位 `Started`；结果到达后单独提交终态与载荷。写事务内不等待模型、Tool 或网络。
5. **关闭 Step**：全部槽位有结果后，同一事务写 Step 节点、关闭 Step 记录、清除 ActiveStep、前移当前位置。协调器决定继续时 `end = Continue(node)`，回到第 2 步开始下一步骤。
6. **关闭 Turn**：最后一个 Step 关闭的同一事务补全 Turn 记录并清除 Session 的进行中 Turn。

不变量：

- 进行中的 Turn：零条或若干条以 `Continue` 关闭的 Step 记录，加至多一条 ActiveStep；接受后或 `Continue` 之后、下一步开始前没有 ActiveStep 是合法状态。
- 已关闭的 Turn：至少一条 Step 记录、没有 ActiveStep，只有最后一条的 `end` 是 `Finish` 或 `Stop`；这个值就定义了"最后一个"，各 Step 记录形状一致。
- `TurnOutcome` 由最后一个 Step 的 `end` 派生，存入 Turn 记录，来源唯一。
- 同一 Session 最多一条 ActiveStep，且仅在 Turn 进行中存在。"只追加不改写"约束树与已关闭记录，不约束 ActiveStep。

ActiveStep 取代 PendingTurn journal、StepClosing、TurnClosing 与结果 pin；Turn 不再有独立的最终发布。

### 6. 模型尝试与 Turn 结局

一次 provider 请求的终态：

```text
AttemptEnd = Completed { step }
           | Stopped { cause, diagnostic }

cause = ProviderError { retryable }
      | Cancelled
      | Deadline
      | OutputLimit
      | InvalidBatch
      | ProcessExit
```

- `ModelAttemptStarted` 不再是事件，改为 ActiveStep 的 `Requesting { attempt }` 状态：请求已登记、尚无完整回复。崩溃后看到它，按 `Stopped { cause: ProcessExit }` 记录。
- 重试只看原因与预算：`cause = ProviderError { retryable: true }` 且该步骤尝试预算未尽时重试。已收到多少流内容不影响重试资格；流式阶段没有副作用，工具只在完整回复后执行，重发半截回复只花费 token 与时间。部分文本只作有界诊断。
- `Deadline`（Turn 墙钟上限）与 `OutputLimit`（单次回复字节上限）沿用现有门禁，只作为原因记录；数值与粒度由 #222 另行裁决。
- `InvalidBatch` 维持 R1 的整批拒绝、不重试；是否反馈模型纠正归 #220。

Step 以 `Stop` 结束时，`cause` 是实际导致停止的原因：某次尝试不再重试时取该尝试的原因；在尝试之前或重试退避中收到取消或 Deadline 时取 Cancelled 或 Deadline，不沿用先前尝试的原因；有完整回复、执行调用期间停止时按第 8 节的规则推导，槽位自身的停止（LANCET 返回的取消或效果不确定）优先，否则取关闭 Step 的外部停止（Cancelled、Deadline 或 ProcessExit）；达到模型步骤上限时为步骤上限。

```text
TurnOutcome = Completed | Failed { reason } | Cancelled
```

- `Finish` → Completed；`Stop { cause: Cancelled }` → Cancelled；其他 `Stop` → Failed。
- 取消 ADR 0005 的 `Interrupted`。原先区分"provider 或模型出错"与"系统停止执行"，但现有消费方（TUI、CLI）对两者没有不同处理；原因信息保留在 `reason` 中。
- `Cancelled` 保留：它来自用户意图，呈现等级与失败不同。

### 7. 调用槽位

槽位保存 `ToolResultOutcome` 的一种，以及按 outcome 确定形状的载荷。与第 6 节的模型尝试一样分“结束形状 × 原因”两维：outcome 只说明 LANCET 给出了什么，原因在载荷里。

| outcome | 含义 | 载荷 |
| --- | --- | --- |
| Succeeded | LANCET 返回 JSON | 完整 JSON value |
| Failed | LANCET 返回 `CallCancelled`、`OutcomeIndeterminate` 以外的 CepError；策略或校验拒绝是其中的错误族（Validation、Permission、Guard） | 结构化 CepError，错误码与可重试性原样保存 |
| NotExecuted | 从未交给 LANCET；唯一保证没有外部效果的结局 | 无；为什么停看 Step 的 `end.cause` |
| Indeterminate | 已越过 Started，效果未知 | `IndeterminateCause`：`NoResult`（Step 停止或进程退出时尚未取得结果，由关闭事务补写）、`Cancelled`（LANCET 返回 `CallCancelled`）、`Reported`（LANCET 返回 `OutcomeIndeterminate`） |

- 发给模型的结果文本不存储，由 provider 投影从载荷派生；投影是纯函数，不在请求时扫描、补齐缺失结果。
- Succeeded 与 Failed 继续下一调用；NotExecuted 与 Indeterminate 停止后续分发。Failed 中拒绝类错误之后是否继续请求模型归 #220。
- 取消、停止或自动关闭时，关闭事务为每个未完成调用写入终态，不伪造成功输出：已 Started 无结果的为 `Indeterminate(NoResult)`，未 Started 的为 NotExecuted。这两种终态只由关闭事务写入，与 Step、Turn 关闭同事务。给模型的文字先用按终态固定的最小文案，#220 定下统一反馈后对齐。
- result Cell 形态不在本阶段冻结。库中的值带格式版本，#117 定下后作为新增字段加入 Succeeded 载荷。
- InvocationId 在第 5 节第 3 步的同一事务中插入全局索引。键冲突时放弃该事务、重新生成后重试；预留与步骤写入在一个事务里，不再需要跨库的候选修订流程。
- "执行前先持久记录 Started，崩溃后无结果即 Indeterminate，不重新执行"保持不变。外部效果与记录无法同时完成，这条规则与记录粒度无关。

为什么这样设计：

- **与模型尝试一致的两维结构。** `AttemptEnd` 只有 `Completed` 与 `Stopped` 两种形状，原因在 `cause` 里；槽位同样用四种形状回答“LANCET 给出了什么”，原因放在 CepError 或 `IndeterminateCause` 里，不按原因拆出新的结局。读者只看 outcome 就能判断效果保证和批次是否继续。
- **停止原因只记在 Step 一处。** 嵌套关系是 Turn → Step → 槽位，原因只从下往上传：槽位自身的停止上升为 Step 的 `end.cause`，外部停止直接记在 Step 上，不往下抄到未执行的槽位。原先每个 NotExecuted 槽位携带的原因只是 Step 原因的副本，还要额外校验两者一致。
- **取消只剩一种写法。** 分发前的取消是 Step 的 Cancelled，余下槽位为 NotExecuted；分发后 LANCET 返回的取消是该槽位的 `Indeterminate(Cancelled)`。分发后的取消不能说明效果没有发生，属于效果未知，所以不再单独设 Cancelled 结局。
- **拒绝不再冒充“执行前被拒”。** 独立的 Rejected 结局容易被读成未执行，但拒绝可能发生在 Component 执行途中，此前的宿主操作可能已生效。拒绝归入 Failed，由错误码及其错误族区分；唯一承诺“没有外部效果”的是 NotExecuted。

### 8. 取消、中断与崩溃后自动关闭

- 取消或失败后，Session 当前位置停在该 Turn 最后写入的节点，已完成的步骤进入之后的上下文。用户取消通常是为了换种说法继续，已完成的工具结果仍有用；需要丢弃时 rewind 到该用户消息之前。
- 崩溃遗留的进行中 Turn 仍按 closure-only 规则由后台在该 Session 下一次写操作前自动关闭，不请求 provider、不执行 Tool：ActiveStep 处于 `Requesting` 时追加 `Stopped { ProcessExit }`；处于执行时按第 7 节写入各槽位终态并写 Step 节点；随后以 `Stop` 关闭 Step 与 Turn。自动关闭所需事实全部在同一库中，由一个事务完成。
- 执行中关闭的 `cause` 与 live 执行、读取校验使用同一规则：槽位自身决定的停止优先（`Indeterminate(Cancelled)` 得到 Cancelled，`Indeterminate(Reported)` 得到 `InvocationIndeterminate`），否则取关闭 Step 的外部停止（live 在两次分发之间发现的 Cancelled 或 Deadline，或自动关闭的 ProcessExit）；完整规则见[会话合同](../architecture/session.md#执行中-step-的停止原因)。live 执行决定停止分发时，补写的终态与 Step、Turn 关闭同事务写入，所以已写入的取消或 Deadline 在崩溃后仍得到同一原因；崩溃落在某个槽位 Started 之前或之后，Step 原因都是 ProcessExit，崩溃时机不改变结局。

### 9. 对外结局呈现

Turn 记录保存面向用户的说明：

```text
notice = Option<{ level, text }>
```

- 写入时按 outcome 定级：Failed → Error，Cancelled → Info，Completed 无 notice。
- `text` 由最后一个 Step 的 `Stop` 原因决定：`ProviderError` 取产生该原因的那次尝试的诊断，即 provider 错误原文，先经现有脱敏；其他原因使用按原因固定、由代码定义的文案。所有 `text` 都不超过 `MAX_PROVIDER_ERROR_CHARS`（2048 字符）。已被重试取代的失败尝试不作为来源。
- 实时 `TurnFailed` 事件携带同一对象，重载前后显示一致靠构造成立。
- 公开读取投影为 `TurnSummary { outcome, notice }`；内部 `reason` 枚举不再导出给前端。

这承接 #221 在历史侧的第 1、3、4 项。实时事件的分级、去重和 `RecoveryRequired` 的呈现仍由 #221 处理。

### 10. 观察与订阅

实时观察由两部分构成：redb 中的持久快照，和后台为每个进行中 Turn 保留的内存观察状态。两者在最后一个已关闭的 Step 处衔接。

- **内存观察状态**：每个进行中的 Turn 保留两样东西，在同一把锁下更新，共用一个事件序号。
  - **事件窗口**：最近的固定条数事件，先沿用现有广播队列的 64 条。窗口大小只影响慢订阅者多久重新同步一次，不影响正确性。
  - **当前尝试的段列表**：当前模型尝试已发出的正文与 reasoning delta，按到达顺序记为 `(类型, 文字)` 段，相邻同类合并。它等于一个一直连着的订阅者按序拼出的结果。新尝试开始时清空，Step 关闭后由 redb 中的 Step 节点取代，Turn 关闭后释放。
- **订阅**：订阅者带游标读取。游标仍在窗口内时，从游标之后逐条继续。新订阅，或游标已落出窗口的慢订阅者，先得到快照（树、Turn 记录与已关闭的 Step，加上段列表及其序号），再从该序号起接窗口中的事件。读得慢只会多重新同步一次，不会丢内容。
- **内存上界**：段列表受单次尝试的正文与 reasoning 上限约束（各 2 MiB，`lattice-conversation/src/limits.rs`），每个 Turn 至多约 4 MiB 加一个窗口；同时进行的 Turn 受现有 Owner 上限（32）约束。工具结果在槽位提交时已写入 redb，从快照读取，不占观察内存。
- **快照切点**：Owner 先提交 redb 事务，再在观察锁内更新段列表并追加事件；快照在观察锁内开启 redb 读事务并复制段列表与序号。所以序号之前各事件对应的提交必定可见，读事务至多多含一次事件尚未追加的提交。段列表标注步骤与尝试序号，快照已显示该尝试 `Stopped` 或该 Step 关闭时才被替代；尝试 `Completed` 而工具仍在执行时继续有效。客户端保留已显示的临时文字，直到对应 Step 节点正文实际装入显示后原位替换；随后到达的同一关闭事件按步骤序号与节点身份幂等应用。
- **结束**：Turn 关闭事件是最后一个事件，在关闭事务提交后追加。订阅者读到它，或快照已显示 Turn 关闭时，收到结束。关闭事件只携带关闭事务已写入的内容，快照显示关闭时这些内容已在快照中，不必等待尚未追加的关闭事件。保证是：收到结束时，客户端已收到该 Turn 的 `TurnSummary` 与 Step 记录，以及从根到当前位置的对话；Turn 关闭时当前位置就是它最后写入的节点，所以观察正在执行的 Turn 时最后一个 Step 一定在内。已 `rewind` 离开的历史 Turn 不另外交付正文。
- **后台重启**：内存观察状态随进程消失，进行中的 Turn 按第 8 节自动关闭。客户端重连后用新快照重建，Turn 记录说明结局；丢失的只有当时正在生成的半截文字，它本就只作诊断。
- **删除 Gap**：原先的 Gap 来自容量 64 的广播队列会挤掉慢订阅者的事件，以及快照不含正在生成的文字、切点之前的事件不能重放。段列表补上了快照缺的部分，落出窗口只是重新同步，订阅协议只剩"快照 + 按序事件 + 结束"。客户端自己的显示队列丢弃事件时，由客户端按游标重新订阅。
- **顺序保留**：Step 节点中助手消息的 parts 按到达顺序保存正文、reasoning 与工具调用的交错。Anthropic 与 Gemini 的解析已按 block/part 顺序生成；OpenAI 兼容解析器现在把正文与 reasoning 各拼成一个字符串、固定按 reasoning → 正文 → 工具调用生成 parts，改为按到达顺序生成（相邻同类合并，工具调用排在首次出现的位置）。Chat Completions 请求只有一个 `reasoning_content` 和一个 `content` 字段，`native_to_wire` 发请求时照旧合并同类内容，同一 provider 回放不受影响。不变量：当前尝试的段列表与最终 Step 的正文/reasoning parts，合并相邻同类后序列相同，Step 关闭时显示不跳变。
- **跨 provider**：段列表只用于显示，不进历史、不发给 provider。历史跨 provider 的投影规则（丢弃 reasoning、签名与 provider 调用 ID，切回原 provider 时用原始消息）沿用 ADR 0005。
- **完成确认**：已关闭的 Turn 记录可读即已持久，不再有每 Turn 的确认状态和补屏障步骤。"未知"只剩一种真实来源：提交请求是否被接受尚不清楚，由同一 `--turn-id` 重发找回处理。
  - 依据（redb 4.2.0 与 4.3.0 源码，默认独占写模式）：`Immediate` 提交先在 header 的局部副本中写入新根并落盘，最后一次 `sync_data` 成功后，才在锁内替换读者使用的共享 header；`begin_read` 只读共享 header。4.3.0 的实验性多进程模式下读者直接读文件 header，可能早于最后一次同步看到新提交；`Durability::None` 提交先可见、不保证持久。所以第 1 节的使用条件不可放宽。
  - 可见可能早于 `commit` 返回：发布共享 header 后还有释放页等后处理。此时业务数据已持久，不影响上面的结论。
  - `commit` 错误分两类（redb 4.3.0）：`TransactionPoisoned` 表示事务已回滚、库仍可开始新事务，但引起它的存储错误不因回滚而修复；其他错误表示事务要么完整生效、要么完全未生效，但可能已经持久，不能当作已回滚，库随后拒绝新的写事务、需关闭重开。本项目的策略对两类同样保守处理：写入方不宣布成功，也不宣布回滚，关闭并重开库后以读到的记录为准，其余按第 8 节处理。这是项目策略，不是 redb 对两类错误的共同保证。

这承接 #224（正常流程报告 Gap 并漏交付事件）与 #225（历史 Turn 显示完成确认未知）。TUI 在本进程内遗忘已取得确认的问题属于前端，不在本 ADR 范围内。

### 11. 验证方式

- redb 是被使用的成熟库，不做性能资格化，不设阈值。ADR 0005 的资格化针对自研的 Git 编排；真实使用出现延迟问题时再测量。
- 以 E2E 场景为主要验证手段，使用被测二进制与真实状态根。场景选择中等到困难的组合，每个场景结束产出可核验、可重复的产物。
  - 2026-09-30 修订：场景通过时不再保留产物。这些成功产物没有被任何流程读取，维护却有成本，维护者裁决删除。场景仍记录状态摘要、树形 `log`、`audit` 输出与所用参数，只在失败时写入失败 bundle。现行规则见[测试工程合同](../development/testing.md#后台与重连场景)，裁决记录见 #230。
- 自有记录层的不变量在 E2E 的崩溃场景中核验：在每个提交点 SIGKILL 后重开，检查树中无未配对节点、一个 Turn 只有最后一个 Step 的 `end` 不是 `Continue`、Turn 记录与最后一个 Step 一致、已写入的取消终态关闭为 Cancelled、有 Owner 执行的进行中 Turn 存在时不同请求的 `send` 与 `rewind` 返回 Busy。
- 观察在 E2E 场景中核验：多步 Turn 中途新建订阅、故意放慢到落出窗口的订阅者、最后事件与 Turn 关闭同时就绪，三种情况下客户端拼出的内容都与关闭后从 redb 重读的内容一致；模型流中正文与 reasoning 交错到达时（含 OpenAI 兼容 provider），实时段列表与 Step parts 合并相邻同类后一致；后台重启后重连得到自动关闭的结局。
- 必须隔离测试的部分（如表编码、值版本解析、旧库拒绝），先列出全部失败方式再写实现。

## 与 ADR 0005 的关系

| ADR 0005 章节 | 本 ADR 后的状态 |
| --- | --- |
| §1 身份与地址 | SessionId、TurnId、InvocationId 保留；raw Git OID 不再是会话地址，新增 NodeId |
| §2 项目存储寻址 | 保留，仅用于 `workspace-history.git` |
| §3 一个状态根、三个仓库 | 取代：一个全局 redb + 每项目 `workspace-history.git` |
| §4 `lattice-store` 通用持久内核 | 适用范围缩小到 `workspace-history.git` |
| §5 操作锁在仓库锁之外 | 保留 SessionExecutionLock 与工作区操作锁，工作区操作锁不再覆盖 Turn 关闭；仓库写锁的约束只适用于 `workspace-history.git` |
| §6 会话权威在 `sessions.git` | 取代：会话权威在全局 redb |
| §7 对话是纯数据合同 | 保留 |
| §8 `TurnStarted` 是接受边界 | 保留语义，边界改为 Turn 记录创建的事务 |
| §9 PendingTurn 是不可变 CAS journal | 取代：ActiveStep |
| §10 协调器拥有循环 | 保留职责分工；模型尝试事件与"收到流事件后不重试"规则被第 6 节取代 |
| §11 InvocationStore 是只追加 ledger | 取代：调用槽位与全局索引 |
| §12 ResultDraft 只是材料 | 取代：结果直接写入槽位 |
| §13 批次执行与取消按序、失败关闭 | 保留 |
| §14 重开只做关闭 | 保留，改由第 8 节的单事务自动关闭实现 |
| §15 Turn outcome 与最终 checkpoint 有类型 | 部分取代：去掉 Interrupted，结局改由最后一个 Step 派生；Turn checkpoint 删除，见第 4 节 |
| §16 WorkspaceHistory 记录关联而非终态权威 | 记录与关联规则保留；Rewind、`RewindCoordinator` 与 Turn checkpoint 取消，见第 4 节 |
| §17 操作员读取是 pinned ledger 读取 | 取代：读全局库 |
| §18 文件系统能力按位置核验 | 保留，适用于 `workspace-history.git`；redb 所在位置另按存储合同核验 |
| §19 所属与依赖图 | 按实施更新 |
| §20 分步硬切 | 历史记录，不再适用 |

ADR 0005 部分被本 ADR 取代，正文保留。ADR 0003 的“Rewind”一节被第 4 节取代，其余结论保留。

## 后果

好处：

- 一次状态变化是一个事务，跨表一致由库保证；PendingTurn journal、TurnClosing、结果 pin、anchor checkpoint、per-binding 顺序和候选修订流程随之删除。
- 进行中与已完成的 Turn 用同一份记录，重开读取的就是写入的，不再需要从 journal 重建。
- 会话操作收敛为 `rewind` 一个移动动作，会话命令从 13 个减到 6 个，用户不需要理解 OID 或分支。
- 结局来源唯一，实时与重载显示一致。

代价与风险：

- 旧会话与调用账本不可读，需要人手处理旧库。
- 删除工作区文件回退功能、Turn checkpoint 及相关代码；WorkspaceHistory 仍然记录前后内容与 Turn/调用身份，将来提供恢复时需要重新设计与验证。Turn 关闭时刻的项目全局历史位置（含其他 Session 的变更）不再保存，将来也无法为已关闭的 Turn 补回。
- 半截回复会被重试：实时观察者可能先看到一段部分文字，随后看到新尝试从头开始，前端须按尝试边界处理。
- 读取与观察模型（固定视图、分页、Gap 与重固定、完成确认、IPC DTO、TUI/CLI 历史加载）都要随存储重写，实施量主要在这里。
- 进行中 Turn 的观察状态占用后台内存：每个 Turn 至多约 4 MiB 文字加一个事件窗口，同时至多 32 个 Turn。这是 provider 解析器已有累积文字的一份拷贝，因为观察层只能拿到中立的 delta 事件。
- OpenAI 兼容 Step 的 parts 从固定顺序改为到达顺序，多段消息的 `semantic_digest` 随之变化；旧数据不迁移，没有兼容负担。
- 全部会话集中在一个库文件；文件损坏影响所有 Session。redb 提供崩溃恢复与完整性检查，本阶段不另建备份机制。

槽位结局改为第 7 节的四种形状后，与本 ADR 此前的文本相比，以下行为有变化：

- 崩溃时有已 Started 的槽位：Step 原因为 ProcessExit（原为 `InvocationIndeterminate`），后续槽位为不带原因的 NotExecuted（原为 `PrecedingInvocationIndeterminate`）。崩溃落在 Started 前后，Step 原因与 Turn 结局相同。
- Started 已提交、交给执行前发现取消或 Deadline：Step 原因是实际的 Cancelled 或 Deadline（原为 `InvocationIndeterminate`），该槽位为 `Indeterminate(NoResult)`。
- live 执行中 LANCET 返回 `CallCancelled`：该槽位为 `Indeterminate(Cancelled)`，后续槽位为 NotExecuted（原为 `BeforeDispatch`），Step 原因为 Cancelled。
- 类型上删除 `NotExecutedReason`（四个变体）、`CancelledReason`、`IndeterminateReason::MayHaveOccurred` 与 `RejectedReason`，结局删除 Rejected 与 Cancelled；证据原因 `EvidenceUnavailableReason::ProcessExit` 改名为 `NoResult`。
- 给模型的最小文案：NotExecuted 为 `{"outcome":"not_executed"}`，Indeterminate 为 `{"outcome":"indeterminate","cause":"no_result|cancelled|reported"}`；Failed 仍给有界的 code 与 message。Anthropic 的 `is_error` 与 Gemini 的 `error` 字段只用于 Failed。
- 操作员读出的原因为 `OperatorReason = Failed { code, retriability } | Indeterminate(IndeterminateCause)`，NotExecuted 与 Succeeded 的 `reason` 为 null。
- redb 格式版本不升级：本 ADR 的存储尚未发布数据。

## 否决的方案

| 方案 | 否决理由 |
| --- | --- |
| 每项目一个 redb | 后台没有项目概念；Session 只绑定项目，跨项目列表与唯一性在全局库里更直接 |
| 保留独立调用账本（Git 或项目级表） | 调用事实是协调器视角的事实，与步骤写入同事务才能去掉跨库协调 |
| 迁移或只读保留旧数据 | 没有保留义务；迁移器与双格式读取都要长期维护 |
| 保留命名分支与 `checkout` | 分支名需要额外的表与引用维护；任意节点 rewind 已覆盖回到旧位置和从别处继续 |
| 保留 `session new` 与 Session 级 agent、目标分支、标签 | agent 已归 Turn 冻结配置；目标分支与标签没有行为依赖；首次 `send` 可在接受事务中同时创建 Session |
| 保留 `session status` | 不带 ID 时与 `list` 重复；单 Session 的字段多为 Git 表示，剩余信息由 `log` 开头显示 |
| rewind 同时回退文件 | 从旧节点继续对话不等于要退回工作区，且项目全局回退会影响其他 Session |
| 保留文件 Rewind 代码但不接入 | 要继续维护和测试一段没有调用方的代码；将来需要恢复时按当时的模型重新设计 |
| 保留 Turn checkpoint | 唯一的功能读取方是已删除的文件 Rewind；保存它要求每次关闭 Turn 取工作区锁并读取另一个库，读取失败会让 Turn 无法关闭，Session 随之无法继续 |
| 只允许 Turn 级 rewind | Step 节点天然配对完整，步骤级定位没有额外成本 |
| 用独立节点表示工具结果 | 会制造有调用无结果的中间状态，需要额外补齐规则 |
| 请求模型时临时补齐缺失结果 | 每次请求都要扫描配对；持久终态已能表达同一事实 |
| 把模型尝试记在 Turn 记录上 | 尝试属于步骤，`ModelAttemptKey` 本身含步骤序号 |
| 保留 `Interrupted` 顶层结局 | 没有消费方区别对待 |
| NodeId 用 UUID 或内容哈希 | 用户无法直接输入；树内用不到全局唯一性；内容哈希在相同内容时冲突 |
| 为 redb 做性能资格化 | 性能属于被使用的库；自有代码只需验证记录层正确性 |
| 保留有损广播队列与 Gap | 正常提交流程也会报告缺口（#224）；可重放的内存日志以相近内存成本消除丢弃来源 |
| 把流式文字写入 redb | 高频写入与步骤、调用槽位的关键写入争抢唯一的写事务；不 fsync 的提交在崩溃后也不保证保留，并不比内存观察状态多保住什么 |
| 完整保留当前 Step 的全部事件 | 内容与段列表重复，且每条 delta 的结构开销可能与文字相当；窗口加段列表的上界按字节计算更易预测 |
| 正文与 reasoning 各存一个字符串 | 丢失交错顺序，重连订阅者看到的与一直在线的订阅者不同 |

## 待定事项

- #117：result Cell 的形态与版本。
- #220：调用拒绝后的继续策略与混合批次的执行方式。
- #221：实时事件的分级、去重与 `RecoveryRequired` 呈现。
- #222：Turn deadline 与输出字节上限的数值和粒度。
- 实施时核对接线：实际使用的 redb 版本、Cargo features、后端与打开模式满足第 1 节的使用条件，代码中没有 `Durability::None` 路径。源码核对只证明提交与可见的顺序，不是对磁盘、文件系统或断电持久性的资格化。
