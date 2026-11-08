# lattice-api 工作指引

提供 `SessionApi`、`LocalClient`、本地后台服务与 `TurnCoordinator`，跨领域协调会话库、请求网关、profile 与工具运行时，被 `lattice-cli` 与 `lattice-tui` 调用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs`、`src/root.rs`、`src/models.rs` | 公开接口重导出、规范本地工作区根路径解析 `resolve_local_workspace_root`、模型候选类型重导出与日志初始化转发 |
| `src/session.rs` | `SessionApi` 会话列表、树形日志、退回、历史、用量、详情读取与 `SessionId` 精确/前缀解析 |
| `src/profile.rs`、`src/config_rejection.rs`、`src/redact.rs` | 委托 `lattice-profile` 加载配置后解析外部策略文件，经单一构造入口生成脱敏单行 `ConfigRejection` |
| `src/stores.rs` | 共享 `SessionStore` 句柄、提交异常后的库级停写、在途工作排空与关闭重开 |
| `src/local/mod.rs`、`src/local/types.rs`、`src/local/ipc.rs` | 本地控制面入口、IPC 消息定义、16 MiB 单帧分帧/拼回及 Unix socket 权限与对端 UID 核验 |
| `src/local/client.rs`、`src/local/operation.rs` | `LocalClient` 连接、自动启动后台、视图与查询转发、`stop` 收到回执后等待 socket 撤下与 `owner.lock` 释放；`LocalTurn` 提交、取消回执与 `detach` 退出交接 |
| `src/local/server.rs` | `serve` 实例资源与元数据任务启动、`Backend` 准入与视图推送、`Owner` 独立监督及有序 `stop` 收尾 |
| `src/local/auth.rs`、`src/local/auth/login.rs` | 后台凭据命令转发、敏感材料传输、错误映射与同连接 SIWC 登录句柄 |
| `src/local/metadata.rs` | `models.dev` 与 ChatGPT 账号目录的单任务调度、按来源共享在途刷新及停机排空 |
| `src/turn/mod.rs`、`src/turn/types.rs`、`src/turn/live.rs`、`src/turn/execute.rs` | Turn 准入（经 `GatewayTurn::prepare` 核对可用性与带来源标注的 `EffortPolicy`）、生产后台接线 `start_turn` 及 `test-utils` 复用的 `run_turn` |
| `src/turn/coordinator.rs`、`src/turn/saga.rs` | 全 Turn `SessionExecutionLock` 持有、`spawn_blocking` 存储适配、模型重试、顺序调用分发及 Step/Turn 关闭 |
| `src/turn/cep.rs` | profile 到 LANCET 准入与执行策略转换、首次共享 Component 的 `spawn_blocking` 组装及工具事件映射 |
| `src/turn/observer.rs` | 当前尝试的生成中文字段列表，以及各观察者待推节点与文字游标 |
| `src/turn/crash_barrier.rs` | `test-crash-barriers` feature 控制的提交后停点与观察调度插桩 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [会话与 Turn](../../docs/architecture/session/README.md)、[实现模块与公开操作](../../docs/architecture/session/operations.md)、[工程规范](../../docs/development/engineering.md) |
| 提交准入、配置冻结、脱敏与重试预算 | [Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md)、[会话的 profile 副本](../../docs/architecture/session/settings.md) |
| Step 事务、调用槽位、锁与故障停写 | [Step 执行与结果保存](../../docs/architecture/session/steps.md)、[会话存储与并发](../../docs/architecture/session/concurrency.md)、[私有存储与持久化](../../docs/architecture/storage.md#锁与并发语义) |
| 找回、视图观察、取消与后台 `stop` | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md) |
| 模型候选、元数据调度与凭据交换 | [模型接入](../../docs/architecture/llm/README.md)、[模型引用与元数据](../../docs/architecture/llm/models.md#元数据的来源与合并)、[凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md) |
| 工具准入、Component 接线与工作区关联 | [Cell 权限与证据](../../docs/architecture/cell.md#调用准入)、[LANCET 工具执行](../../docs/architecture/lancet.md)、[本地工作区与变更历史](../../docs/architecture/workspace.md)、[测试工程](../../docs/development/testing.md) |

## 约束

- `TurnCoordinator` 作为唯一的持久 Turn 协调者持有全 Turn `SessionExecutionLock`，复用单一的分发、schema 与 Component 组装路径；阻塞式会话库、Git 与首次共享 Component 组装均放入 `spawn_blocking`，按[锁与并发语义](../../docs/architecture/storage.md#锁与并发语义)保持会话库写事务与工作区历史仓库锁在同步闭包内独立获取、互不重叠。
- `submit` 只接受新请求，找回统一走按 `TurnId` 查询；`observer.rs` 为每个观察者维护最新待推条目与文字游标并以非阻塞通知解耦观察背压、实时排空与持久提交；Step 重试提示与 `TurnSummary` notice 按[模型请求与执行预算](../../docs/architecture/session/acceptance.md#模型请求与执行预算)生成，接受与观察交接见[请求身份与找回](../../docs/architecture/session/recovery.md#请求身份与找回)与[观察与重连](../../docs/architecture/session/recovery.md#观察与重连)。
- `local/server.rs` 的 `stop` 先取消元数据刷新任务并调用 `stop_logins`，待已接管 Turn 收尾后再关闭认证生命周期并排空连接，交接顺序见[取消与退出](../../docs/architecture/session/recovery.md#取消与退出)与[停止后台](../../docs/architecture/session/recovery.md#停止后台)。
- `cep.rs` 以 `[tools].enabled` 为第一阶段、叠加 `[lancet.capabilities]` 减法构造准入策略，缺失已加载 profile 时默认拒绝，`cep_policy_from_profile` 只转换执行策略，见[调用准入](../../docs/architecture/cell.md#调用准入)。
- 调用分发前先提交 `Started`，槽位终态与 Step 停止原因严格由已保存的阶段单次推导；写事务提交失败时经 `stores.rs` 立即停写全库、排空在途工作后关闭重开，见[调用槽位](../../docs/architecture/session/steps.md#调用槽位)与[事务与一致性](../../docs/architecture/session/concurrency.md#事务与一致性)。
- 执行者消失后的 Turn 保留原持久状态，`rewind` 只将当前节点指针移到目标 `NodeId` 并保留原分叉、工作区文件与会话 profile 副本，见[执行者消失后的 Turn](../../docs/architecture/session/recovery.md#执行者消失后的-turn)与[会话的 profile 副本](../../docs/architecture/session/settings.md#会话的-profile-副本)。
- `session.rs` 解析 `SessionId` 时先精确匹配再按已存前缀匹配，歧义时最多列出 5 个候选并拒绝空白输入，内部与持久层始终使用 32 位十六进制规范 `SessionId`。
- 本 crate 之外：profile 字段与目录权限校验由 `lattice-profile` 负责；会话库表与事务原语由 `lattice-session` 负责；provider wire 协议与认证生命周期由 `lattice-llm` 负责；Component 组装与执行分别由 `lancet-api` 和 `lancet-runtime` 负责；终端与命令行呈现由 `lattice-tui` 和 `lattice-cli` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-api`（含提交、取消交接、视图切点、库级故障、执行者消失及 `tests/workspace_history_acceptance.rs`） |
| 修改 Turn 协调、流消费或工具接线 | `cargo test -p lattice-agent`、`cargo test -p lattice-session`、`cargo test -p lancet-runtime` |
| 修改 `LocalClient`、IPC 或公开 DTO | `cargo test -p lattice-cli`、`cargo test -p lattice-tui`，并按[测试工程](../../docs/development/testing.md)运行受影响的 `lattice-system-harness` 场景 |
