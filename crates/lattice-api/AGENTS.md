# lattice-api 工作指引

## 职责

提供 SessionApi、LocalClient、本地后台及公开的视图、查询与结果；转换冻结配置并驱动 TurnCoordinator 经会话库事务推进执行。Component 组成在 lancet-api，执行在 runtime；本 crate 拥有跨领域协调而非下层机制。

- `local/client.rs`：LocalClient 的连接与分帧，转交视图与查询。
- `local/operation.rs`：LocalTurn 是本客户端一次提交的具体句柄，管理提交、在途取消与退出交接；前端生成身份、加载配置并消费视图，持久 Turn 仍由后台执行。接口分层见[公开操作](../../docs/architecture/session.md#实现模块与公开操作)。
- `local/server.rs`：serve 持实例资源和连接；Backend 管准入、存储句柄、视图及本进程完成诊断，Owner 监督独立操作。接管、同一取消载体与独立收尾遵循[取消与退出](../../docs/architecture/session.md#取消与退出)。
- `session.rs`：公开会话操作、固定读取、ID 解析和结构化错误映射。
- `config_rejection.rs`、`redact.rs`：配置拒绝的说明只经一个入口构造，文字在其中脱敏并规范化为单行；`profile.rs`、`profile_paths.rs`：提交端共用的 agents 目录、profile 查找与名称一致性校验。规则见[配置拒绝的原因](../../docs/architecture/session.md#配置拒绝的原因)。
- `turn/live.rs`、`turn/execute.rs`：同一 live 核心及独立接受/完成结果；start_turn 用于后台执行接线，run_turn/stream_turn 仅供测试复用核心。
- `turn/cep.rs`：profile→policy、Tool 事件映射及生产依赖接线；首次共享 Component 组成经 spawn_blocking，组装仍归 lancet-api。
- coordinator/saga 模块：阻塞存储适配、可注入 IdSource、调用登记、顺序执行、槽位终态、Step/Turn 关闭及会话退回。
- `stores.rs`：共享会话库句柄与库级停写、排空和重开；`turn/observer.rs`：当前尝试段列表与每个观察者的推送状态。

## 上位文档

修改前读取[会话](../../docs/architecture/session.md)、[工作区](../../docs/architecture/workspace.md)、[存储](../../docs/architecture/storage.md)、[LANCET](../../docs/architecture/lancet.md)及 [Cell](../../docs/architecture/cell.md)合同。

## 局部约束

- TurnCoordinator 是唯一持久 Turn 协调者，使用窄领域接口持有全 Turn SessionExecutionLock；无第二 dispatch/schema/Component 组装路径或跨库事务。会话库写事务与工作区历史仓库锁不同时持有，也不同时持有两把仓库写锁。CLI 渲染与 provider wire 分别留在 CLI/LLM。
- 接受与找回遵循[请求身份与找回](../../docs/architecture/session.md#请求身份与找回)：`submit` 只接受新请求，找回经按 TurnId 查询。观察经会话视图交付，按观察者推送条目的最新值；背压不得阻塞 live task drain 或持久提交，条目和错误保持结构化。面向用户的等级与说明由 Step 条目中的重试提示与 `TurnSummary` 的 notice 给出，见[模型请求与执行预算](../../docs/architecture/session.md#模型请求与执行预算)。
- profile capability 转换将 enabled 作为第一阶段、capabilities 作减法；没有已加载 profile 时 fail closed。`cep_policy_from_profile` 只携带执行策略，不隐式授予准入能力。
- 阻塞 Git/store 调用用 spawn_blocking，写事务与仓库锁不跨 await。调用开始前先提交 Started，失败后不继续分发；commit 出错按[事务与一致性](../../docs/architecture/session.md#事务与一致性)停止全库写入和新 provider/Tool 启动，排空在途工作后关闭重开，不推定已回滚。
- 槽位结果由到达的阶段确定：停在 `Registered` 为 NotExecuted，停在 `Started` 为效果不确定，不猜 NotExecuted 或重分发；分发前取消、Deadline 等停止原因随 Step 关闭写入一次。
- 执行者消失的 Turn 不被继续或补写；关闭 Step 不请求模型、不重执行 Tool、不读取工作区历史，也不取得工作区操作锁。`rewind` 移到已有 NodeId 本身，保留原分叉，不恢复文件；根节点与 agent 选择按会话合同执行。
- Session ID 解析精确优先，再对 store.list 前缀匹配；歧义最多列 5 个候选并要求更多字符，拒绝空/全空白。规范 SessionId 仍是32位十六进制，缩写只属输入/显示。

## 验证入口

`cargo test -p lattice-api`。按变化关联 runtime、agent、session、CLI；覆盖提交、取消、读取与控制交接、观察切点、库级故障和执行者消失后的结局，文件关联由 workspace_history_acceptance 保护。被测产物与外部场景选择遵循[测试工程](../../docs/development/testing.md)。
