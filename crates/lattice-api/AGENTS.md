# lattice-api 工作指引

## 职责

提供 SessionApi、LocalClient、本地后台及公开事件/结果；转换冻结配置并驱动 journal-backed TurnCoordinator。Component 组成在 lancet-api，执行在 runtime；本 crate 拥有跨领域协调而非下层机制。

- `local/`：本地 IPC、任务接管、独立接受/完成回执、查询/订阅、取消、恢复与排空。
- `session.rs`：公开会话操作、固定读取、ID 解析和结构化错误映射。
- `turn/live.rs`、`turn/execute.rs`：同一 live 核心及独立接受/完成结果，stream_turn/run_turn 复用该核心。
- `turn/cep.rs`：profile→policy、Tool 事件映射及生产依赖接线；首次共享 Component 组成经 spawn_blocking，组装仍归 lancet-api。
- coordinator/saga 模块：阻塞存储适配、可注入 IdSource、日志、绑定、预留、顺序执行、结果材料、终态和最终发布。
- `turn/invocation_facts.rs`：pinned audit ledger → session pending facts，避免 audit→session 生产依赖。
- RewindCoordinator 单独处理显式恢复，不把 PendingTurn 当 Rewind 崩溃日志。

## 上位文档

修改前读取[会话](../../docs/architecture/session.md)、[工作区](../../docs/architecture/workspace.md)、[存储](../../docs/architecture/storage.md)、[LANCET](../../docs/architecture/lancet.md)及 [Cell](../../docs/architecture/cell.md)合同。

> 本入口的后台相关修订待人类批准；生效前原有效版本继续适用，见[修订状态](../../docs/README.md#工作分支修订状态)。

## 局部约束

- TurnCoordinator 是唯一 Turn saga 所有者，使用窄领域接口持有全 Turn SessionExecutionLock；无第二 dispatch/schema/Component 组装路径或跨库事务，不同时持有两把仓库写锁。CLI 渲染与 provider wire 分别留在 CLI/LLM。
- 接受/完成确认、同请求重试、固定读取与 Gap 遵循会话合同；只读操作与可能补屏障的 `lookup_or_recover` 分开接线。观察背压不得阻塞 live task drain 或持久提交，事件和错误保持结构化。
- profile capability 转换将 enabled 作为第一阶段、capabilities 作减法；没有已加载 profile 时 fail closed。`cep_policy_from_profile` 只携带执行策略，不隐式授予准入能力。
- 阻塞 Git/store 调用用 spawn_blocking。live/reopen 共享累积 transcript 和候选基础；纯 reducer 不采样工作区。runtime drain 后在共享操作锁内读取 current、补全 checkpoint、发布 TurnClosing 和最终会话引用；错误或 malformed OID 不替换为 Empty，重开 pinned/current 不符拒绝。
- 准入不能证明 pre-dispatch 时保持 StepClosing 并报告 RecoveryRequired；不猜 NotExecuted，也不重分发。分发前取消关闭为 CancelledBeforeDispatch，按序关闭剩余绑定并记录 CancelTurn；DispatchStarted 后取消按持久 Cancelled 处理。
- `rewind_to_turn` 先拒绝不存在、外会话、不可达或 checkpoint 不可用目标，恢复工作区后才移动选中会话引用并 pin 被放弃头。冲突与部分持久失败按工作区合同报告；SessionApi 普通引用操作不恢复文件。
- Session ID 解析精确优先，再对 store.list 前缀匹配；歧义最多列 5 个候选并要求更多字符，拒绝空/全空白。规范 SessionId 仍是32位十六进制，缩写只属输入/显示。
- XDG 测试通过 lancet-audit 的 test-support dev-dependency 使用唯一 ScopedXdgStateHome，不建立本地互斥副本。

## 验证入口

`cargo test -p lattice-api`。按变化关联 runtime、agent、session、CLI；streaming 覆盖 live事件、collector、失败、取消、提交及背压，checkpoint/Rewind 覆盖对应 acceptance。被测产物与外部场景选择遵循[测试工程](../../docs/development/testing.md)。
