---
status: active
---

# 实现模块与公开操作

> 所属领域：[会话与 Turn](README.md)

以下将前面的角色对应到代码模块，并按**产品 IPC 协议与客户端门面**、**后台宿主入口**、**后台内部实现**和**领域与网关 crate** 分清调用层级。`lattice-server` 负责后台内的跨领域协调，把工具执行接缝、模型网关和会话库写入接入 Turn；适用边界见[范围与禁止事项](concurrency.md#范围与禁止事项)，公开面规则见[Crate 职责与依赖边界](../crates.md)。

| 层级 | 模块与入口 | 具体职责 |
| --- | --- | --- |
| 产品 IPC 协议 | `lattice-ipc`（`Request` / `Response`、`LoginAction` / `LoginResponse`、`ipc`） | 定义本地 IPC 请求、响应、登录交换帧、`LocalError`、会话/模型展示类型及 IM 与后台组件状态类型；执行 16 MiB 单帧魔数校验、本进程有效 UID 读取与对端 UID 核验 |
| 产品客户端门面 | `lattice-local`（`LocalClient`、`LocalTurn`、`LocalAuthLogin`） | 前端唯一的本地操作入口：[`client.rs`](../../../crates/lattice-local/src/client.rs) 封装连接已运行的本地后台与单次 IPC 请求（含 IM 凭据与配置重载）；[`operation.rs`](../../../crates/lattice-local/src/operation.rs) 的 `LocalTurn` 驱动提交、观察流重连、取消回执与 `detach` 退出交接；[`login.rs`](../../../crates/lattice-local/src/login.rs) 的 `LocalAuthLogin` 驱动同连接 SIWC 登录交换 |
| 后台宿主与服务入口 | `lattice-daemon` 调用 `lattice_server::{serve, ServeExit, DrainOutcome, HostLifecycle, InstanceLock, ComponentRegistration, ComponentBuildOutcome, PreparedComponent, ComponentStop, ComponentRemountHandle, install_component_panic_hook, scoped_component_future}` | `lattice-server` 生产公开面，由独立宿主进程 `lattice-daemon`（及进程内组件 `lattice-im` 的 panic 作用域包装）调用：持有实例锁和 socket、在绑定发布后触发 `HostLifecycle::on_ready`、并发启动已注册组件的初始挂载（`lattice-daemon` 注册内置 `"im"` 组件与 `inotify` 配置监视）、启动模型元数据调度、接收并核验连接，并在 `configure(New)` 后触发后台草稿清理；收尾后经 `ServeExit` 将 `DrainOutcome`、`InstanceLock` 与 `shutdown_deadline` 交回 `lattice-daemon` 关闭 runtime |
| 后台内部实现 | `lattice-server` 私有模块 `local::components`（`ComponentManager`） | 托管进程内可选组件（如 `"im"`）：在后台就绪后并发启动初始挂载并与后续合并重新挂载串行，旧实例未退出前不启动新实例（单实例收尾上限 10 秒），配置无效时保留存活旧实例并记录 `UsingPreviousConfiguration`，以线程局部作用域抑制组件 panic 原始载荷并隔离异常，停机第 1 步唤醒在途重新挂载返回 `Stopping`、第 3 步在严格 10 秒上限内统一收尾，并向 `DaemonStatus.components` 投影非 `Disabled` 状态（见[IM 网关](../im.md)） |
| 后台内部实现 | `lattice-server` 私有模块 `local::service`（`Service`、`InProcessTurn`、`InProcessTurnOutcome`、`StopNotification`） | 包装同一个 `Arc<Backend>` 的进程内服务门面，向本地 IPC 分发与进程内调用方提供同一套 `configure`、`feed`、`context_op`、`trigger`、`submit`、`inspect`、`observe`、`cancel`、`head`、`turn`、`usage` 与 `stopping` 实现；`InProcessTurn` 绑定固定 `(SessionId, TurnId)` 只读观察流式助手纯文本与终态结局，丢弃时不取消执行 |
| 后台内部实现 | `lattice-server` 私有模块 `local::server`（`Backend` / `Owner`） | `Backend` 管理准入、草稿清理、会话库句柄、视图及本进程诊断，为每个观察者保存视图的推送状态，并将 IPC 的 `configure`、`feed`、`context_op`、`trigger`、`submit`、`inspect`、`cancel`、`head`、`turn`、`usage` 无条件委托给 `Service`；`Owner` 持有已接管操作的内存槽位（Turn 执行带身份、取消标志、回执、监督信息和当前尝试的段列表；`configure`、`feed`、`context_op`、`rewind` 与草稿清理为无 Turn 身份占位）。任务脱离请求连接或进程内调用栈运行，监督者等待操作收尾后才移除 `Owner` |
| 后台内部实现 | `lattice-server` 私有模块 `root`、`configure`、`profile`、`config_rejection` | `LocalWorkspaceRoot::from_path` 解析并规范化工作区路径；`configure::resolve_settings` 加载指定 agent 的 profile 或以当前会话设置为基准，查询模型可用档位并执行档位规则，构造新设置与档位调整回执；`ConfigRejection` 单一入口生成脱敏单行配置拒绝 |
| 后台内部实现 | `lattice-server` 私有模块 `session`（`SessionApi`）与 `stores` | 当前代会话库的内部异步门面与共享库句柄管理：在 `spawn_blocking` 中执行会话身份精确/前缀解析与用量读取，监控写提交异常并触发停写重开（`SessionApi` 生产不对外公开，仅在 `test` 或 `test-utils` 下供测试导出） |
| 后台内部实现 | `lattice-server` 私有模块 `turn`（`TurnCoordinator`、`start_execution`、`ToolExecutor`） | 唯一持久 Turn 协调者，持有全 Turn `SessionExecutionLock`，接受前经注入的 `ToolExecutor::open_turn` 冻结本轮工具目录（生产默认私有 `NoTools`），从会话设置冻结输入，推进模型循环、调用执行、Step 与 Turn 关闭；会话库写入经 `lattice-session` 的领域接口完成（`start_turn` 与工具接缝类型仅在 `test` 或 `test-utils` 下供测试导出，`run_turn` 仅限本 crate 单元测试） |
| 领域与网关 crate | `lattice-agent::ModelStepDriver` | 接收冻结请求和对话历史，驱动一次 provider 请求，向协调层返回类型化尝试结果，并发送非阻塞观察回调 |
| 领域与网关 crate | `lattice-llm` | 请求网关：提供无凭据的模型档位能力查询（`model_supported_efforts`），统一判定可用性，`GatewayTurn::prepare` 完成提交准入与模型及档位复核，每次请求按 provider `id` 与模型 `id` 解析配置、取得凭据，完成 provider wire 请求、流事件适配和对话投影 |
| 领域与网关 crate | `lattice-conversation` | 维护模型选择值（`ProviderId`、`ModelId`、`ModelRef`）、推理档位 `Effort`、有序消息、ToolCall typestate、调用结果与结构化 `ToolError`、内容摘要、尝试诊断和 usage 等纯数据 |
| 领域与网关 crate | `lattice-context` | 维护模型上下文编辑操作（`ContextOp` 与 7 类 `ContextEdit`）、渠道标签（`Channel`）、路径归约器（`resolve` → `EffectiveContext`）与 provider 中立投影（`EffectiveContext::project()`） |
| 领域与网关 crate | `lattice-session` | 维护独立于存储机制的会话树领域类型（`model/`：`AgentName`、`ProjectPath`、`TurnTimeout`、`RetryPolicy`），并拥有全局会话库 `lattice.redb`：Session 记录（含 `ProjectPath` 项目绑定）、会话树（`Root`、`Input`、`ContextOp`、`Step`）、会话设置（`SessionSettings` 与 `SettingsVersion`）、配置表、图片字节表 `blobs`、TurnId 索引与 InvocationId 索引；提供事务写入原语、由阶段给出结论、全树严格校验，以及原始路径与归约上下文两条独立读路径 |

公开入口按调用层级分为两类，后台内部实现不直接向外部调用方暴露（进程内 `Service` 与本地 IPC 在后台内部共用同一套准入、草稿清理、取消与查询实现）：

- **后台宿主入口（进程内 Rust 调用，非 IPC）**：`lattice_server::serve`，参数为状态布局、可选的 home 目录、排空上限、停机请求的接收端、宿主生命周期回调 `HostLifecycle` 与已注册组件列表 `Vec<ComponentRegistration>`，返回 `ServeExit`；仅由独立宿主二进制 `lattice-daemon` 在后台进程内调用。
- **产品的 IPC 操作（前端经 `lattice-local` 发起，线上消息由 `lattice-ipc` 定义）**：客户端命令（`lattice-cli`）与交互界面（`lattice-tui`）全部通过下表中的客户端入口与本地 IPC 消息完成用户工作，不直接调用 `SessionApi`、`configure` 或 `TurnCoordinator`：

| 用途与 IPC 消息 | 客户端入口（`lattice-local`） | 输入及前提 | 输出与效果 |
| --- | --- | --- | --- |
| 配置会话：`configure` | `LocalClient::configure_once`、`configure` | `New(SessionId)` 或 `Existing(SessionId)`、工作区路径 `workspace`（IPC 中为原始字节数组；必须是存在的规范绝对路径：后台规范化后核对结果与传入值相同，相对路径、含符号链接或 `..` 的路径以及无法规范化的路径均返回 BadRequest），以及可选的 `agent`、`model`、`effort`；`New` 必须给出 `agent` | 新建草稿会话并写入初始会话设置，或核对项目绑定后更新已有会话的会话设置并刷新 `updated_at`；返回 `ConfigureReply { head, adjustment }`（`adjustment` 为 `Option<EffortAdjustment { from, to }>`），观察者收到新的会话头。详情见[会话设置](settings.md) |
| 解析会话身份：`resolve_session` | `LocalClient::resolve_session` | 完整 `SessionId` 或至少 4 个字符的前缀 | 精确身份优先，否则在同一读事务内按前缀做一次范围读取并返回匹配的 `SessionRecord`；空白或不足 4 个字符返回 BadRequest，无匹配返回 NotFound，多条匹配列出至多 5 个候选并返回 BadRequest |
| 查看会话：`list`、`log` | `LocalClient::list_sessions`、`log` | 无，或 SessionId | `list` 列出全部会话（草稿会话照常出现在列表中），每行给出 SessionId、项目绑定 `ProjectPath`（`--json` 中为原始字节数组，人类可读输出为有损文本）、当前节点序号、节点数、更新时间，以及最后一轮未关闭时它的 TurnId 和后台是否正在执行它；`log` 先给出项目绑定（同上，`--json` 为原始字节数组，人类可读输出为有损文本）、当前位置、当前 agent 与未关闭的最后一轮，再以树形列出全部节点（含不在当前分叉上的节点），标出当前位置，并在每个 Turn 的首节点注明 TurnId 与 `TurnSummary` |
| 观察会话：`watch` | `LocalTurn`（`LocalClient::observe`、`follow_turn`、`start_turn`；底层 `LocalClient::watch` 仅在 `test` 或 `test-utils` 下导出） | SessionId | 推送会话头、当前路径上最近几轮的对话条目、之后变化的条目与生成中的文字，见[观察与重连](recovery.md#观察与重连)；草稿会话与当前位置为根节点的会话同样可以观察（初始只有会话头），`configure` 成功后观察者收到新的会话头。会话不存在时返回 NotFound |
| 查看会话头：`head` | `LocalClient::session_head` | SessionId | 返回与视图开头相同的会话头，完整字段为：`session_id`（会话身份）、`position`（当前节点序号）、`agent`（当前 agent，取自会话设置）、`model?`（会话设置中的模型 `id`）、`provider?`（会话设置中的 provider `id`）、`effort?`（会话设置中的推理档位）、`settings_version`（当前设置版本）、`turn_id?`（当前位置所属的那一轮；停在根节点、`Input` 或 `ContextOp` 节点时为空，因此不能用 `turn_id` 为空判断一个会话是不是草稿会话）、`owner`（`turn_id` 所示那一轮有没有 Owner）、`diagnostic?`（后台已知的执行诊断）、`path_replaced`（单次查询固定为 `false`；观察流中因 `rewind` 替换当前路径时为 `true`）、`has_older?`（仅在窗口头——即初始、重载或 `path_replaced`——给出 `Some(bool)`，常规增量头及单次 `head` / `configure` 回复为 `None`）。不读条目，不占观察许可；遇到同会话无 Turn 身份的操作在执行时等待其结束后读取。会话不存在时返回 NotFound |
| 查看一个 Turn：`turn` | `LocalClient::turn`、`wait_for_turn` | TurnId | 返回这一轮的 `TurnSummary` 与全部 Step 节点；请求仍在准入时返回在途，没有这个请求时返回 NotFound。接受结果不明时用它找回 |
| 投喂输入：`feed` | `LocalClient::feed` | 已有会话的 SessionId、规范工作区路径 `workspace`、来源渠道 `channel`、保序输入片段 `parts`（文本与图片，图片为 base64）及可选 `expected_position`；前端发送前经 `check_feed_plan` 核对单轮上限与 16 MiB 帧长 | 按四类写操作统一优先级校验（见下 `submit` 行），持无身份 Owner 槽与 `SessionExecutionLock`，落盘写入单条 `Node::Input`（及去重图片 `blobs`）并推进 `session.current`，调用 `ObserverSink::record_appended_node` 实时向观察者推送新会话头与 `ConversationEntry::Input`；不触发模型请求，返回 `FeedReply { session_id, node }` |
| 上下文操作：`context_op` | `LocalClient::context_op` | 已有会话的 SessionId、规范工作区路径 `workspace`、`op: ContextOp`、可选 `images: Vec<ContextImage>`（供 `ReplaceInput` 引用，图片为 base64）及可选 `expected_position`；前端发送前经 `check_context_op_plan` 核对图片去重/引用与 16 MiB 帧长 | 按四类写操作统一优先级校验，逐张校验 `ImageRef::from_bytes`（无效或重复摘要返回 BadRequest），持无身份 Owner 槽与 `SessionExecutionLock` 调用 `apply_context_op` 写入 `Node::ContextOp`（`ContextError` 映射为 BadRequest）并推进 `session.current`，经 `ObserverSink::record_appended_node` 实时推送新会话头与 `ConversationEntry::ContextOp`，返回 `ContextOpReply { session_id, node }` |
| 独立触发：`trigger` | `LocalClient::trigger`、`start_trigger`、`start_trigger_submission`（复用 `LocalTurn`） | 已有会话的 SessionId、TurnId、规范工作区路径 `workspace`、`trigger_channel` 及可选 `expected_settings` / `expected_position`（不携带新输入） | 与 `submit` 共用带 `TurnId` 的 Owner 槽、ADR 0008 §7 身份优先准入规则（`read.turn_node(turn_id).is_some()` 立即返回 Conflict(`turn_id_used`)）、取消载体与 `turn` 找回；落库前验证当前路径归约后的有效上下文非空（为空返回 BadRequest）并经 `ResolvedRequest::check_request` 检查存活历史图片模态与 32 MiB 预算，再调用 `trigger_turn` 预留 `Step 0`（`input_nodes` 为空）并由 `TurnCoordinator` 推进执行 |
| 提交：`submit` | `LocalClient::start_turn`、`start_submission`（返回 `LocalTurn`） | 已有会话的 SessionId、TurnId、工作区路径 `workspace`（IPC 中为原始字节数组，要求同 `configure`）、`trigger_channel`、`1..=N` 条带 `Channel` 输入 `inputs: Vec<TurnInputItem>`（每条含保序 `parts`，图片为标准 base64），以及可选的 `expected_settings` 与 `expected_position` | 四类写操作（`submit`、`trigger`、`feed`、`context_op`）执行统一错误优先级：1. 准入锁内：`turn_id` 在途冲突（仅 `submit`/`trigger`，返回 Conflict(`turn_id_used`)）→ 后台停机返回 Stopping → 全局 Owner 达 32 返回 Busy；2. （仅 `submit`/`trigger`）`read.turn_node(turn_id).is_some()` 立即返回 Conflict(`turn_id_used`)（保留 ADR 0008 §7 身份优先原则，重发不承担找回职责）；3. 工作区规范路径核验（BadRequest）→ 会话存在性（NotFound）→ 项目绑定核对（Conflict(`binding_mismatch`)）；4. 同会话已有活跃 Owner 互斥返回 Busy（严格排在输入格式 BadRequest、`SettingsChanged` 与 `PositionChanged` 之前）；5. 输入规范化 / 图片字节与摘要校验（BadRequest，按[输入与传输限制](concurrency.md#输入与传输限制)检查）；6. 获取 `SessionExecutionLock`（被占用返回 Busy）→ 在模型准备与 32 MiB 预检前先读核对 `expected_settings`（Conflict(`settings_changed`)）与 `expected_position`（Conflict(`position_changed`)）→ 检查网关配置、模型、工具目录及首步 `check_request` 预检 → 写事务内二次原子核对绑定、设置版本与位置基线。成功后由 `LocalTurn` 交付接受回执与观察更新，见[请求接受](acceptance.md#请求接受)与[请求身份与找回](recovery.md#请求身份与找回) |
| 检查有效上下文：`inspect` | `LocalClient::inspect` | SessionId 与可选位点 `position: Option<NodeId>`（`None` 取 `session.current`） | 纯只读查询：不抢 Owner 槽与 `SessionExecutionLock`（活跃 Turn 执行期间仍直接成功、不返回 Busy），返回 `InspectReply { session_id, position, inspection: ContextInspection }`；会话或指定节点不存在时返回 NotFound |
| 查看用量：`usage` | `LocalClient::usage` | SessionId（支持精确身份或前缀） | 当前路径上逐轮的用量与费用估算（每轮以 `start_node: NodeId` 标识，CLI 文本表首列在 `NODE` 表头下显示 `@<start_node>`），给出当前路径合计和全会话合计（含 rewind 丢掉的分支）；用量口径与费用规则见[模型接入合同](../llm/usage.md#用量缓存与费用) |
| 模型候选：`models` | `LocalClient::models` | `ModelCandidateQuery`：`List { provider: Option<ProviderId>, ready_only: bool }` 或 `Target(ModelRef)` | `List` 给出候选及各自的上下文窗口、输出上限、可用档位、价格与 `availability`，过滤 deprecated 与账号目录外的 `chatgpt` 模型；`ready_only` 为真时只返回 `ready` 候选且跳过不可用 provider 的模型与警告。`Target` 返回指定模型的单项候选（含 deprecated、目录外或无元数据的模型）及其 `availability`。合并只读当前全局配置与元数据；`List` 范围包含已登录且未被跳过的 `chatgpt` 且缺目录时先异步补拉，`Target` 保持纯本地，见[模型候选](../llm/models.md#模型候选) |
| 凭据与元数据：`auth_*` 与 `models_refresh` | `LocalClient::{auth_set, auth_login, auth_logout, auth_remove, auth_status, models_refresh}` 与 `LocalAuthLogin` | 见[凭据命令](../llm/credentials.md#凭据命令) | 录入与删除 key、SIWC 登录与登出、provider 可用性状态（含后台生成的 `ProviderAuthStatus.diagnostic`）；立即刷新 models.dev 与已登录账号的模型目录。两来源分别发布，部分失败返回失败来源、原因及已更新部分 |
| IM 凭据与配置重载：`im_auth_set`、`im_auth_remove`、`im_auth_status`、`im_reload` | `LocalClient::{im_auth_set, im_auth_remove, im_auth_status, im_reload}` | 平台名（当前仅支持 `telegram`）与密钥（仅 `im_auth_set`），或无参数 | 在阻塞线程池经 `ImAuthStore` 读写 `<config_dir>/im/auth.json`（错误零秘密回显），写删成功后或收到 `im_reload` 时触发 `"im"` 组件合并重新挂载并返回 `ImAuthSetReply`、`ImAuthRemoveReply`、`ImAuthStatus` 或 `ImMountResult`（见[IM 网关](../im.md#宿主托管重新挂载与-cli-接口)） |
| 读取更早的对话：`history` | `LocalClient::history_before`（每次取 `HISTORY_TURNS` 轮） | SessionId、节点 `before` 与轮数（1 到 100，超出返回 BadRequest） | 沿 `before` 的父链往回取完整的若干轮，返回 `LocalHistory { entries, has_more }`，条目与视图中的扁平 `ConversationEntry` 相同，由 `has_more` 显式标明是否仍有更早历史，见[观察与重连](recovery.md#观察与重连) |
| 退回：`rewind` | `LocalClient::rewind` | SessionId、目标 NodeId 与工作区路径 `workspace`（IPC 中为原始字节数组，要求同 `configure`） | 核对项目绑定 `ProjectPath`（错误诊断中的路径为有损文本）后，把当前位置移到树上任意已有节点；之后的提交从那里分叉，原分支保留在树上。不修改会话设置，也不修改工作区文件 |
| 请求取消：`cancel` | `LocalTurn::cancel`，或独立命令的 `LocalClient::cancel` | TurnId（`session cancel` 先经 `head` 查出当前轮的 TurnId） | 句柄同步登记意图并管理在途回执；独立命令返回后台接管结果，持久结局另行查询 |
| 查看与停止后台：`status`、`stop` | `LocalClient::{status, status_with_peer_pid, stop}`（返回 `DaemonStatus` / `StopOutcome`） | 当前本地后台 | `status` 返回 `DaemonStatus`（字段为 `epoch`、`stopping`、`owners`、`unconfirmed_operations`、`components`，其中 `components` 为按 `name` 排序的非 `Disabled` 组件 `ComponentStatus` 列表），`status_with_peer_pid` 同时经同一连接的 `SO_PEERCRED` 读取对端进程号；`stop` 关闭新准入并等待已接管操作收尾，socket 撤下、`owner.lock` 可取得后返回 `StopOutcome`（同版本返回完整 `DaemonStatus`；更旧协议后台优先经版本无关控制帧 `LTCTRL\0\x01` 返回 `CrossVersionControl`，不支持控制帧的历史旧后台经预先捕获的同 UID `pidfd` 发送 `SIGTERM` 返回 `CrossVersionSignal`；更新版本仍拒绝），限时内锁仍未放开报 Stopping |

CLI 的 `lattice session` 子命令为 `send`、`feed`、`context-op`、`trigger`、`inspect`、`rewind`、`list`、`log`、`turn`、`usage` 和 `cancel`，顶层 `lattice run` 是 `session send` 的别名；`lattice daemon` 子命令为 `status`、`start`、`restart`、`stop` 与 `install`（`install` 写出用户级 `lattice.service` 并依次调用 `systemctl --user daemon-reload`、`enable <unit>` 与 `restart lattice.service`；`start` 与 `restart` 薄封装 `systemctl --user start|restart lattice.service`，均不支持 `--json`，见 [ADR 0011](../../adr/0011-im-gateway-inside-backend.md) 与 #436）；`lattice im` 子命令为 `auth`（`set`、`remove`、`status`）与 `reload`（见[IM 网关](../im.md#宿主托管重新挂载与-cli-接口)）；凭据与模型的命令 `lattice auth`、`lattice models` 见[模型接入合同](../llm/credentials.md#凭据命令)。`session usage <SESSION_ID>` 逐轮列出用量与费用估算并给出合计，支持 `--json`。`session turn` 查看单个 Turn 的详情，请求仍在准入时报告在途，接受结果不明时用它找回；`--follow` 遇到请求仍在准入时等准入结束，不在途中退出，已接受则继续观察这一轮直至结束，没有这个请求则报告 NotFound，退出 2。`session cancel <SESSION_ID>` 取消该会话当前正在执行的一轮：CLI 用 `head` 查出会话头所示那一轮的 TurnId，再按 TurnId 取消，并输出该 TurnId；没有正在执行的一轮时报告无可取消。已接管、未接管或没有可取消的一轮时退出 0；取消回复丢失、结果不明时，输出带原 TurnId 的取消记录，状态为 `unknown`，退出 1；会话不存在退出 2。

`session send`（`--model <provider>/<model>` 与 `--effort <档位>` 选择模型与推理档位；`--image <PATH>` 附带图片，可重复、按给出顺序排在文本之后，可以只给图片，文本与图片都没给时为参数错误并退出 2；`--channel <CHANNEL>` 指定来源渠道，默认 `cli`，同时用作 `trigger_channel` 与单条输入项渠道；`--expected-position <NODE_ID>` 校验已有会话的期望位点基线，须与 `--session` 同用，无 `--session` 时直接以退出码 2 拒绝；新建会话时 `--agent` 必填，已有会话可选）先经 `LocalClient::status` 连接已经运行的本地后台（未运行时输出 `not_running` 错误记录并以 `1` 退出，不生成请求身份），随后按以下顺序执行：

1. 带 `--session` 时先解析会话前缀，不带时生成新的 SessionId；随后生成 TurnId，输出 `submit` 的 `sending` 记录，再读取当前目录 `current_dir()`（读取前收到 SIGINT 则输出 `submit` 的 `not_accepted` 回执并退出 1；`current_dir()` 失败则输出 `submit` 的 `rejected` 回执并退出 1；路径规范化与校验由后台在 `configure` 或 `submit` 时执行）。因此进程无论在哪一步中断，都能按第一条记录中的 TurnId 查询结果；中断在提交发出之前时，查询返回 NotFound。
2. 读取每张图片（须为常规文件、非空、不超过 3.75 MiB，且魔数为 PNG、JPEG、WebP 或 GIF），保留 `ImageRef` 并随即丢弃图片字节，再构造 `PlannedTurnRequest` 经 `check_turn_plan` 按[输入与传输限制](concurrency.md#输入与传输限制)做单轮上限、非空与 16 MiB 帧长度本地预检；未通过时输出 `submit` 的 `rejected` 回执并退出 2，不执行 `configure`，因此不会留下草稿会话。
3. 决定是否先执行 `configure`：
   - 不带 `--session`：执行 `configure(New)`；
   - 带 `--session`，且给出了 `--agent`、`--model` 或 `--effort` 中的任何一个：执行 `configure(Existing)`；
   - 带 `--session`，三者都没给：跳过 `configure`。
4. 执行了 `configure` 时，按结果继续（`configure` 只发一次，结果不明时按 `unknown` 处理；重跑命令是安全的，见[configure 的找回](recovery.md#configure-的找回)）：

   | 结果 | CLI 的输出与后续 |
   | --- | --- |
   | `configured` | `--json` 输出 `configure` 的 `configured` 记录（人类可读模式在档位自动调整时打印一行提示），随后进入提交步骤，`expected_settings` 为空 |
   | `rejected` | 先输出 `configure` 的 `rejected` 记录，再以同一个错误输出 `submit` 的 `rejected` 回执；退出码按提交被拒的规则 |
   | `unknown` | 先输出 `configure` 的 `unknown` 记录，再输出 `submit` 的 `not_accepted` 回执；退出码为 1 |

5. 提交之前重读每张图片并核对长度、格式与摘要未变；读取失败或发生改变时输出 `submit` 的 `rejected` 回执并退出 2（已输出的 `configure` 记录保留），不发出 `submit`；核对通过后按 `[Text(prompt)?, Image…]` 构造 `TurnRequest` 发起提交。

上下文域的四个新增子命令统一以位置参数 `<SESSION_ID>` 接收已有会话（经 `resolve_session` 支持唯一前缀解析），并严格保持 `parse_node_id` 仅接受纯 ASCII 数字节点号：

- `session feed <SESSION_ID> [PROMPT] [--image <PATH>...] [--channel <CHANNEL>] [--expected-position <NODE_ID>]`：`PROMPT` 与 `--image` 至少给其一，`--channel` 默认 `cli`。先在本地读取 `current_dir()` 与图片引用并执行 `check_feed_plan`（未通过直接以 `bad_request` 退出 2，不连接后台），随后连接后台解析会话、复核计划并重读校验图片字节后调用 `feed`；文本输出 `session=<id> node=<n>`，`--json` 输出 `FeedReply`。
- `session context-op <SESSION_ID> <EDITS...> [--channel <CHANNEL>] [--expected-position <NODE_ID>]`：支持全部 7 种 `ContextEdit`（`--omit-node <NODE_ID>`、`--replace-input <NODE_ID> <TEXT>` 后可紧跟 0..=8 个 `--image <PATH>`、`--replace-step <NODE_ID> <TEXT>`、`--omit-invocation <INVOCATION_ID>`、`--replace-tool-result <INVOCATION_ID> <TEXT>`、`--strip-reasoning <NODE_ID>`、`--omit-image <NODE_ID> <IMAGE>`、`--replace-image <NODE_ID> <IMAGE> <TEXT>`），严格按命令行出现顺序组装多条编辑（未传编辑标志或 `--image` 未紧跟在 `--replace-input` 之后时报错退出 2）。`<IMAGE>` 接受 `<media_type>:<byte_length>:<sha256_hex>`（与 `session inspect` 输出格式一致）或 `@<PATH>`（流式计算本地文件 `ImageRef`）；携带的图片载荷按 `sha256` 去重并先在本地经 `check_context_op_plan` 预检后再连接后台调用 `context_op`；文本输出 `session=<id> node=<n>`，`--json` 输出 `ContextOpReply`。
- `session trigger <SESSION_ID> [--channel <CHANNEL>] [--expected-position <NODE_ID>] [--agent <NAME>] [--model <PROVIDER/MODEL>] [--effort <EFFORT>]`：不写入新输入，直接触发基于当前有效上下文的一轮推理；传入 `--agent`、`--model`、`--effort` 任一时先执行 `configure(Existing)` 并将返回的 `settings_version` 作为 `expected_settings` 传入 `trigger`，否则 `expected_settings` 为 `None`。完整复用 `session send` 的流式渲染、信号取消交接、八种 JSONL 记录与 `0`/`1`/`2` 退出码。
- `session inspect <SESSION_ID> [--position <NODE_ID>]`：只读检查当前或指定位点的有效模型上下文，文本模式列出位置、有效条目与已操作节点数、总文本与总图片字节、按渠道升序计数、已操作节点状态、可直接复制给 `context-op` 的 `<media_type>:<byte_length>:<sha256_hex>` 图片列表与逐条条目摘要，`--json` 直出 `InspectReply`。

`session send`、`session trigger` 与 `session turn --follow` 的 `--json` 输出 JSONL，单轮跟随模式只放行归属目标 `TurnId` 的 `Step` 条目与文本增量（不含前置 `Input` 或 `ContextOp` 条目），共有八种记录：`submit`（发出请求前一条写明 TurnId 与 SessionId，结束时一条写明回执结果）、`configure`（会话配置结果：`configured`、`rejected` 或 `unknown`）、`cancel`（取消回执）、`head`（会话头）、`entry`（扁平对话条目，读者按 `node` 覆盖）、`text`（生成中的文字）、`connection` 与 `error`。向已有会话发送且未给出 `--agent`、`--model`、`--effort` 时，或在 `configure` 之前读取当前目录失败、中断，或首遍图片读取与本地预检未通过时，没有 `configure` 记录；`session turn --follow` 不会配置会话，也不输出 `configure` 记录。结局在本轮最后一个 Step 条目的 `TurnSummary` 中，不另设结束记录。

输出写明身份的 `submit` 之后，这次提交以一条回执结束（进程被杀除外）。回执的 `result` 为 `accepted`（带接受的这一轮）、`rejected`（带拒绝的错误）、`not_accepted` 或 `unknown`（结果不明，可带错误）；`recovered` 表示结果来自按 TurnId 查询。后台的拒绝（包括 `configure` 与提交的配置错误）是 `rejected`。`rejected` 的错误为 BadRequest（含配置拒绝）、NotFound 或 Conflict 时退出 2，为其他种类（如 Busy、Stopping）时退出 1；`configure` 被拒与提交被拒按同一规则。`not_accepted` 表示确定未被接受，有三种来源：按 TurnId 查询为 NotFound，取消发生在请求写出之前、请求没有发出，或前置 `configure` 结果为 `unknown`、提交未发出。`error` 记录只用于不属于这次提交的错误，例如生成身份之前的使用错误、观察时后台回复的错误；`sending` 要写完整的 SessionId，`--session` 的前缀在生成身份之前解析，解析失败也是 `error` 记录。回执为 `not_accepted` 或 `unknown` 时退出 1。

`session turn` 的详情（含 `--json`）给出 `TurnSummary`，并按步骤序号列出该 Turn 的全部 Step 节点：节点序号、`progress` 及其给出的结束方式与原因，每次尝试的 `progress` 及其给出的终态与原因，以及各槽位的 `progress` 及其给出的结果。它不按路径读取对话。命令行按 TurnId 读取一个 Turn 的全部 Step 只有这一个入口；观察中的 Step 节点随视图推送，见[观察与重连](recovery.md#观察与重连)。输出不含冻结配置中的秘密。不带 `--follow` 而请求仍在准入时，`session turn` 输出一行在途说明，`--json` 时输出 `{"status":"in_flight","turn_id":…}`，都退出 0。

`session turn --follow` 所跟的一轮有 Owner 执行时一定在当前位置（执行中 `rewind` 与同会话的新提交都返回 Busy），视图开头就给得到它的条目。这一轮已结束（已关闭，或未关闭而没有 Owner）时，若视图开头给出的条目里有它带 `TurnSummary` 的最后一步，照常输出这一轮的记录；否则（之后又跑了 10 轮以上，退回到这一轮中途，或最后一步已不在当前路径上）只给结局，不给回复正文：按 `session turn` 的详情输出（`--json` 时是同样的一行），退出码按最新结局。

LocalTurn 交付提交与取消的回执。普通退出使用 `detach` 取得最后已知的提交、取消与诊断，或由 Drop 停止本地后续工作；两者都不新增取消。`LocalTurn::turn_id` 给出提交与取消共用的身份。

公开类型与方法见[本地客户端门面](../../../crates/lattice-local/src/lib.rs)、[本地连接与单次请求](../../../crates/lattice-local/src/client.rs)、[LocalTurn 与客户端结果类型](../../../crates/lattice-local/src/operation.rs)、[IPC 协议与结果类型](../../../crates/lattice-ipc/src/types.rs)、[后台协调入口](../../../crates/lattice-server/src/lib.rs)及[后台宿主二进制入口](../../../crates/lattice-daemon/src/main.rs)，命令用法见[验证操作指南](../../development/validation-guide.md#环境与后台)。

## 凭据命令与私有交换

凭据命令经后台执行，规则见[模型接入合同](../llm/credentials.md#凭据命令)。登录占一个普通连接，不建立 Turn Owner：前端发 `auth_login {provider}`，后台在同一连接交付 `auth_login_authorization {authorization_url}`；前端可以发送 `action=callback {url}` 或 `action=cancel`，后台最后给 `auth_login_result`（signed_in、非敏感状态及目录刷新结果，或 cancelled）或安全错误。凭据保存后拉取目录，目录失败随 signed_in 返回警告，不撤销登录。浏览器直接回调无需前端续消息。新消息与其载荷严格拒绝未知字段，URL 只在私有登录消息中传输。

前端 EOF、取消或传输失败会取消未提交的登录；后台负责必要文件工作的监督收尾。凭据文件发布是成功提交点，发布后断线或迟到取消不回滚，结果不明时用 `auth status` 核验。持续读取任务保存同一连接的半帧进度与响应编号，界面取消一次事件等待不会重建 socket 读取。logout/remove 的结果显式带撤销状态，remove 同时保留是否删除材料的含义。

## 当前路径的用量查询

`usage` 在一个读事务内固定 Session 的当前位置，沿父链到根，按 `Step 0` 分组并按路径先后返回。退回轮中某个 Step 时，当前路径只累计该 Step 及其祖先中的尝试。每轮给出 `turn_id`、该轮 `Step 0` 的节点号 `start_node`、`Step 0` 的 `turn_start` 中的 `provider` 与 `model`，以及 `usage`；模型身份取自该轮 `Step 0` 节点。返回对象为 `{ session_id, position, turns, path_total, session_total }`，其中 `position` 是本次读取的位置，逐轮 `usage`、`path_total` 与 `session_total` 均按[尝试计量与完整性](steps.md#关闭-step-与-turn)派生。`path_total` 汇总当前路径；`session_total` 在同一读事务中汇总该会话全部节点的尝试，不沿父链，包含 rewind 后留存的分支，每个节点只计一次。

当前位置为根、`Input` 或 `ContextOp`（尚未到达该分支上任何 `Step 0`）时，仅汇总其祖先路径上已存在的轮次；若祖先路径无任何 `Step`，`turns` 为空、`path_total` 为空和，`session_total` 仍包含已有分支；路径止于 `Reserved` Step 0 时保留该轮，没有尝试的空和由 `attempts=0` 表达。Sent 与执行者消失后留下的尝试同样参与汇总。

会话库严格打开并校验成功后，身份解析与用量读取只读取 Session 记录、身份键和会话节点的扁平轻量计量投影，零缓冲跳过 `input`、`op`、`reply`、`slots` 与 `config_digest` 等正文或配置载荷。需要打开会话库时，仍按[存储合同](../storage.md#身份版本与打开)执行全库解码与校验。

`lattice session usage <SESSION_ID>` 的文本表首列在 `NODE` 表头下显示 `@<start_node>`，随后逐轮列出 `TURN`、模型、五项计数、尝试数与费用，末尾两行为“当前路径合计”和“全会话合计”，分别对应 JSON 的 `path_total` 与 `session_total`。逐轮明细只列当前路径，所以 `session_total` 可能大于逐轮之和。单项计数没有任何尝试报告时显示 `-`，已知零显示 `0`，溢出的小计附 `(overflow)`；金额格只按[费用估算](../llm/usage.md#费用估算)显示确切、估算或未知，不附 `(partial)`。全局 `--json` 输出上述一个结构化对象；读取成功退出 0，身份解析和错误分类沿用其他会话查询。
