---
status: active
---

# 会话与 Turn

> 类型：架构合同
> 上位依据：[项目宪章](../../governance/charter.md)、[宿主平台](../platform.md)
> 决策记录：[ADR 0006](../../adr/0006-tree-session-and-global-redb-store.md)、[ADR 0007](../../adr/0007-session-tree-as-single-authority.md)、[ADR 0008](../../adr/0008-backend-api-commands-view-queries.md)、[ADR 0010](../../adr/0010-protocol-adapters-and-configured-providers.md)、[ADR 0012](../../adr/0012-backend-owns-session-settings.md)

## 目的与范围

用户向一个会话提交任务，后台接受后驱动模型、执行模型选择的 Tool、保存调用结果，再把结果交给模型继续回答。用户可以同时观察进度，也可以关闭终端，稍后重新连接查询状态和读取已保存的对话；需要从较早的位置换一种说法继续时，把会话退回树上的某个节点再发送。

本合同说明会话如何创建与持有设置，一次任务如何被接受、执行和保存，会话历史如何组织成树，以及用户中途离开、取消任务或后台重启后如何继续处理。模型请求怎样发出、发到哪里、带什么凭据，由[模型接入合同](../llm/README.md)维护。

以下以生产默认的纯对话流程为贯穿场景：用户提出问题，协调层经默认的 `NoTools` 冻结空工具目录、不向模型暴露工具，模型直接给出完整回答并关闭这一轮。测试构建开启 `test-fake-tools` 并设置 `LATTICE_TEST_TOOLS` 时注入假工具执行器，驱动登记、执行与续跑的多步工具循环，槽位推进与结果保存见[调用槽位](steps.md#调用槽位)。

## 模型与职责

### 会话、Turn 与会话树

Session（会话）保存用户的一段持续对话，以 `SessionId` 标识，永久绑定用户工作的项目（`ProjectPath`）。新会话由客户端生成 `SessionId` 并经 `configure(New)` 显式创建为[草稿会话](concurrency.md#会话状态与草稿清理)。Session 记录只保存身份、项目绑定、当前位置与时间；会话下一轮将使用的配置保存在另一条记录中，即会话设置，见[会话设置](settings.md)。

后台接受一次用户任务后，形成一个 Turn：纯对话场景包含用户问题与模型的完整回答；启用工具的多步循环中，中间的工具调用步骤与最终回答也属于同一 Turn。客户端在向已有会话提交前生成 `TurnId`（UUID v4）；接受结果不明时，按 TurnId 查询找回这一次请求。

会话历史是一棵树。一个会话里发生过什么，以树上的节点为唯一权威：后台为这个会话提交的每一件事都写在节点上，其他表是节点的一部分、指针，或从节点得出的索引。节点有两种：

- **UserMessage**：一条用户输入。一个 Turn 从它的 UserMessage 开始。接受这一轮时确定的事实也记在这个节点上：`turn_id`、接受时间、agent 名、模型、provider、推理档位和执行限制。体积大的系统提示与工具目录放在配置表，节点只记它们的内容摘要，见[表、读取范围与 rewind](#表读取范围与-rewind)。
- **Step**：这一轮的一个模型步骤。它记录这一步的各次模型请求（称为尝试）、得到的模型回复、回复发出的各个工具调用及其结果（每个调用一个槽位）、这一步怎么结束，以及最后一次推进的时间。

每个 Session 建立时写入根节点 `0`：它没有内容、不属于任何 Turn，代表空对话，树始终只有这一个根。其他节点记录父节点和所属 `turn_id`，Step 节点另记它是这一轮的第几步。节点身份 NodeId 是每个 Session 内单调递增的 `u64`，写入节点时分配，第一条 UserMessage 为 `1`，父节点序号恒小于子节点；跨 Session 引用写作 `(SessionId, seq)`，用户可以直接输入序号。**当前位置**是 Session 记录中的一个 NodeId：新节点以当前位置为父，写入后当前位置前移到新节点。

父子关系只表示先后：子节点接在父节点之后。节点属于哪个 Turn、是第几步，由节点上的 `turn_id` 和步骤序号表达，不由树形表达。所以一个 Turn 在树上是相连的一段：它的 UserMessage，后面依次是它的各个 Step，节点序号连续。树上的分叉只由 `rewind` 产生，表示同一位置之后的另一种后续。

消息内容由有序片段组成：文字、模型推理内容（reasoning）、工具调用请求（ToolCall）或调用结果（ToolResult）。ToolCall 说明要使用哪个工具和参数；ToolResult 记录执行结果与返回内容。一次工具调用以 `InvocationId` 标识，一个 Step 可以包含多次调用，一个 Turn 可以包含多个 Step。

#### 预留、progress 与结论

Step 节点在这一步开始之前预留。接受一轮的事务写入 UserMessage，同时预留第 0 步；一个 Step 决定继续请求模型时，关闭它的事务同时预留下一步。预留的 Step 没有尝试，也没有回复。Turn 以完成或停止结束时不预留下一步。

Step、尝试和调用槽位都会经历几个阶段。节点为它们各保存一项 `progress`，即最后到达的阶段：它到达时写入，只前进，此后不因关闭、取消或进程退出而改动。最后一个阶段带着实际结果；停在其他阶段时，结论由阶段本身确定，不另设字段：

| 对象 | `progress` | 停在这里时的结论 |
| --- | --- | --- |
| Step | `Reserved`、`Requesting` | 停止，原因为进程退出 |
| Step | `Executing` | 停止，原因由各槽位的结果推出，推不出时为进程退出 |
| Step | `Closed { end }` | `end`：继续、完成，或停止及实际原因 |
| 尝试 | `Sent` | 停止，原因为进程退出 |
| 尝试 | `Ended { end, diagnostic }` | `end`：完成，或停止及实际原因 |
| 调用槽位 | `Registered` | NotExecuted |
| 调用槽位 | `Started` | Indeterminate，没有取得结果 |
| 调用槽位 | `Terminal { result }` | `result`：工具执行器给出的结局与载荷 |

这张表是结论的唯一来源。会话库读取时按它给出每个对象的结论，所有读者经同一处取得。所以树上没有空缺：任何时刻，每个调用槽位都有结果，每次尝试都有终态，每个 Step 都有结束方式。执行者在任何时刻消失，树上已有的内容就是这一轮的结局，不需要事后补写。各阶段的事务和推导规则见 [Step 执行与结果保存](steps.md#step-执行与结果保存)。

树只追加节点。已使用会话中已写入的节点里只有一个可以改写：Session 中序号最大的节点，且它是尚未到达 `Closed` 的 Step。改写由执行这一轮的协调层在 `progress` 前进时提交。UserMessage、已关闭的 Step，以及之后已经写入新节点的 Step 保持不变。

`progress` 没有到达 `Closed` 的 Step，可能仍在执行，也可能执行者已经消失、它就停在那一步。两种情况在树上没有区别，区别在于后台是否仍持有这一轮的 Owner，见[执行者消失后的 Turn](recovery.md#执行者消失后的-turn)。不是 Session 最后接受的那一轮，一定不在执行。

Turn 没有单独保存的结局，也没有单独的结束节点。它的结局由最后一个 Step 的结论决定；最后一个 Step 以完成或停止到达 `Closed` 时，这一轮关闭。对外的读取投影见[关闭 Step 与 Turn](steps.md#关闭-step-与-turn)。

#### 表、读取范围与 rewind

会话库中的记录：

| 记录 | 键 | 内容 | 写入与修改 |
| --- | --- | --- | --- |
| 节点 | `(SessionId, seq)` | 根节点、UserMessage 与 Step，如上 | 追加；已使用会话中只有末端未关闭的 Step 可改写，草稿会话逾期后可整体删除 |
| 配置 | 内容摘要 | 一轮所用的系统提示与工具目录，由 UserMessage 节点按摘要引用；内容相同的只存一份 | 接受时尚无同一摘要才写入，写入后保持不变 |
| TurnId 索引 | `TurnId` | 这一轮 UserMessage 节点的位置 `(SessionId, seq)`，用于按 TurnId 找回请求、取消和查看详情 | 与 UserMessage 节点同事务写入 |
| Session 记录 | `SessionId` | 身份、项目绑定（`ProjectPath`）、创建与更新时间；当前位置这个指针 | `configure(New)` 建立时写入；当前位置随节点写入和 `rewind` 移动，更新时间随 `configure` 与接受事务刷新 |
| 会话设置 | `SessionId` | 会话的有效 profile、内联保存的系统提示（上限 2 MiB）、可选的模型（`ModelRef`）与可选推理档位（`Effort`），见[会话设置](settings.md) | `configure(New)` 创建会话时写入，之后由 `configure(Existing)` 更新；接受事务与 `rewind` 不修改 |
| InvocationId 索引 | `InvocationId` | 调用所在的 Step 与片段位置 | 登记调用时写入 |

一轮的结局、notice、usage 汇总和结束时间都从它的 Step 节点得出，见[关闭 Step 与 Turn](steps.md#关闭-step-与-turn)。配置表以内容摘要为键，键由内容算出，不会与引用它的节点不一致。TurnId 索引与 InvocationId 索引是派生记录，与节点在同一事务中更新。

模型上下文与视图读取的对话只有一种范围：从根到 Session 当前位置的路径。发给模型的上下文取这条路径上的 UserMessage 和已有回复的 Step；没有回复的 Step 不发给 provider，对话条目照常给出。根节点 `0` 不含内容，不发给 provider。要让模型上下文与当前视图换到其他分叉上的对话，先 `rewind` 到那里；历史查询按 `before` 的父链读取，不受当前位置限制，见[观察与重连](recovery.md#观察与重连)。

`rewind N` 只有一种语义：把当前位置移到节点 N 本身，下一次提交的新节点以 N 为父。它只移动这个指针，不在树上写入节点：对树的操作不进入树的记录。改写某条用户消息就是退回到它的父节点再发送，改写第一条消息就是 `rewind 0`。退回到一个没有回复的 Step，与退回到它的父节点，发给模型的上下文相同。会话层不提供“编辑消息”等第二种入口；前端可以把选中消息并改写的交互翻译为这两步。

当前 agent 是会话设置中的 agent（`profile.name`）；每一轮实际使用的 agent 记在它的 UserMessage 节点上。`rewind` 不改变会话设置，所以退回旧节点后当前 agent 不变，也包括退回根节点 `0`。通过 `configure` 指定 agent 会加载该 agent 的 profile 并更新会话设置。

### 组件与接口

后台 API 分为三类：命令改变会话或执行（`configure`、`submit`、`cancel`、`rewind`），视图推送会话现在的样子（`watch`），查询读取已写入的事实（`head`、`history`、`turn`、`list`、`log`、`usage`）。LocalClient 把它们交给前端。一次提交经 `LocalTurn` 进行：它是这次提交的具体句柄，保留原请求身份，处理接受回执、取消意图与退出交接；它不是第二个持久 Turn，也不拥有后台执行。前端持有句柄、消费视图，后台的 Owner 与 TurnCoordinator 分别监督操作和推进持久执行。

| 场景中的角色 | 负责的工作与交接 |
| --- | --- |
| 前端 CLI / TUI | 接收输入、选择操作，生成会话与请求身份，经 `configure` 创建会话或更新设置；发起提交、退回或取消，观察会话视图并决定何时退出 |
| 客户端库 LocalClient / LocalTurn | 交付配置与原提交请求，处理接受回执、取消意图与退出交接；转交视图与查询，超过单帧的消息由连接层分帧与拼回 |
| 后台入口 serve | 接收本地连接，维护后台实例及连接资源，将请求交给 Backend |
| 后台任务管理 Backend / Owner | Backend 管理准入、草稿清理、视图、查询及本进程诊断，为每个观察者保存视图的推送状态；准入时读取设置与凭据，见[执行配置冻结](acceptance.md#执行配置冻结)。Owner 监督已接管操作，并为正在执行的 Turn 保留当前尝试的段列表。提交从准备起持有同一取消载体，直至执行核心独立收尾或给出明确诊断，前端退出不改变其责任 |
| 执行协调 TurnCoordinator | 推进一次 Turn：请求模型、组织工具调用、把每一步进度写入会话库，最后关闭 Step 与 Turn |
| 模型驱动 ModelStepDriver / LLM | 接收对话和本次配置，请求模型，将响应交回 TurnCoordinator |
| 工具执行 ToolExecutor | 接收已登记的工具调用，执行后向 TurnCoordinator 返回执行结局与载荷 |
| 会话库 `lattice-session` | 保存 Session 记录、会话树、会话设置、配置表、TurnId 索引与 InvocationId 索引；每次推进是一个事务 |

前端负责用户交互，LocalClient 负责操作交付与连接，后台负责任务生命周期与视图，TurnCoordinator 负责持久执行流程，会话库负责事实的保存与一致性。[实现模块与公开操作](operations.md#实现模块与公开操作)进一步说明各 crate 和操作接口。

## 文件索引

| 文件 | 内容 |
| --- | --- |
| [settings.md](settings.md) | 会话设置、设置版本、`configure` 的计算基准与档位规则 |
| [flow.md](flow.md) | 从配置与提交到结果可见的完整流程，以及用户读到的几种事实 |
| [operations.md](operations.md) | 实现模块、公开操作、CLI `send` 顺序与 JSONL 记录、凭据私有交换及用量查询 |
| [acceptance.md](acceptance.md) | Turn 的接受、执行配置冻结、配置拒绝的阶段与诊断、模型请求及执行预算 |
| [conversation.md](conversation.md) | 有序消息、ToolCall 与跨 provider 回放投影 |
| [steps.md](steps.md) | Step 节点与事务序列、调用槽位，以及关闭 Step 与 Turn |
| [recovery.md](recovery.md) | 请求身份与找回、`configure` 的找回、观察与重连、取消与退出、执行者消失后的 Turn 及停止后台 |
| [concurrency.md](concurrency.md) | 会话库与项目绑定、事务与一致性、操作锁、[会话状态与草稿清理](concurrency.md#会话状态与草稿清理)、范围边界及输入与传输限制 |

## 验证与承接

| 检查对象 | 入口 |
| --- | --- |
| 对话顺序、批次校验、参数摘要 | `cargo test -p lattice-conversation` |
| 流解析的到达顺序与 provider 投影 | `cargo test -p lattice-llm` |
| 回复累积、流中断与资源计量 | `cargo test -p lattice-llm` |
| 单次尝试结果的映射 | `cargo test -p lattice-agent --features test-utils` |
| 会话库表编码、值版本、存在 `sessions.git` 或 `invocations.git` 时的拒绝、阶段给出的结论与读取校验 | `cargo test -p lattice-session` |
| 提交、多步 Turn、取消、退回分叉、进程退出后的结局与观察 | 真实二进制与真实状态根上的 E2E 场景，按[测试工程合同](../../development/scenarios/README.md#自动场景)选择 |
| 会话设置与配置读取 | 局部测试覆盖 `configure` 的基准计算、档位规则与设置版本；E2E 场景见[测试工程合同](../../development/scenarios/harness/config-and-profile-copy.md) |

E2E 崩溃场景在每个提交点 SIGKILL 后重开，先不做任何写入，核验：每个 Step、尝试和槽位停在崩溃前最后提交的阶段，读出的结论符合[预留、progress 与结论](#预留progress-与结论)的表；一个 Turn 中除最后一个 Step 外都以 `Continue` 关闭；`TurnSummary` 与最后一个 Step 的结论一致。随后核验 `send` 与 `rewind` 直接成功，旧 Turn 的节点保持原样；有 Owner 执行的 Turn 存在时 `send` 与 `rewind` 返回 Busy。

观察场景核验：中途开始的观察、变慢的观察者、空闲时 `rewind` 替换路径、Turn 关闭与最后一次推送同时就绪，各情况下观察者最后持有的条目与关闭后用历史查询重读的一致，并核验上文的段列表一致性；实时观察与重新打开会话看到的条目相同，包括重试提示；执行者消失后重新打开，会话头直接说明没有 Owner。历史查询的页边界只落在 UserMessage 之前，轮次序号与从根数起的结果相同。会话设置场景核验：原 `agent.toml` 修改或删除后，已有会话仍按已保存的会话设置执行；`configure` 修改模型或档位后重新打开会话，沿用当前会话设置的模型与档位；`rewind` 不改变会话设置与当前 agent；草稿会话逾期无观察者时被删除，有观察者或已接受过一轮的会话保留；前端的环境与后台启动时不同时，后台继续使用启动时的配置。提交场景覆盖[请求身份与找回](recovery.md#请求身份与找回)表中的每一行，以及接受结果不明时按 TurnId 查询找回。CLI 场景核验 `--json` 只出现八种记录，`send` 第一条是写明身份的 `submit`，按需输出 `configure` 记录，此后恰好有一条同一 TurnId 的 `submit` 回执，`session send --json` 只输出本轮，`session cancel <SESSION_ID>` 取消正在执行的一轮、没有时报告无可取消。

纯文档修订静态核对职责、流程、条款承接、数值来源、引用和差异；语义审阅须能复述提交、并行观察、结果取得、退回分叉及执行者消失后的结局，链接检查不能代替它。实际测试和未验证范围记录在对应任务中。

记录格式或事务序列修改须检查已存记录、调用方与读取校验。背景取舍见 [ADR 0012](../../adr/0012-backend-owns-session-settings.md)、[ADR 0010](../../adr/0010-protocol-adapters-and-configured-providers.md)、[ADR 0007](../../adr/0007-session-tree-as-single-authority.md)、[ADR 0006](../../adr/0006-tree-session-and-global-redb-store.md) 与 [ADR 0005](../../adr/0005-durable-turn-invocation-and-private-git-stores.md) 中的有效决策；未迁入的局部细则继续由[会话](../../../crates/lattice-session/AGENTS.md)、[API](../../../crates/lattice-api/AGENTS.md)、[对话](../../../crates/lattice-conversation/AGENTS.md)和 [Agent](../../../crates/lattice-agent/AGENTS.md)入口承接。
