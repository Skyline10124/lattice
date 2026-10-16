---
status: active
---

# LATTICE

LATTICE 是以会话为中心的 LLM 运行时，Agent 通过 LANCET 调用封闭目录中的结构化 Tool，操作用户的 Local Workspace。
本文件是项目唯一的领域术语表，提供简短定义、关键区别与合同入口；完整规则由对应合同维护。
术语表不批准新语义。

## 术语

**Local Workspace**:
用户的当前项目文件树，是 Agent 观察与变更的唯一文件事实来源。
见[工作区模型](../architecture/workspace.md#工作区内容身份与历史)。
_Avoid_: Lower, Turn Workspace, guest workspace, authoritative snapshot

**Policy-Visible Workspace**:
读取策略允许 Agent 观察的 Local Workspace 路径集合，是当前本地状态的过滤视图，不是副本或跨调用快照。
默认包含已跟踪及未被忽略的文件，显式包含、读取拒绝与系统排除的优先级见[路径策略](../architecture/workspace.md#路径策略)。
_Avoid_: Workspace Projection, sandbox tree, transferred workspace

**Tool**:
封闭 Agent 目录中具有类型化参数与结果的能力，不是任意程序；一次 Tool call 是 Agent 在模型步骤后选择的该能力的一次调用。
见[工具模型](../architecture/lancet.md#概念与契约来源)。
_Avoid_: command, script, plugin, executable, untyped tool string

**Tool Descriptor**:
Tool 自有的权威契约，定义名称、结构化参数与结果、能力、作用域及可观察效果。
唯一维护源是 Source Tool manifest，生成物与 provider 投影的关系见[契约来源](../architecture/lancet.md#概念与契约来源)及[生成与校验](../architecture/lancet.md#生成与校验)。
_Avoid_: registry-owned duplicate schema, adapter-specific tool definition

**Closed Tool Catalog**:
一个已构建运行时中固定且经批准的 Tool Descriptor 集合，缺失、重复或未经批准的组成均被拒绝。
默认发行包含五个内置 Tool；增加 Source Tool 须在新运行时构建前完成准入，运行中不能加载更多代码，见[封闭组成](../architecture/lancet.md#构建与封闭组成)。
_Avoid_: runtime plugin registry, arbitrary tool loading, parallel central catalog

**Source Tool**:
独立编写和维护源代码、以隔离的 WASM Component 参与受批准运行时组成的 Tool，只能通过 Tool Host 影响外界。
仓库内生成、构建与嵌入和外部 Bundle 安装设计的区别见[构建与封闭组成](../architecture/lancet.md#构建与封闭组成)。
_Avoid_: trusted native dependency, runtime-loaded plugin, user process

**Tool Bundle**:
Source Tool 的安装设计形式，包含唯一权威 Tool Descriptor 的 manifest、预编译隔离 Component，以及完整性与来源证据；安装不得执行第三方构建代码。
外部 Bundle 安装尚未实现，设计及实现边界见[Bundle 安装设计](../architecture/lancet.md#bundle-安装设计)。
_Avoid_: source dependency, native executable, unverified artifact

**Tool Host**:
Tool 接触宿主的唯一权限边界，提供执行策略并记录效果的受控工作区、网络、会话及调用控制接口，不暴露环境中的文件系统、网络、进程权限或运行时内部句柄。
见[运行时与 Host 边界](../architecture/lancet.md#运行时与-host-边界)。
_Avoid_: convenience context, raw backend handle, ambient host access

**Structured Workspace Operation**:
对 Local Workspace 的类型化观察或变更，其数据不被重新解释为 shell 语法或 Agent 编写的指令。
见[工作区组件接口](../architecture/workspace.md#组件接口)。
_Avoid_: execution backend, Upper operation, command string

**Local Revision**:
一次观察所依据的本地文件内容身份，用于核验后续变更是否仍适用于该状态。
见[工作区模型](../architecture/workspace.md#工作区内容身份与历史)及[观察接口](../architecture/workspace.md#观察接口与边界)。
_Avoid_: baseline commit, Turn Snapshot, workspace version

**Mutation Precondition**:
本地变更前必须仍成立的事实，例如预期 Local Revision 或创建目标仍不存在。
最终核验边界见[请求与安装](../architecture/workspace.md#请求与安装)。
_Avoid_: merge base, best-effort overwrite

**Atomic Local Mutation**:
获准的结构化变更，安装后立即成为 Local Workspace 状态，且通过单个目标名称不会读到半写入内容；后续 Tool、模型或 Turn 失败不自动撤销它。
此处原子可见性不等于多文件或跨库事务、持久性确认及并发隔离，见[文件变更](../architecture/workspace.md#文件变更)及[并发边界](../architecture/workspace.md#操作锁)。
_Avoid_: checkpoint, delayed writeback, sandbox promotion

**Replan Required**:
当前 Local 状态不再满足调用的 Mutation Precondition，或语义编辑需要重新规划时的结果，要求 Agent 重新观察并发起新调用。
见[失败与后续处理](../architecture/workspace.md#失败与后续处理)。
_Avoid_: automatic retry, automatic merge, replay

**System Path Exclusion**:
运行时维护且 profile 不得放宽的路径排除规则，覆盖项目 Git 管理状态、LATTICE 私有状态及项目根外路径。
见[路径策略](../architecture/workspace.md#路径策略)。
_Avoid_: configurable deny pattern, projection exclusion

**Session**:
持续对话的容器，以 SessionId 标识并绑定一个项目，保存一棵会话树和当前位置；不带 Session 的首次提交在接受事务中创建它。
见[会话、Turn 与会话树](../architecture/session.md#会话turn-与会话树)。
_Avoid_: terminal connection, background process, workspace copy

**会话树**:
Session 内由根节点 `0`、UserMessage 与 Step 节点组成的只追加单根树。根节点没有内容、不属于任何 Turn，代表空对话。新节点以当前位置为父；发给模型的上下文是根到当前位置的路径。它记录工作区交互，但不拥有 Local Workspace 内容。
见[会话、Turn 与会话树](../architecture/session.md#会话turn-与会话树)。
_Avoid_: Session History, branch, Git commit graph, workspace checkpoint lineage

**NodeId**:
会话树节点在 Session 内单调递增的 `u64` 序号，根节点为 `0`，父节点序号恒小于子节点；跨 Session 引用写作 `(SessionId, seq)`。
见[会话、Turn 与会话树](../architecture/session.md#会话turn-与会话树)。
_Avoid_: Git OID, branch name, TurnId

**Turn**:
一次已接受任务：一个 UserMessage 节点加若干 Step 节点，结局由最后一个 Step 节点得出；一个 Turn 可以有多个 Step。
见[会话、Turn 与会话树](../architecture/session.md#会话turn-与会话树)。
_Avoid_: model attempt, single tool call, terminal lifetime

**Step**:
一轮中的一个模型步骤，是会话树上的节点：在开始之前预留，记录各次 provider 请求（尝试）、完整回复、每个工具调用的槽位和结束方式。只有末端未关闭的 Step 会被改写。
见[Step 节点与事务序列](../architecture/session.md#step-节点与事务序列)。
_Avoid_: model attempt, partial reply, ToolCall without result

**progress**:
Step、尝试和调用槽位最后到达的阶段，只前进。最后一个阶段带着实际结果；停在其他阶段时，结论由阶段确定。
见[预留、progress 与结论](../architecture/session.md#预留progress-与结论)。
_Avoid_: ActiveStep, PendingTurn, completion marker

**TurnSummary**:
一轮结局的公开读取投影 `{ outcome, notice, progress }`，每次读取从最后一个 Step 节点得出，不另存。Turn 与文件变更的关联记在 Workspace History 的变更记录中。
见[关闭 Step 与 Turn](../architecture/session.md#关闭-step-与-turn)。
_Avoid_: Turn 记录, TurnCommit, commit OID

**LocalTurn**:
本客户端一次提交的具体句柄，关联原请求身份，处理接受回执、取消意图与退出交接；不是第二个持久 Turn 或后台执行所有者。会话内容由视图交付，不经它。
主定义与生命周期见[组件与接口](../architecture/session.md#组件与接口)及[客户端操作](../architecture/session.md#实现模块与公开操作)。
_Avoid_: TurnCoordinator, durable Turn, execution owner

**TurnId**:
客户端在提交前生成的请求身份。`submit` 只接受未用过的 TurnId，已被使用即返回 Conflict；接受结果不明时按它查询找回。
见[请求身份与找回](../architecture/session.md#请求身份与找回)。
_Avoid_: NodeId, provider call ID, InvocationId

**InvocationId**:
一次工具调用的持久身份，贯穿槽位登记、执行、取消、结果和证据；provider Tool Call ID 只属模型协议材料。
见[登记与持久边界](../architecture/lancet.md#登记与持久边界)。
_Avoid_: provider call ID, tool name, PlanKey

**调用槽位**:
模型回复中一个 ToolCall 在 Step 节点里的持久记录。登记事务为回复中的每个 ToolCall 建立一个槽位并分配 InvocationId，顺序与回复中的片段顺序一致。它先为该调用将来的 ToolResult 占住位置，故称槽位；之后依次推进到 `Started` 与 `Terminal`，到达 `Terminal` 时写入结果、载荷与 Cell 证据。每个阶段都有确定的结论，所以树上不会出现有调用而无结果的节点。槽位归 `lattice-session`，随 Step 节点在同一个会话库事务里写入。
见[调用槽位](../architecture/session.md#调用槽位)。
_Avoid_: invocation ledger, `invocations.git`, BindingPlan, provider Tool Call ID as slot identity

**持久对话内容**:
已写入会话树的 UserMessage 与 Step 节点，作为对话条目交付，也供后续模型请求使用。
它与尚未保存的临时流式文字分开，见[用户读到的几种事实](../architecture/session.md#用户读到的几种事实)。
_Avoid_: token replay, layout body width, frozen configuration

**视图**:
后台按会话推送的“这个会话现在的样子”：会话头（当前位置、当前 agent、当前位置所属那一轮有没有 Owner 及执行诊断）、当前路径上最近几轮的对话条目、之后变化的条目与生成中的文字。观察者变慢时只少收中间版本，不需要重新同步。
见[观察与重连](../architecture/session.md#观察与重连)。
_Avoid_: snapshot, event window, subscription cursor, Gap

**对话条目**:
视图与历史查询交付的会话内容单位，一个条目对应一个节点，以 NodeId 标识，同一节点的新值整条替换旧值。UserMessage 条目带轮次序号，Step 条目带结论与重试提示，本轮最后一步的条目另带 `TurnSummary`。
见[观察与重连](../architecture/session.md#观察与重连)。
_Avoid_: TurnEvent, HistoryEntry, per-call item

**Workspace History**:
LATTICE 已成功安装的 Atomic Local Mutation 的私有持久记录，使用 Git tree 表达前后状态，仅覆盖 LATTICE 变更过的路径；每条变更记录带 `TurnId` 与 `InvocationId`。
它位于 Local Workspace 和用户项目 Git 之外的私有 bare Git 库，见[私有存储与数据关系](../architecture/workspace.md#私有存储与数据关系)。
_Avoid_: whole-project snapshot, execution workspace, projection, checkpoint,
receipt store

**Rewind**:
把 Session 当前位置移到会话树上任意已有节点本身；之后的提交以该节点为父分叉，原分支保留。改写一条用户消息即退回到它的父节点再提交（第一条消息为 `rewind 0`）。它只切换对话位置，不恢复工作区文件。
见[实现模块与公开操作](../architecture/session.md#实现模块与公开操作)。
_Avoid_: branch, checkout, reset, file restore, workspace rollback
