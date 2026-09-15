---
status: active
---

# LATTICE

LATTICE 是以会话为中心的 LLM 运行时，Agent 通过 LANCET 调用封闭目录中的结构化 Tool，操作用户的 Local Workspace。
本文件是项目唯一的领域术语表，提供简短定义、关键区别与合同入口；完整规则由对应合同维护。
引用待批准修订的词条按[工作分支修订状态](../README.md#工作分支修订状态)使用，生效前仍以原有效合同为准；术语表不批准新语义。

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
持续对话的容器，以 SessionId 标识并绑定一个项目，保存历史位置与会话状态。
见[会话、Turn 与历史](../architecture/session.md#会话turn-与历史)。
_Avoid_: terminal connection, background process, workspace copy

**Turn**:
一次已接受任务，包含用户输入、模型步骤、工具调用及关闭结果；一个 Turn 可以有多个模型步骤。
见[会话、Turn 与历史](../architecture/session.md#会话turn-与历史)。
_Avoid_: model attempt, single tool call, terminal lifetime

**LocalTurn**:
本客户端一次提交或观察交互的具体句柄，关联原请求身份与已收到的状态、内容和诊断；不是第二个持久 Turn 或后台执行所有者。
主定义与生命周期见[组件与接口](../architecture/session.md#组件与接口)及[客户端操作](../architecture/session.md#实现模块与公开操作)。
_Avoid_: PendingTurn, TurnCoordinator, durable Turn, execution owner

**TurnId**:
客户端为提交保留的请求身份，与 SessionId 一起用于找回原请求；TurnCommit 的对象地址另由 Git OID 表达。
请求重试见[身份与确认](../architecture/session.md#请求身份与确认)；调用批次预留键（代码 `PlanKey`）的完整身份包含 Session，跨 Session 同 TurnId 计划在项目级调用账本中不冲突，见[绑定与逐调用关闭](../architecture/session.md#绑定与逐调用关闭)。
_Avoid_: commit OID, provider call ID, InvocationId

**PendingTurn**:
已接受但尚未持久关闭的 Turn 进度，保存冻结事实、累积对话和当前关闭阶段。
见[日志与阶段](../architecture/session.md#日志与阶段)。
_Avoid_: live process, token buffer, retry queue

**TurnCommit**:
Turn 关闭后保存的不可变结果对象，包含对话、outcome 和工作区 checkpoint，以 Git OID 寻址。
见[Turn 最终发布](../architecture/session.md#turn-最终发布)。
_Avoid_: TurnId, user project commit, whole-workspace snapshot

**InvocationId**:
一次工具调用的持久身份，贯穿绑定、预留、执行、取消、结果和证据；provider Tool Call ID 只属模型协议材料。它与标识一次工具调用批次预留的 `PlanKey`（Session+Turn+步骤）分工不同：前者定位单次调用，后者是批次预留的幂等键。
见[绑定与持久边界](../architecture/lancet.md#绑定与持久边界)。
_Avoid_: provider call ID, tool name, PlanKey

**持久对话内容**:
已保存的用户消息、完整模型消息与已关闭 ToolResult，供历史读取和后续模型请求使用。
它与尚未保存的临时流式文字分开，见[用户读到的几种事实](../architecture/session.md#用户读到的几种事实)。
_Avoid_: token replay, layout body width, frozen configuration

**会话固定视图**:
后台为一次读取选定的会话历史位置与已保存进度版本，使读取期间的新发布不混入该次对话。
见[观察与重连](../architecture/session.md#观察与重连)；该后台读取语义仍属 R1 待批准修订，不表示整个工作区快照。
_Avoid_: live mutable head, whole-workspace snapshot

**观察 Gap**:
通知客户端观察事件不连续的信号，客户端据此提示缺口并重新读取固定的持久对话。
见[观察与重连](../architecture/session.md#观察与重连)，属 R1 待批准修订；观察缺失不等同于持久消息或调用终态丢失。
_Avoid_: journal corruption, tool failure, layout gap

**Session History**:
对话 Turn 与 Tool transcript 的 Git 风格图，记录工作区交互但不拥有 Local Workspace 内容。
普通会话引用移动不恢复文件，见[会话、Turn 与历史](../architecture/session.md#会话turn-与历史)。
_Avoid_: workspace checkpoint lineage, Local rollback log, retained sandbox

**Workspace History**:
LATTICE 已成功安装的 Atomic Local Mutation 的私有持久记录，使用 Git tree 表达前后状态，仅覆盖 LATTICE 变更过的路径。
它位于 Local Workspace 和用户项目 Git 之外的私有 bare Git 库，见[私有存储与数据关系](../architecture/workspace.md#私有存储与数据关系)。
_Avoid_: whole-project snapshot, execution workspace, projection, checkpoint,
receipt store

**Workspace-History Object ID**:
已记录 Workspace History 位置的 Git 对象身份；项目全局 checkpoint 的 `Empty` 分支表示权威历史引用不存在，不是已有 Git OID。
每个完成的 Turn 都在最终发布边界读取当前项目 checkpoint 并存入不可变提交，即使本 Turn 未写文件，也可包含其他 Session 已记录的变更。
见[Turn checkpoint](../architecture/workspace.md#turn-checkpoint)、[OID 保留与可解析性](../architecture/workspace.md#私有存储与数据关系)及[Turn 最终发布](../architecture/session.md#turn-最终发布)。
_Avoid_: workspace snapshot id, session commit id, checkpoint handle

**Rewind**:
用户显式选择较早的可达 Session History 点，按目标 Turn 的项目全局 checkpoint 撤销其后的 LATTICE 变更后缀，包含其他 Session 的后续变更，不按当前 Session 过滤。
它保留无关外部修改，拒绝强制覆盖已偏离记录后态的路径，会话侧只移动所选会话的引用，见[显式 Rewind](../architecture/workspace.md#显式-rewind)。
_Avoid_: whole-workspace snapshot restore, automatic overwrite, conversation-only reset

**Rewind Conflict**:
Rewind 全路径预检发现受影响路径已变化、缺失、类型改变或不可读时的结构化拒绝，列出每个分歧路径及记录与观察到的 Local Revision 或路径状态。
此时不执行本地恢复、不移动工作区历史 current 或会话引用，可追加冲突审计；执行中失败可能已产生部分效果，见[冲突、部分完成与恢复](../architecture/workspace.md#冲突部分完成与恢复)。
_Avoid_: partial rewind, per-path skip, forced overwrite
