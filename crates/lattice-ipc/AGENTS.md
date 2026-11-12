# lattice-ipc 工作指引

定义本地 IPC 的请求、响应、登录帧、错误与模型/凭据展示类型，提供 `PROTOCOL_VERSION` 版本魔数、16 MiB 单帧分帧编解码、基于 `rustix` 的有效用户与对端 UID 核验，以及线上 JSON 形状快照门禁，被 `lattice-api` 使用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 显式重导出 IPC 协议类型、模型与凭据展示类型，以及线上消息引用的 `lattice-conversation` 与 `lattice-session` 只读领域值类型 |
| `src/types.rs` | 分页常量（`WATCH_TURNS`、`HISTORY_TURNS`、`MAX_HISTORY_TURNS`）、`Request` / `Response`、会话配置与提交载荷、视图更新、`LocalError` / `RequestConflict`（`ConflictReason`）与恢复/存储诊断、`TurnRef` / `TextSegment` 及 Unix 路径原始字节 serde |
| `src/models.rs` | `ModelCandidateQuery` 请求类型，以及 `ModelCandidates`、`ModelCandidate`、`ModelPrice`、`ModelWarning`、`Availability`（含 `status_str`）、`ConfigurationRequirement`（含 `as_str` 与 `Display`）展示类型 |
| `src/auth.rs` | 透明清零且不实现 `Debug` 的 `AuthKey`、`AuthStatus` / `ProviderAuthStatus` / `RevocationStatus` 展示类型，以及专用登录连接帧 `LoginAction` / `LoginResponse` / `AuthLoginOutcome` / `AuthCatalogOutcome` |
| `src/ipc.rs` | `PROTOCOL_VERSION`（`63`）与帧魔数、`MAX_FRAME`（16 MiB）单帧读写与响应分片拼回、响应缓冲清零、`rustix` 有效 UID 读取与 `socket_peercred` 对端核验，以及 `test-utils` 下的整帧读取辅助 `read` |
| `src/wire_shape/`、`src/wire_shape.json` | 本地 IPC 闭包内全部枚举变体、结构体 `full`/`minimal` 形态与场景帧的逐字节 JSON 快照基线及 `PROTOCOL_VERSION` 门禁（`gate.rs` 与 `fixtures/` 分离） |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [Crate 职责与依赖边界](../../docs/architecture/crates.md)、[工程规范](../../docs/development/engineering.md) |
| 分帧、魔数、超时与对端 UID 核验 | [私有存储与持久化](../../docs/architecture/storage.md#本地访问与所有权边界)、[会话存储与并发](../../docs/architecture/session/concurrency.md#输入与传输限制) |
| 请求、响应、视图与分页常量 | [实现模块与公开操作](../../docs/architecture/session/operations.md)、[重开、取消与未完成状态](../../docs/architecture/session/recovery.md#观察与重连) |
| 模型候选与凭据展示类型 | [模型引用与元数据](../../docs/architecture/llm/models.md#模型候选)、[凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md) |

## 约束

- `Cargo.toml` 的 `dependencies` 与 `build-dependencies` 中，workspace 内部成员仅允许 `lattice-session`、`lattice-conversation`、`lattice-store`（当前仅引入前两者），由 `cargo xtask crate-boundary` 门禁强制检查。
- `src/lib.rs` 保持显式重导出列表，不使用通配符 `*`，也不重导出 `SessionStore` 或存储写入接口。
- `AuthKey` 与响应分片缓冲 `ResponseFrame` 保持清零（`Zeroize`）且不实现暴露明文的 `Debug`；线上类型的字段顺序、serde 标签、`null`/省略规则与 `deny_unknown_fields` 严格受 `wire_shape.json` 守卫。
- 本 crate 之外：实例锁（`owner.lock`）、socket 绑定与清理时序、`ApiError` 映射、`lattice-llm` 到展示类型的逐字段转换及后台执行协调均在 `lattice-api` 中完成。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `env -u LATTICE_UPDATE_WIRE_SHAPE cargo test -p lattice-ipc` 与 `cargo clippy -p lattice-ipc --all-targets -- -D warnings` |
| 修改线上类型、字段属性或协议分帧 | `env -u LATTICE_UPDATE_WIRE_SHAPE cargo test -p lattice-ipc wire_shape`（补齐 `src/wire_shape/fixtures/` 对应片段；故意变更线上格式时须先递增 `PROTOCOL_VERSION`，再以 `LATTICE_UPDATE_WIRE_SHAPE=1` 更新快照） |
| 修改 `Cargo.toml` 依赖或 `test-utils` feature | `cargo xtask crate-boundary` 与 `cargo xtask harness-boundary` |
| 修改被后台或前端消费的类型与方法 | `cargo test -p lattice-api`、`cargo test -p lattice-cli`、`cargo test -p lattice-tui` |
