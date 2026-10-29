# lattice-api 工作指引

## 职责

提供 SessionApi、LocalClient、本地后台及公开的视图、查询与结果；读取并冻结 profile、维护会话的 profile 副本、经 `lattice-llm` 的请求网关发出模型请求、承接凭据与模型命令，并驱动 TurnCoordinator 经会话库事务推进执行。Component 组成在 lancet-api，执行在 runtime；本 crate 拥有跨领域协调而非下层机制。

- `local/client.rs`：LocalClient 的连接与分帧，转交视图与查询。
- `local/auth.rs`：后台凭据目录、录入材料与凭据错误映射。录入材料只随请求帧进入后台。
- `local/metadata.rs`：模型元数据的定时与手动刷新。默认每 24 小时刷新：启动时缓存不足 24 小时则等到期再拉取，否则立即拉取。每次触发都读 `[metadata] auto_refresh`，关闭时跳过本次；配置无效或拉取失败时从 1 分钟起翻倍退避重试，上限 1 小时。手动刷新与定时任务共享同一次在途操作。停止信号只结束刷新任务。
- `local/operation.rs`：LocalTurn 是本客户端一次提交的具体句柄，管理提交、在途取消与退出交接；前端生成身份、交出 agent、模型与推理档位的选择并消费视图，配置由后台读取，持久 Turn 仍由后台执行。接口分层见[公开操作](../../docs/architecture/session.md#实现模块与公开操作)。
- `local/server.rs`：serve 持实例资源和连接，并在启动时拉起元数据刷新任务。Backend 管准入、存储句柄、视图及本进程完成诊断，Owner 监督独立操作。`stop` 先取消刷新任务，再等待已接管的 Turn 收尾。接管、同一取消载体与独立收尾遵循[取消与退出](../../docs/architecture/session.md#取消与退出)。
- `models.rs`：公开模型候选类型，并转发日志初始化。
- `session.rs`：公开会话操作、固定读取、ID 解析和结构化错误映射。
- `config_rejection.rs`、`redact.rs`：配置拒绝的说明只经一个入口构造，文字在其中脱敏并规范化为单行；`profile.rs`：后台按项目根与启动时的配置目录查找 profile、校验名称一致性、加载系统提示并冻结有效策略。规则见[配置拒绝的原因](../../docs/architecture/session.md#配置拒绝的原因)。
- `turn/live.rs`、`turn/execute.rs`：同一 live 核心及独立接受/完成结果；start_turn 用于后台执行接线，run_turn/stream_turn 仅供测试复用核心。
- `turn/cep.rs`：profile→policy、Tool 事件映射及生产依赖接线；首次共享 Component 组成经 spawn_blocking，组装仍归 lancet-api。
- coordinator/saga 模块：阻塞存储适配、可注入 IdSource、调用登记、顺序执行、槽位终态、Step/Turn 关闭及会话退回。
- `stores.rs`：共享会话库句柄与库级停写、排空和重开；`turn/observer.rs`：当前尝试已生成的正文与 reasoning（段列表，视图从中推送生成中的文字），以及每个观察者待推送的条目与已发文字的位置。

## 上位文档

修改前读取[会话](../../docs/architecture/session.md)、[模型接入](../../docs/architecture/llm.md)、[工作区](../../docs/architecture/workspace.md)、[存储](../../docs/architecture/storage.md)、[LANCET](../../docs/architecture/lancet.md)及 [Cell](../../docs/architecture/cell.md)合同。

## 局部约束

- TurnCoordinator 是唯一持久 Turn 协调者，使用窄领域接口持有全 Turn SessionExecutionLock；无第二 dispatch/schema/Component 组装路径或跨库事务。会话库写事务与工作区历史仓库锁不同时持有，也不同时持有两把仓库写锁。CLI 渲染与 provider wire 分别留在 CLI/LLM。
- 接受与找回遵循[请求身份与找回](../../docs/architecture/session.md#请求身份与找回)：`submit` 只接受新请求，找回经按 TurnId 查询。观察经会话视图交付，按观察者推送条目的最新值；背压不得阻塞 live task drain 或持久提交，条目和错误保持结构化。面向用户的等级与说明由 Step 条目中的重试提示与 `TurnSummary` 的 notice 给出，见[模型请求与执行预算](../../docs/architecture/session.md#模型请求与执行预算)。
- profile capability 转换将 enabled 作为第一阶段、capabilities 作减法；没有已加载 profile 时 fail closed。`cep_policy_from_profile` 只携带执行策略，不隐式授予准入能力。
- 阻塞 Git/store 调用用 spawn_blocking，写事务与仓库锁不跨 await。调用开始前先提交 Started，失败后不继续分发；commit 出错按[事务与一致性](../../docs/architecture/session.md#事务与一致性)停止全库写入和新 provider/Tool 启动，排空在途工作后关闭重开，不推定已回滚。
- 槽位结果由到达的阶段确定：停在 `Registered` 为 NotExecuted，停在 `Started` 为效果不确定，不猜 NotExecuted 或重分发；分发前取消、Deadline 等停止原因随 Step 关闭写入一次。
- 执行者消失的 Turn 不被继续或补写；关闭 Step 不请求模型、不重执行 Tool、不读取工作区历史，也不取得工作区操作锁。`rewind` 移到已有 NodeId 本身，保留原分叉，不恢复文件，也不改变 profile 副本；agent 的选择与副本的写入按[会话的 profile 副本](../../docs/architecture/session.md#会话的-profile-副本)执行。
- Session ID 解析精确优先，再对 store.list 前缀匹配；歧义最多列 5 个候选并要求更多字符，拒绝空/全空白。规范 SessionId 仍是32位十六进制，缩写只属输入/显示。

## 验证入口

- 运行 `cargo test -p lattice-api`。测试要覆盖提交、取消、读取与控制的交接、观察切点、库级故障和执行者消失后的结局；与工作区文件的关联由 `workspace_history_acceptance` 保护。
- 按改动的影响，再检查 runtime、agent、session 与 CLI 的测试。
- 被测产物与外部场景的选择遵循[测试工程](../../docs/development/testing.md)。
