---
status: active
---

# 会话存储与并发

> 所属领域：[会话与 Turn](README.md)

## 会话库与项目绑定

会话事实全部位于全局会话库 [`lattice.redb`](../storage.md#私有状态根)，由 `lattice-session` 维护表定义、值格式版本与业务不变量。库中的记录集合见[表、读取范围与 rewind](README.md#表读取范围与-rewind)。

Session 永久绑定产品组合层提供的 `ProjectStoreKey`，执行请求的规范工作区必须与该绑定一致。提交端保留规范 SessionId，输入或显示中的缩写由公开 API 解析。操作员查询按项目绑定过滤，接口见 [LANCET 操作员接口](../lancet.md#操作员接口)。

会话库只在创建新 Session 的提交路径上首次创建；打开不初始化、不修复。发现旧版会话库或调用库时打开失败并指出路径，由人删除或存档。创建、打开与格式版本规则由[存储合同](../storage.md#会话库)维护。

## 事务与一致性

每次推进是一个写事务，使用 `Durability::Immediate`：`commit` 返回时事务已持久，已提交的事务对之后开始的读事务可见，读者不会看到尚未持久的提交；使用条件见[存储合同](../storage.md#会话库)。一个事务内的多条记录一起生效或一起不生效，所以跨记录的关系，如“以 `Continue` 关闭的 Step 与预留的下一个 Step”“槽位与 InvocationId 索引”，在任何读事务中都成立。

- 写事务内只做会话库读写，不等待模型、Tool、网络或其他锁；阻塞的库操作在 `spawn_blocking` 等阻塞执行边界内完成，不跨 await 持有写事务。
- 读者用读事务取得一致快照，不需要锁。
- **事务失败**：redb 的 `commit` 错误分两类。`TransactionPoisoned` 表示事务已回滚、库仍可开始新事务，但引起它的存储错误可能仍在库中；其他错误表示事务可能已持久、不能当作已回滚，库随后拒绝新的写事务。本项目的策略对两类一律保守处理：后台停止在该库上的全部写入，正在执行的 Turn 不再启动 provider 或 Tool，后台关闭并重新打开会话库，以读到的记录为准；仍未关闭的 Turn 停在已写入的进度，见[执行者消失后的 Turn](recovery.md#执行者消失后的-turn)。

## 操作锁

| 操作与执行者 | 锁和范围 | 持有期及忙时处理 |
| --- | --- | --- |
| 执行核心的 live Turn | 独占 SessionExecutionLock | 覆盖整个 Turn；新执行非阻塞获取，同 Session 不同请求忙时拒绝，不排队 |
| `rewind` | 独占 SessionExecutionLock | 覆盖该写操作；与 live Turn 互斥 |
| `list`、`log`、`turn`，视图与历史查询的读取 | 会话库读事务 | 不取操作锁，不等待 live Turn 排空 |
| 后台准入 | Backend 短临界区 | 只管理接管权，释放后才执行 provider、存储或整段 Turn |
| 工作区观察与变更 | 项目 WorkspaceOperationLock | 按[工作区操作锁](../workspace.md#操作锁)覆盖相应操作；关闭 Step 不取得该锁 |
| 会话库写入 | 该库的写事务 | 库保证同一时刻只有一个写事务；覆盖一次推进 |

不同 Session 的执行锁独立，同项目不同 Session 的模型执行可以重叠。共享工作区按操作协调，而非按整个 Turn 排队。涉及多类锁时依次取得 SessionExecutionLock、WorkspaceOperationLock，最后是会话库写事务或至多一个 repository write lock，两者不同时持有；锁序与释放规则由[存储合同](../storage.md#锁与并发语义)维护。

## 范围与禁止事项

| 对象与条件 | 边界、理由及可采用流程 |
| --- | --- |
| 前端与观察连接 | 不持有后台执行生命周期，释放观察不取消任务；需要取消时发送原身份 cancel，需要停止时显式 stop |
| Turn 持久协调 | Backend/Owner 只接管和监督，不建立第二个持久 Turn 状态机；产品执行和关闭经 TurnCoordinator 与会话库 |
| 已接受请求 | 不删除或当作未发生，不另建请求账本或持久双写；保留原身份，按 TurnId 查询找回，执行者消失后保留已写入的进度 |
| 模型/Tool 重试 | 模型只按原因与尝试预算重试；执行者消失后不请求 provider、不重执行 Tool，避免重复外部效果 |
| 已写入的节点 | 只有末端未关闭的 Step 由执行它的协调层改写，其余不修改、不删除；退回只移动当前位置 |
| 会话事实的保存位置 | 接受之后发生的事只写在节点上：不为同一事实另设记录，也不在读取时从别处补齐。`TurnSummary`、重试提示等读取投影从节点得出 |
| 对话与会话领域 | conversation 不持有存储、运行时、HTTP 客户端或身份生成器；session 只接收组合层提供的项目绑定，不从进程 cwd 推导 |
| 配置与凭据 | 后台读取，前端只交选择；endpoint 只来自内置表与用户全局的 `providers.toml`，凭据只在 `auth.json` 与后台内存中，见[模型接入合同](../llm/providers.md#endpoint-的来源) |
| 文件与跨库效果 | 退回不修改文件，取消/超时/断连不推定效果未发生，也不提供跨库事务或回滚；文件恢复不在本合同提供 |
| 锁与阻塞操作 | 不同时持有会话库写事务与仓库写锁，不持写事务或仓库锁反向取操作锁或升级共享锁；写事务内不等待外部操作 |
| 部署与数据能力 | 本地 Linux 后台与本地持久化范围内工作；远程服务、HA、多进程访问会话库、真实数据迁移和逐 token 持久化不属于本合同 |

## 输入与传输限制

下表分别列出既有协议上限和当前本地实现的资源上限。MiB/KiB 按二进制字节计量；列出的大小边界包含等于上限，超过才拒绝。没有容量或延迟测量足以证明这些数值的充分性，不把常量当作性能保证；新增本地上限的充分性仍待裁决。

| 对象/场景 | 度量、限制与触发行为 | 来源与证据范围 |
| --- | --- | --- |
| 新用户输入 | User 消息全部 text parts 的 UTF-8 字节之和，最多 2 MiB，即 2,097,152 字节；超一字节在接受前拒绝 | 既有协议；`lattice-session` 的 `MAX_USER_INPUT_BYTES`。源码解释为对齐助手正文上限，非测量结果 |
| 工具目录快照 | 按工具名排序 entries 的规范 JSON，最多 1 MiB，即 1,048,576 字节；新快照超限拒绝，已存快照不合法时报损坏 | `lattice-session` 的 `MAX_CATALOG_SNAPSHOT_BYTES`；ADR 0005 请求接受协议与 [ADR 0009](../../adr/0009-remove-legacy-lancet-tool-executor.md#4-会话库升版与存储收敛)。计量对象不是文件目录或 provider wire |
| 调用结果载荷 | Succeeded 载荷规范 JSON 最多 65,536 字节；超过时不保存正文，记录摘要与长度，内容不可用 | `lattice-session` 的 `MAX_TOOL_CONTENT_BYTES`；见[调用槽位](steps.md#调用槽位) |
| 有效 profile | `agent.toml` 与外部策略文件各最多 16 MiB，即 16,777,216 字节；system prompt 最多 2 MiB，Agent 名最多 128 字节，模型引用的 provider 部分最多 64 字节并按 `id` 规则校验、模型部分最多 4,096 字节；schema_version 为 1，策略已解析；max_retries ≤ 10，退避初始/最大值均 ≤ 60,000 毫秒且最大值不小于初始值，显式 timeout（Turn deadline）为 1–86,400 秒；无效时在接受前按配置错误拒绝 | `agent.toml` 加载与文件上限见 [load.rs 的 load_agent_profile、MAX_PROFILE_FILE_BYTES 与 MAX_SYSTEM_PROMPT_BYTES](../../../crates/lattice-profile/src/load.rs)，Agent 名、超时与重试策略的领域类型构造入口见 [model/mod.rs 的 AgentName::parse、TurnTimeout::new 与 RetryPolicy::new](../../../crates/lattice-session/src/model/mod.rs)，模型引用的领域类型构造入口见 [model_selection.rs 的 ModelRef::parse、ProviderId::parse 与 ModelId::parse](../../../crates/lattice-conversation/src/model_selection.rs)，外部策略见 [cep.rs 的 MAX_EXTERNAL_POLICY_FILE_BYTES](../../../crates/lattice-api/src/turn/cep.rs)。上限 86,400 秒由 ADR 0010 第 11 节确定，使超过 SIWC token 有效期的 Turn 成为实际用法；未设置时使用 300 秒，与观察 idle 分开；粒度与默认值由 #222 裁决 |
| provider 与 TLS 根 | key 见[模型接入合同的限制](../llm/README.md#限制)；模型名 ≤ 4,096 字节，base_url ≤ 8,192 字节；根证书 ≤ 1,024 个、单个 DER ≤ 65,536 字节、DER 总量 ≤ 4 MiB（过大或无法按 rustls 信任锚规则解析 DER 的单张跳过，不占名额也不计入总量；按加载顺序保留前缀，达到个数或总量上限即停止并记 WARN，之后的根证书不信任）；没有可用的根证书时 https 端点在接受前按配置错误拒绝，http 端点照常；接受时能发现的问题在接受前拒绝，执行中发现的按 `ProviderError { retryable: false }` 停止，诊断写明是配置问题 | 模型名上限与构造入口见 [model_selection.rs 的 ModelId::parse 与 MAX_MODEL_ID_BYTES](../../../crates/lattice-conversation/src/model_selection.rs)，base_url 上限见 [net-guard 的 MAX_BASE_URL_BYTES](../../../crates/net-guard/src/lib.rs)，根证书限制见 [client.rs 的 MAX_TLS_ROOTS 与 select_roots](../../../crates/lattice-llm/src/client.rs)；URL 禁止凭据、query、fragment 并通过 provider 网络策略校验 |
| 模型 HTTP 传输 | 建连 30 秒、读空闲 30 秒；不设单独的请求总时限，流由 Turn deadline 约束，另有 86,400 秒兜底，与 Turn deadline 的上限一致；非 2xx 正文最多读 64 KiB，诊断最多 256 字符；provider 建议等待最多采纳 60 秒。每个 Turn 第一次请求时解析、校验并固定地址，Turn 内复用连接：交付结束事件前最多再等 0.5 秒、读 64 KiB 把响应体读完，连接才能回到连接池；连接或传输错误后重新解析。请求除认证与协议头外只带 `User-Agent: lattice/<版本> (linux <内核版本>; <架构>)`、`Accept: text/event-stream, application/json` 与 `x-client-request-id`（取 SessionId，provider 由此可知哪些请求属于同一会话） | [client.rs](../../../crates/lattice-llm/src/client.rs)、[transport.rs](../../../crates/lattice-llm/src/transport.rs) 与 [saga.rs](../../../crates/lattice-api/src/turn/saga.rs) 的 MAX_PROVIDER_RETRY_AFTER；256 依据 #265 收集的 146 条真实 provider 错误消息（整行 P90 为 252 字符），其余为实现常量 |
| IPC 帧 | 单帧 JSON payload 最多 16 MiB，不含 8 字节 magic 和 4 字节长度头；零长度或超限读取拒绝，编码超限拒绝 | [ipc.rs](../../../crates/lattice-api/src/local/ipc.rs) 的 MAX_FRAME、read/write；超过单帧的消息由连接层分帧传输 |
| IPC 等待 | 单帧 socket 写入最多 5 秒，超时返回传输错误和回执可能未知；后台首请求整帧读取最多 10 秒，超时结束该连接处理。登录后续帧等待首字节不限时，首字节到达后须在 10 秒内完成该帧；人类授权等待不使用观察 idle 时限 | 同文件 WRITE_TIMEOUT、LOGIN_FRAME_TIMEOUT 和 [server.rs::connection](../../../crates/lattice-api/src/local/server.rs)，不是 Turn 总时限 |
| 后台资源槽 | 同时处理连接最多 64，满时关闭新接入连接；观察者许可最多 32，不足返回 Busy，`head` 等查询不占观察许可；已接管 Owner 最多 32，准入不足返回 Busy | server.rs 的 serve、Backend::new/claim、Owner::new。限制各自对象，观察者额度为查询/取消留连接空间，不承诺延迟 |
| 内存观察状态 | 每个正在执行的 Turn 一份当前尝试的段列表，受助手正文与 reasoning 上限约束，至多约 4 MiB，同时进行的 Turn 受 Owner 上限约束；每个观察者一份待推送条目的集合与文字的已发位置，受观察者许可约束 | 见[观察与重连](recovery.md#观察与重连)；段列表保存共同回复累积器发出的显示增量，与它组装的完整回复共享来源 |
| 客户端观察 idle | 当前位置那一轮有 Owner 执行时，300 秒没有任何推送即终止当前观察，交付连接诊断，保留已知结局；会话空闲时不计时；每次建连及收到推送重置期限 | [STREAM_IDLE_TIMEOUT_SECS](../../../crates/lattice-api/src/turn/execute.rs) 与 [LocalClient](../../../crates/lattice-api/src/local/client.rs)；与后台执行及收尾独立 |
| 客户端取消回执 | 每次 Cancel 最多等待 5 秒，超时为 Unknown；一次补发条件由[客户端意图与退出交接](recovery.md#客户端意图与退出交接)限定 | LocalTurn 的 drive_cancel；两个前端共用 |
| 首次取消时提交仍在途 | 从首次取消起等待提交回执 5 秒，到期报告提交结果不明，原提交任务继续接收迟到回执；提交连接断开后才按 TurnId 查询 | LocalTurn 的 run / handoff_submission；不是普通提交总时限，也不自动重发请求 |
| 取消后的退出交接 | detach 最多使用 5 秒交接控制回执，Taken 可提前返回；同一取消意图已完成上述未知交接且无在途 Cancel 时，不再空等第二个窗口 | LocalTurn 的 detach / run；普通退出不新增取消。这些是客户端等待预算，不承诺后台完成、阻塞配置 I/O 或 runtime shutdown 在五秒内结束 |

模型流限制见[模型请求与执行预算](acceptance.md#模型请求与执行预算)。帧、页和字段校验提供有界传输，不提供有界解析内存的额外保证。
