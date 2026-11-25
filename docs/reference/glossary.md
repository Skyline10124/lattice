---
status: active
---

# LATTICE

LATTICE 是以会话为中心的 LLM 运行时，协调层经可注入的 `ToolExecutor` 驱动工具调用槽位；当前版本生产环境默认注入空目录的 `NoTools`，以纯对话运行。
本文件是项目唯一的领域术语表，提供简短定义、关键区别与合同入口；完整规则由对应合同维护。
术语表不批准新语义。

## 术语

**Local Workspace**:
会话绑定的规范化项目根目录，其规范路径字节保存在 Session 记录中，并作为 `ToolExecutor::open_turn` 的工作区参数；LATTICE 自身不读写其中的业务文件。
见[项目绑定](../architecture/storage.md#项目绑定)。
_Avoid_: Lower, Turn Workspace, guest workspace, authoritative snapshot

**Tool**:
工具执行器经 `ToolSpec` 为一轮提供的类型化能力，接受事务把本轮 `ToolSpec` 列表冻结为工具目录，模型的一次 Tool call 由注入的 `ToolExecutor` 执行；生产默认注入空目录的 `NoTools`。
见[执行配置冻结](../architecture/session/acceptance.md#执行配置冻结)及[调用槽位](../architecture/session/steps.md#调用槽位)。
_Avoid_: command, script, plugin, executable, untyped tool string

**Session**:
持续对话的容器，以 SessionId 标识并绑定一个项目，保存一棵会话树、当前位置与会话设置；由 `configure(New)` 显式创建为草稿会话，接受第一轮后成为已使用会话。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: terminal connection, background process, workspace copy

**草稿会话**:
英文名 Draft session。由 `configure(New)` 创建、整棵会话树只有根节点 `0` 的会话；照常出现在 `list` 中并可被观察，接受第一轮后成为已使用会话，逾期且无观察者时由后台整体删除。
见[会话状态与草稿清理](../architecture/session/concurrency.md#会话状态与草稿清理)。
_Avoid_: 评论草稿（TUI）, 空会话, 临时会话

**会话树**:
Session 内由根节点 `0`、`Input`、`ContextOp` 与 `Step` 节点组成的只追加单根树。根节点没有内容、不属于任何 Turn，代表空对话。新节点以当前位置为父；发给模型的上下文是根到当前位置的路径经 `lattice-context` 归约与投影后的结果。它记录对话、上下文操作与工具调用历史，不拥有 Local Workspace 内容。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: Session History, branch, Git commit graph, workspace checkpoint lineage

**NodeId**:
会话树节点在 Session 内单调递增的 `u64` 序号，根节点为 `0`，父节点序号恒小于子节点；跨 Session 引用写作 `(SessionId, seq)`。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: Git OID, branch name, TurnId

**Turn**:
一次已接受任务：以携带 `turn_start` 元数据的 `Step 0` 为起点、后接若干后续 `Step` 节点（普通提交在同一事务中先追加本轮 `Input` 节点并在 `turn_start.input_nodes` 认领），结局由最后一个 Step 节点得出；一个 Turn 可以有多个 Step。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: model attempt, single tool call, terminal lifetime

**Step**:
一轮中的一个模型步骤，是会话树上的节点：在开始之前预留，记录各次 provider 请求（尝试）、完整回复、每个工具调用的槽位和结束方式。只有末端未关闭的 Step 会被改写。
见[Step 节点与事务序列](../architecture/session/steps.md#step-节点与事务序列)。
_Avoid_: model attempt, partial reply, ToolCall without result

**progress**:
Step、尝试和调用槽位最后到达的阶段，只前进。最后一个阶段带着实际结果；停在其他阶段时，结论由阶段确定。
见[预留、progress 与结论](../architecture/session/README.md#预留progress-与结论)。
_Avoid_: ActiveStep, PendingTurn, completion marker

**TurnSummary**:
一轮结局的公开读取投影 `{ outcome, notice, progress }`，每次读取从最后一个 Step 节点得出，不另存。
见[关闭 Step 与 Turn](../architecture/session/steps.md#关闭-step-与-turn)。
_Avoid_: Turn 记录, TurnCommit, commit OID

**LocalTurn**:
本客户端一次提交的具体句柄，关联原请求身份，处理接受回执、取消意图与退出交接；不是第二个持久 Turn 或后台执行所有者。会话内容由视图交付，不经它。
主定义与生命周期见[组件与接口](../architecture/session/README.md#组件与接口)及[客户端操作](../architecture/session/operations.md#实现模块与公开操作)。
_Avoid_: TurnCoordinator, durable Turn, execution owner

**TurnId**:
客户端在提交或独立触发前生成的请求身份。`submit` 与 `trigger` 只接受未用过的 TurnId，已被使用即返回 Conflict；接受结果不明时按它查询找回。
见[请求身份与找回](../architecture/session/recovery.md#请求身份与找回)。
_Avoid_: NodeId, provider call ID, InvocationId

**InvocationId**:
一次工具调用的持久身份，贯穿槽位登记、执行、取消与结果保存；provider Tool Call ID 只属模型协议材料。
见[调用槽位](../architecture/session/steps.md#调用槽位)。
_Avoid_: provider call ID, tool name, PlanKey

**调用槽位**:
模型回复中一个 ToolCall 在 Step 节点里的持久记录。登记事务为回复中的每个 ToolCall 建立一个槽位、分配 InvocationId 并记录不透明工具身份 `tool_identity` 与规范参数摘要，顺序与回复中的片段顺序一致。它先为该调用将来的 ToolResult 占住位置，故称槽位；之后推进到 `Started`，执行器返回非 `Deadline` 结果时再推进到 `Terminal` 并写入结局与载荷（返回 `Deadline` 时保持 `Started` 并按 `Indeterminate(NoResult)` 读出）。每个阶段都有确定的结论，所以树上不会出现有调用而无结果的节点。槽位归 `lattice-session`，随 Step 节点在同一个会话库事务里写入。
见[调用槽位](../architecture/session/steps.md#调用槽位)。
_Avoid_: invocation ledger, `invocations.git`, BindingPlan, provider Tool Call ID as slot identity

**ToolExecutor**:
协调层注入的工具执行接缝：接受事务前按会话与工作区打开一次本轮 `TurnTools`，给出冻结进配置表的 `ToolSpec` 列表（名称、描述、规范输入 schema 与不透明 `tool_identity`），并在调用槽位提交为 `Started` 后分发单次执行。协调层不感知底层运行时或策略细节。
见[执行配置冻结](../architecture/session/acceptance.md#执行配置冻结)及[实现模块与公开操作](../architecture/session/operations.md#实现模块与公开操作)。
_Avoid_: LancetRuntime, global mutable tool registry, ambient host executor

**ToolOutcome**:
`TurnTools::execute` 对单次已 `Started` 调用返回的执行结果：`Succeeded(Value)`、`Failed(ToolError)`、`Cancelled`、`Deadline` 或 `Indeterminate`。除 `Deadline` 直接关闭当前 Step 并令槽位保留 `Started`（读作 `Indeterminate(NoResult)`）外，其余变体由协调层映射为槽位的 `Terminal` 结局与载荷。
见[调用槽位](../architecture/session/steps.md#调用槽位)。
_Avoid_: raw Result string, CepError, untyped exit status

**ToolError**:
工具调用失败时持久保存并投影给模型的结构化载荷 `{ code, message }`，其中 `code` 为机器错误码字符串，`message` 为面向模型与诊断的说明文本；provider 投影将其编码为 `{"error":{"code":...,"message":...}}`。
见[对话与投影边界](../architecture/session/conversation.md#对话与投影边界)及[调用槽位](../architecture/session/steps.md#调用槽位)。
_Avoid_: CepError, unstructured error string, Cell evidence error

**NoTools**:
生产后台默认注入的 `ToolExecutor` 空实现：返回空的 `ToolSpec` 列表，使本轮冻结目录为空、发给模型的请求不带工具定义，所有会话按纯对话运行。
见[执行配置冻结](../architecture/session/acceptance.md#执行配置冻结)。
_Avoid_: disabled tool flag, runtime plugin fallback

**测试假执行器**:
仅在显式开启 `test-fake-tools` Cargo feature 且后台启动环境设置 `LATTICE_TEST_TOOLS`（逗号分隔工具名）时启用的确定性 `FakeToolExecutor`；未开启 feature 或未设置该环境变量时仍使用 `NoTools`。它按调用参数中的动作控制成功、超限输出、结构化失败、阻塞取消/超时与不确定结局，专供测试验证协调层多步工具循环。
见[会话与 Turn](../architecture/session/README.md#目的与范围)及[验证集与显式 feature](../development/testing.md#验证集与显式-feature)。
_Avoid_: production fallback tool, mock LLM server

**持久对话内容**:
已写入会话树的 `Input`、`ContextOp` 与 `Step` 节点，作为原始对话条目交付，也在发往模型前经有效上下文归约与投影。
它与尚未保存的临时流式文字分开，见[用户读到的几种事实](../architecture/session/flow.md#用户读到的几种事实)。
_Avoid_: token replay, layout body width, frozen configuration

**图片引用**:
英文名 Image reference（`ImageRef`）。用户消息图片片段中与存储解耦的内容寻址元数据，由原始字节的 SHA-256 摘要、前 12 字节魔数判定的媒体类型（PNG、JPEG、WebP、GIF）与原始字节长度组成；会话树节点与视图推送只携带它，已接受图片的原始字节独立存入会话库 `blobs` 表。
见[用户图片与单轮输入规范化](../architecture/session/conversation.md#用户图片与单轮输入规范化)。
_Avoid_: 内联 base64 节点, 路径引用, 扩展名格式

**视图**:
后台按会话推送的“这个会话现在的样子”：会话头（当前 agent、会话设置中的模型与推理档位、设置版本、当前位置及其所属那一轮有没有 Owner、执行诊断等，完整字段清单见[实现模块与公开操作](../architecture/session/operations.md#实现模块与公开操作)）、当前路径上最近几轮的对话条目、之后变化的条目与生成中的文字。观察者变慢时只少收中间版本，不需要重新同步。
见[观察与重连](../architecture/session/recovery.md#观察与重连)。
_Avoid_: snapshot, event window, subscription cursor, Gap

**对话条目**:
视图与历史查询交付的扁平会话内容单位（`ConversationEntry`），一个条目对应一个非根节点（`Input`、`ContextOp` 或 `Step`），以 NodeId 标识，同一节点的新值整条替换旧值。`Step` 条目带步骤详情、完整回复投影与重试提示，仅 `Step 0` 顶层携带 `agent_name`（不再携带路径轮次序号或物理 `turn_start` 元数据），本轮最后一步的条目另带 `TurnSummary`。
见[观察与重连](../architecture/session/recovery.md#观察与重连)。
_Avoid_: TurnEvent, HistoryEntry, per-call item

**Rewind**:
把 Session 当前位置移到会话树上任意已有节点本身；之后的提交以该节点为父分叉，原分支保留。改写一条用户消息即退回到它的父节点再提交（第一条消息为 `rewind 0`）。它只切换对话位置，不修改会话设置，也不恢复工作区文件。
见[实现模块与公开操作](../architecture/session/operations.md#实现模块与公开操作)。
_Avoid_: branch, checkout, reset, file restore, workspace rollback

**provider**:
一个可以接收模型请求的服务，由配置描述：标识、协议、endpoint、认证方式、features 与元数据条目。来自内置表与用户全局的 `providers.toml`；官方厂商、兼容厂商与本地服务用同一种描述，代码中没有厂商类型。
见[provider 配置](../architecture/llm/providers.md#provider-配置)。
_Avoid_: vendor type, alias, router entry, URL-detected vendor

**feature**:
某个协议适配器支持的一项可选行为，表达厂商对协议的改动，取值是布尔量或有限枚举，默认值等于标准协议行为。每个协议的 features 是封闭集合，不能用它注入任意请求字段；与构建用的 Cargo feature 无关。
见[features](../architecture/llm/protocols.md#features)。
_Avoid_: compat flag, extra body, request patch

**请求网关**:
`lattice-llm` 对外唯一的模型请求入口：调用方给出 provider `id`、模型 `id`、推理档位与请求内容，得到流式事件、用量与费用估算；provider 解析、凭据、协议适配与传输都在它内部，每次请求按当前配置解析。
见[组成与归属](../architecture/llm/README.md#组成与归属)。
_Avoid_: ModelRouter, connection material, per-Turn LlmConnection

**推理档位**:
`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max` 七档之一，表示一轮使用的推理强度；适配器按协议与 features 换成请求参数，模型不支持的档位不可选。
见[推理档位](../architecture/llm/reasoning.md#推理档位)。
_Avoid_: thinking budget, reasoning mode, effort string

**输入模态**:
英文名 Input modality（`InputModality`）。模型可接收的用户输入内容类别（`text`、`image`、`audio`、`video`），由模型目录的 `modalities.input` 得出；`providers.toml` 中手写的 `input` 存在时，整表覆盖目录的列表。目标模型声明了输入模态且不含 `image` 时，含图片的请求在发送前被拒绝。
见[元数据的来源与合并](../architecture/llm/models.md#元数据的来源与合并)。
_Avoid_: voice 模态, 布尔图片开关, 输出模态

**会话设置**:
英文名 Session settings。会话保存在会话库中的下一轮配置记录（`SessionSettings`），含有效 profile、内联系统提示、可选的模型与可选推理档位，内容摘要构成设置版本（`SettingsVersion`）；由 `configure` 写入或更新，接受一轮时从中冻结本轮配置。provider 配置与凭据不在会话设置中。
见[会话设置](../architecture/session/settings.md#会话设置)。
_Avoid_: frozen configuration, current agent lookup, session config cache

**IM 网关**:
本地后台进程内把外部即时通讯平台（当前为 Telegram 私聊）接入 LATTICE 会话服务的组件，负责授权过滤、命令分发、流式草稿与文本切分回复；它面向外部消息平台收发用户对话，不同于向模型服务发起调用的请求网关。
见[IM 网关](../architecture/im.md#组件与职责)。
_Avoid_: 请求网关, 独立桥接进程, webhook 服务, 远程后台入口

**台账**:
英文名 Ledger。`lattice-im` 在 `<state-root>/lattice/im/ledger` 维护的私有状态文件，只保存外部对话与 `SessionId` 的当前绑定及重投窗口内入站事件的相位（`Registered`、`Accepted`、`Merged`、`DeliveredTurn`、`DeliveredReset`）；对话历史与图片内容保存在会话库中，它不保存消息正文、图片字节或模型与工具调用记录。
见[IM 网关](../architecture/im.md#状态)。
_Avoid_: 会话树, invocation ledger, `invocations.git`, 消息历史库
