# lattice-local 工作指引

提供本地前端客户端句柄、会话观察状态机、SIWC 登录客户端、请求身份生成与 stderr 日志初始化，作为 `lattice-cli` 与 `lattice-tui` 访问本地后台的唯一扁平客户端门面。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 扁平显式重导出客户端句柄、`init_logging`、`new_session_id`、`new_turn_id` 及前端所需的 `lattice-ipc`、`lattice-conversation` 与 `StateLayout` 类型，以及 `test-utils` 下的 `Departure` 与 `test_support` |
| `src/endpoint.rs` | 本地 Unix socket 端点路径校验、客户端建连与对端有效 UID 核验，以及等待 socket 撤下与 `owner.lock` 释放的 `Departure` |
| `src/client.rs` | `LocalClient`（自动启动后台、单次请求/查询、`stop` 后等待 `Departure`、`auth_login` 握手）与 `LocalWatch`（持续读取任务、观察流驱动与按 `STREAM_IDLE_TIMEOUT_SECS` 的有执行者观察 idle 判定） |
| `src/operation.rs` | `LocalTurn`（提交请求、取消意图登记、5 秒回执等待与一次补发、按 `TurnId` 找回及 `detach` 退出交接）、`DetachReport`、`ConfigureOutcome` 与 `STREAM_IDLE_TIMEOUT_SECS`（`300` 秒） |
| `src/login.rs` | `LocalAuthLogin`（持续读取任务保留半帧进度、发送 `callback`/`cancel`、Drop 中止读任务并清零缓冲）、`AuthLoginEvent` 与只提供 `new` / `expose` 的 `SensitiveLoginUrl` |
| `src/logging.rs` | `init_logging`：将日志输出到 stderr，依据交互终端状态与 `NO_COLOR` 控制 ANSI 颜色，保持 stdout 专供结构化输出 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [Crate 职责与依赖边界](../../docs/architecture/crates.md)、[实现模块与公开操作](../../docs/architecture/session/operations.md)、[工程规范](../../docs/development/engineering.md) |
| 连接寻址、自动拉起后台与 `Departure` | [私有存储与持久化](../../docs/architecture/storage.md#本地后台与访问边界)、[停止后台](../../docs/architecture/session/recovery.md#停止后台) |
| `LocalWatch`、`LocalTurn`、取消交接与观察 idle | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md)、[会话存储与并发](../../docs/architecture/session/concurrency.md#输入与传输限制) |
| `LocalAuthLogin` 与 `SensitiveLoginUrl` | [凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md#凭据命令)、[凭据命令与私有交换](../../docs/architecture/session/operations.md#凭据命令与私有交换) |

## 约束

- `Cargo.toml` 的 `dependencies` 与 `build-dependencies` 中，workspace 内部成员仅允许 `lattice-ipc`、`lattice-conversation`、`lattice-store`；普通依赖闭包中禁止出现 `lattice-server`、`lattice-llm`、`lattice-profile`、`lattice-agent`、`lattice-im`，由 `cargo xtask crate-boundary` 门禁强制检查。
- `src/lib.rs` 保持全部内部子模块私有与扁平显式重导出列表，不使用通配符 `*`，也不向门面重导出 `Request`、`Response`、`AuthLoggedOut`、`LoginAction`、`LoginResponse` 或 `AuthKey`；其他 crate 用不到的自有函数、方法与常量不公开，仅供测试复用的 `LocalWatch`、`Departure`、`test_support` 与底层客户端/重连方法收窄在 `test` 或 `test-utils` 下；前端经 `LocalTurn` 观察会话。
- `SensitiveLoginUrl` 内部持有 `Zeroizing<String>`，只提供 `new(String)` 与 `expose(&self) -> &str`，不实现 `Debug`、`Display`、`Serialize`、`Deref` 或隐式转换；`LocalWatch` 与 `LocalAuthLogin` 的持续读取任务保留同一连接的半帧进度，避免界面单次轮询取消丢失数据，并在 Drop 时中止读任务。
- 工作区路径由调用方以规范绝对路径传入（CLI 与 TUI 传 `current_dir()`），本 crate 原样转发；后台规范化后核对结果与传入值相同，再校验项目绑定；实例锁（`owner.lock`）持有、socket 绑定与清理时序、`ApiError` 映射、模型/凭据展示转换与持久 Turn 协调由 `lattice-server` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-local` 与 `cargo clippy -p lattice-local --all-targets -- -D warnings` |
| 修改 `Cargo.toml` 依赖或 `test-utils` feature | `cargo xtask crate-boundary` 与 `cargo xtask harness-boundary` |
| 修改客户端句柄、观察流、登录流或门面导出 | `cargo test -p lattice-cli -p lattice-tui`、`cargo test -p lattice-server` |
