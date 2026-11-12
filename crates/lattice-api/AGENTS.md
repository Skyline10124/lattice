# lattice-api 工作指引

提供 `SessionApi`、`LocalClient`、本地后台服务、`TurnCoordinator` 与 `ToolExecutor` 接缝，跨领域协调会话库、请求网关、profile 与工具执行器，被 `lattice-cli` 与 `lattice-tui` 调用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs`、`src/root.rs`、`src/models.rs` | 公开接口重导出、规范本地工作区根路径解析 `resolve_local_workspace_root`、`availability_diagnostic` 诊断还原入口、IPC 模型展示类型重导出与日志初始化转发 |
| `src/session.rs` | `SessionApi` 会话列表、树形日志、退回、历史、用量、详情读取与 `SessionId` 精确/前缀解析 |
| `src/configure.rs`、`src/profile.rs`、`src/config_rejection.rs`、`src/redact.rs` | 会话设置与档位选择纯函数 `resolve_settings`，委托 `lattice-profile` 加载配置并转换为有效 profile，经单一构造入口生成脱敏单行 `ConfigRejection` |
| `src/stores.rs` | 共享 `SessionStore` 句柄、提交异常后的库级停写与关闭重开、以及排空超时停写终态（`disable_writes`） |
| `src/local/mod.rs`、`src/local/types.rs`、`src/local/ipc.rs` | 本地控制面公开门面显式重导出、不可序列化的客户端结果（`DetachReport`、`ConfigureOutcome`）、`InstanceLock` 实例锁句柄与 socket 绑定/清理/`Departure` 等待（分帧与对端 UID 核验委托 `lattice-ipc`） |
| `src/local/error.rs`、`src/local/presentation.rs` | `ApiError` 到 `LocalError` 的分类与安全存储诊断映射；后台边界 `lattice-llm` 与 `lattice-ipc` 模型查询、模型候选及凭据状态的逐字段无通配符转换 |
| `src/local/client.rs`、`src/local/operation.rs` | `LocalClient` 连接、自动启动后台、`configure` 与视图/查询转发、`stop` 收到回执后等待 socket 撤下与 `owner.lock` 释放；`LocalTurn` 提交、取消回执与 `detach` 退出交接 |
| `src/local/server.rs` | `serve` 实例资源与元数据任务启动、`Backend` 会话配置（`configure`）与草稿清理（`cleanup_drafts`）、提交准入、视图推送、`Owner` 独立监督，以及信号与 IPC `stop` 共用的有界停机顺序（返回 `ServeExit`） |
| `src/local/auth.rs`、`src/local/auth/login.rs` | 后台凭据命令存储调用、错误映射，以及同连接 SIWC 登录句柄 `LocalAuthLogin` 与后台登录交换（登录成功帧经 `presentation` 转换 `ProviderAuthStatus`） |
| `src/local/metadata.rs` | `models.dev` 与 ChatGPT 账号目录的单任务调度、按来源共享在途刷新及停机排空 |
| `src/turn/mod.rs`、`src/turn/types.rs`、`src/turn/live.rs`、`src/turn/execute.rs` | Turn 准入（按会话设置与 `expected_settings` 核对版本、经 `GatewayTurn::prepare` 严格核对可用性与已存档位、接受前调用 `ToolExecutor::open_turn` 冻结本轮工具目录）、生产后台接线 `start_turn` 及 `test-utils` 复用的 `run_turn` |
| `src/turn/coordinator.rs`、`src/turn/saga.rs` | 全 Turn `SessionExecutionLock` 持有、`spawn_blocking` 存储适配、模型重试、顺序调用分发及 Step/Turn 关闭 |
| `src/turn/tools.rs` | `ToolExecutor` / `TurnTools` / `ToolSpec` / `ToolOutcome` 工具执行接缝、生产默认注入 `NoTools`，以及 `test-fake-tools` feature 下的假执行器 `FakeToolExecutor`（按后台启动环境 `LATTICE_TEST_TOOLS` 启用） |
| `src/turn/observer.rs` | 当前尝试的生成中文字段列表，以及各观察者待推节点与文字游标 |
| `src/turn/crash_barrier.rs` | `test-crash-barriers` feature 控制的提交后停点、基于 Step/槽位标识的 marker 写入与观察调度插桩 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [Crate 职责与依赖边界](../../docs/architecture/crates.md)、[会话与 Turn](../../docs/architecture/session/README.md)、[实现模块与公开操作](../../docs/architecture/session/operations.md)、[工程规范](../../docs/development/engineering.md) |
| 提交准入、配置冻结、脱敏与重试预算 | [Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md)、[会话设置](../../docs/architecture/session/settings.md) |
| Step 事务、调用槽位、锁与故障停写 | [Step 执行与结果保存](../../docs/architecture/session/steps.md)、[会话存储与并发](../../docs/architecture/session/concurrency.md)、[私有存储与持久化](../../docs/architecture/storage.md#锁与并发语义) |
| 找回、视图观察、取消与后台 `stop` | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md) |
| 模型候选、展示转换、元数据调度与凭据交换 | [Crate 职责与依赖边界](../../docs/architecture/crates.md#类型归属与门面传递)、[模型接入](../../docs/architecture/llm/README.md)、[模型引用与元数据](../../docs/architecture/llm/models.md#元数据的来源与合并)、[凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md) |
| 工具执行接缝、目录冻结与测试假工具 | [Step 执行与结果保存](../../docs/architecture/session/steps.md#调用槽位)、[Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#执行配置冻结)、[ADR 0009](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md)、[测试工程](../../docs/development/testing.md) |

## 约束

- `TurnCoordinator` 作为唯一的持久 Turn 协调者持有全 Turn `SessionExecutionLock`，复用单一的模型驱动与工具分发路径；阻塞式会话库访问放入 `spawn_blocking`，写事务内不等待模型、工具执行器或网络。
- `submit` 只接受新请求，找回统一走按 `TurnId` 查询；`observer.rs` 为每个观察者维护最新待推条目与文字游标并以非阻塞通知解耦观察背压、实时排空与持久提交；Step 重试提示与 `TurnSummary` notice 按[模型请求与执行预算](../../docs/architecture/session/acceptance.md#模型请求与执行预算)生成，接受与观察交接见[请求身份与找回](../../docs/architecture/session/recovery.md#请求身份与找回)与[观察与重连](../../docs/architecture/session/recovery.md#观察与重连)。
- `local/server.rs` 的 `configure(New)` 在请求处理完毕并尝试写回响应后（无论请求成功或被拒、响应写出成功或失败）触发受后台监督的草稿会话清理，并在准入锁边界与 `watch` 观察者登记串行，见[会话状态与草稿清理](../../docs/architecture/session/concurrency.md#会话状态与草稿清理)；`serve_with_backend` 作为唯一的停机执行点处理信号与 IPC `stop`、单次持锁排空判定、超时停写、尾段连接服务与收尾及阻塞线程最终释放（最终释放与撤下 socket 共用第 6 步期限，经 `ServeExit` 把实例锁与该期限交给进程入口），见[取消与退出](../../docs/architecture/session/recovery.md#取消与退出)与[停止后台](../../docs/architecture/session/recovery.md#停止后台)。
- `local/presentation.rs` 用无通配符的穷尽 `match` 与完整结构体解构完成 `lattice-llm` 与 `lattice-ipc` 展示类型的逐字段转换，不经 JSON 往返，并在脱敏之后再转换模型候选，其单元测试对手写的目标契约值覆盖全部枚举分支与结构体的 `full`/`minimal` 形态；公开门面显式重导出前端所需类型，不向门面暴露 `Request`、`Response`、登录帧或 `AuthKey`。
- `ToolExecutor` 经构造参数注入后台与执行入口，不使用全局变量：生产默认注入空目录的 `NoTools`（发给模型的请求不带工具定义）；开启 `test-fake-tools` feature 的构建仅在后台启动环境设置了 `LATTICE_TEST_TOOLS` 时启用 `FakeToolExecutor`，未设置时仍使用 `NoTools`。接受事务前调用 `open_turn` 冻结本轮工具目录，协调层用同一份目录驱动模型请求与 `InvalidBatch` 批次校验。
- 调用分发前先检查取消与本轮 deadline，提交 `start_invocation` 后调用 `TurnTools::execute` 并传入同一取消标志与 deadline：`ToolOutcome::Deadline` 不调用 `finish_invocation`，直接以 `DispatchStop::Deadline` 关闭 Step（槽位保持 `Started`，读出 `Indeterminate(NoResult)`）；其余四种结果映射为 `ToolReply` 写入终态。写事务提交失败时经 `stores.rs` 停写并排空重开，排空超时停机时经 `StoreManager::disable_writes` 停写并保留库句柄至最终释放，见[调用槽位](../../docs/architecture/session/steps.md#调用槽位)、[事务与一致性](../../docs/architecture/session/concurrency.md#事务与一致性)与[停止后台](../../docs/architecture/session/recovery.md#停止后台)。
- 执行者消失后的 Turn 保留原持久状态，`rewind` 只将当前节点指针移到目标 `NodeId` 并保留原分叉、工作区文件与会话设置，见[执行者消失后的 Turn](../../docs/architecture/session/recovery.md#执行者消失后的-turn)与[会话设置](../../docs/architecture/session/settings.md#设置与一轮的配置)。
- `session.rs` 解析 `SessionId` 时先精确匹配再按已存前缀匹配，歧义时最多列出 5 个候选并拒绝空白输入，内部与持久层始终使用 32 位十六进制规范 `SessionId`。
- 本 crate 之外：IPC 协议定义、模型与凭据展示类型、版本魔数、单帧编解码、对端 UID 核验与线上形状快照由 `lattice-ipc` 负责；profile 字段与目录权限校验由 `lattice-profile` 负责；会话库表与事务原语由 `lattice-session` 负责；provider wire 协议与认证生命周期由 `lattice-llm` 负责；终端与命令行呈现由 `lattice-tui` 和 `lattice-cli` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-api` 与 `cargo test -p lattice-api --features test-fake-tools,test-crash-barriers`（含提交、取消交接、视图切点、库级故障、执行者消失、假工具执行及 `tests/session_without_workspace_history.rs`） |
| 修改 Turn 协调、流消费或工具接线 | `cargo test -p lattice-agent`、`cargo test -p lattice-session` |
| 联动修改 `lattice-ipc` 请求、响应、登录帧或展示类型 | `env -u LATTICE_UPDATE_WIRE_SHAPE cargo test -p lattice-ipc wire_shape`（见 [`crates/lattice-ipc/AGENTS.md`](../lattice-ipc/AGENTS.md)） |
| 修改 `LocalClient`、展示转换或公开门面重导出 | `cargo test -p lattice-cli`、`cargo test -p lattice-tui`，并按[测试工程](../../docs/development/testing.md)运行受影响的 `lattice-system-harness` 场景 |
