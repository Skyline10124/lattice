---
status: active
---

# LATTICE

LATTICE 是以会话为中心的 LLM 运行时，协调层经可注入的 `ToolExecutor` 驱动工具调用槽位；当前版本生产环境默认注入空目录的 `NoTools`，以纯对话运行。
本文件是项目唯一的领域术语表，提供简短定义、关键区别与合同入口；完整规则由对应合同维护。
术语表不批准新语义。

## 术语

**Local Workspace**:
会话绑定的规范化项目根目录，用于推导 ProjectStoreKey 并作为 `ToolExecutor::open_turn` 的工作区参数；LATTICE 自身不读写其中的业务文件。
见[ProjectStoreKey](../architecture/storage.md#projectstorekey)。
_Avoid_: Lower, Turn Workspace, guest workspace, authoritative snapshot

**Tool**:
工具执行器经 `ToolSpec` 为一轮提供的类型化能力，接受事务把本轮 `ToolSpec` 列表冻结为工具目录，模型的一次 Tool call 由注入的 `ToolExecutor` 执行；生产默认注入空目录的 `NoTools`。
见[执行配置冻结](../architecture/session/acceptance.md#执行配置冻结)及[调用槽位](../architecture/session/steps.md#调用槽位)。
_Avoid_: command, script, plugin, executable, untyped tool string

**Session**:
持续对话的容器，以 SessionId 标识并绑定一个项目，保存一棵会话树和当前位置；不带 Session 的首次提交在接受事务中创建它。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: terminal connection, background process, workspace copy

**会话树**:
Session 内由根节点 `0`、UserMessage 与 Step 节点组成的只追加单根树。根节点没有内容、不属于任何 Turn，代表空对话。新节点以当前位置为父；发给模型的上下文是根到当前位置的路径。它记录对话与工具调用历史，不拥有 Local Workspace 内容。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: Session History, branch, Git commit graph, workspace checkpoint lineage

**NodeId**:
会话树节点在 Session 内单调递增的 `u64` 序号，根节点为 `0`，父节点序号恒小于子节点；跨 Session 引用写作 `(SessionId, seq)`。
见[会话、Turn 与会话树](../architecture/session/README.md#会话turn-与会话树)。
_Avoid_: Git OID, branch name, TurnId

**Turn**:
一次已接受任务：一个 UserMessage 节点加若干 Step 节点，结局由最后一个 Step 节点得出；一个 Turn 可以有多个 Step。
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
客户端在提交前生成的请求身份。`submit` 只接受未用过的 TurnId，已被使用即返回 Conflict；接受结果不明时按它查询找回。
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
已写入会话树的 UserMessage 与 Step 节点，作为对话条目交付，也供后续模型请求使用。
它与尚未保存的临时流式文字分开，见[用户读到的几种事实](../architecture/session/flow.md#用户读到的几种事实)。
_Avoid_: token replay, layout body width, frozen configuration

**视图**:
后台按会话推送的“这个会话现在的样子”：会话头（当前位置、当前 agent、profile 副本中的模型与推理档位、当前位置所属那一轮有没有 Owner 及执行诊断）、当前路径上最近几轮的对话条目、之后变化的条目与生成中的文字。观察者变慢时只少收中间版本，不需要重新同步。
见[观察与重连](../architecture/session/recovery.md#观察与重连)。
_Avoid_: snapshot, event window, subscription cursor, Gap

**对话条目**:
视图与历史查询交付的会话内容单位，一个条目对应一个节点，以 NodeId 标识，同一节点的新值整条替换旧值。UserMessage 条目带轮次序号，Step 条目带结论与重试提示，本轮最后一步的条目另带 `TurnSummary`。
见[观察与重连](../architecture/session/recovery.md#观察与重连)。
_Avoid_: TurnEvent, HistoryEntry, per-call item

**Rewind**:
把 Session 当前位置移到会话树上任意已有节点本身；之后的提交以该节点为父分叉，原分支保留。改写一条用户消息即退回到它的父节点再提交（第一条消息为 `rewind 0`）。它只切换对话位置，不恢复工作区文件。
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

**profile 副本**:
会话持有的一份有效 profile，以及当前的 provider `id`、模型 `id` 与推理档位，新会话首次被接受时写入；之后的提交只用副本，原 profile 的修改只对新会话生效。provider 配置与凭据不在副本中。
见[会话的 profile 副本](../architecture/session/settings.md#会话的-profile-副本)。
_Avoid_: frozen configuration, current agent lookup, session config cache
