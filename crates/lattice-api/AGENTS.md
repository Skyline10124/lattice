# lattice-api 工作指引

## 职责

提供 SessionApi、LocalClient、本地后台及公开事件/结果；转换冻结配置并驱动 TurnCoordinator 经会话库事务推进执行。Component 组成在 lancet-api，执行在 runtime；本 crate 拥有跨领域协调而非下层机制。

- `local/client.rs`：LocalClient 的连接、共同惰性握手及固定历史读取。
- `local/operation.rs`：LocalTurn 是本客户端一次提交或观察交互的具体句柄，管理交付、在途取消、观察、正文任务和退出交接；前端提供意图与配置来源并消费更新，持久 Turn 仍由后台执行。接口分层见[公开操作](../../docs/architecture/session.md#实现模块与公开操作)。
- `local/server.rs`：serve 持实例资源和连接；Backend 管准入、存储句柄及本进程完成诊断，Owner 监督独立操作。接管、同一取消载体与独立收尾遵循[取消与退出](../../docs/architecture/session.md#取消与退出)。
- `session.rs`：公开会话操作、固定读取、ID 解析和结构化错误映射。
- `turn/live.rs`、`turn/execute.rs`：同一 live 核心及独立接受/完成结果；start_turn 用于后台执行接线，run_turn/stream_turn 仅供测试复用核心。
- `turn/cep.rs`：profile→policy、Tool 事件映射及生产依赖接线；首次共享 Component 组成经 spawn_blocking，组装仍归 lancet-api。
- coordinator/saga 模块：阻塞存储适配、可注入 IdSource、调用登记、顺序执行、槽位终态、Step/Turn 关闭及会话退回。
- `stores.rs`：共享会话库句柄与库级停写、排空和重开；`turn/observer.rs`：事件窗口与当前尝试段列表。

## 上位文档

修改前读取[会话](../../docs/architecture/session.md)、[工作区](../../docs/architecture/workspace.md)、[存储](../../docs/architecture/storage.md)、[LANCET](../../docs/architecture/lancet.md)及 [Cell](../../docs/architecture/cell.md)合同。

## 局部约束

- TurnCoordinator 是唯一持久 Turn 协调者，使用窄领域接口持有全 Turn SessionExecutionLock；无第二 dispatch/schema/Component 组装路径或跨库事务。会话库写事务与工作区历史仓库锁不同时持有，也不同时持有两把仓库写锁。CLI 渲染与 provider wire 分别留在 CLI/LLM。
- 接受、同请求找回遵循[请求身份与找回](../../docs/architecture/session.md#请求身份与找回)，只读重连使用 query/subscribe。观察按快照、按序事件与结束交付；背压不得阻塞 live task drain 或持久提交，公开事件保留 TurnId，事件和错误保持结构化。
- profile capability 转换将 enabled 作为第一阶段、capabilities 作减法；没有已加载 profile 时 fail closed。`cep_policy_from_profile` 只携带执行策略，不隐式授予准入能力。
- 阻塞 Git/store 调用用 spawn_blocking，写事务与仓库锁不跨 await。调用开始前先提交 Started，失败后不继续分发；commit 出错按[事务与一致性](../../docs/architecture/session.md#事务与一致性)停止全库写入和新 provider/Tool 启动，排空在途工作后关闭重开，不推定已回滚。
- live 与自动关闭共用有序槽位终态规则；分发前取消、Deadline 和未执行原因随余下槽位与 Step/Turn 关闭同事务保存，Started 无终态按效果不确定处理，不猜 NotExecuted 或重分发。
- 自动关闭只消费持久事实，不请求模型、重执行 Tool、读取工作区历史或取得工作区操作锁。`rewind` 移到已有 NodeId 本身，保留原分叉，不恢复文件；根节点与 agent 选择按会话合同执行。
- Session ID 解析精确优先，再对 store.list 前缀匹配；歧义最多列 5 个候选并要求更多字符，拒绝空/全空白。规范 SessionId 仍是32位十六进制，缩写只属输入/显示。

## 验证入口

`cargo test -p lattice-api`。按变化关联 runtime、agent、session、CLI；覆盖提交、取消、读取与控制交接、观察切点、库级故障和 closure-only，文件关联由 workspace_history_acceptance 保护。被测产物与外部场景选择遵循[测试工程](../../docs/development/testing.md)。
