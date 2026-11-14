# lattice-store 工作指引

提供 Linux 私有存储布局、私有目录与文件原子读写、文件锁、`fsync` 与位置能力探测，被 `lattice-session`、`lattice-llm`、`lattice-local` 与 `lattice-daemon` 使用（`lattice-server` 仅在测试中使用）。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出布局、私有目录与文件操作、锁、同步屏障、探测与错误类型 |
| `src/layout.rs` | XDG 私有状态根解析与 `StateLayout` 目录拓扑 |
| `src/private_file.rs` | 私有模块：私有目录与单分量私有文件的属主/权限核验、子目录打开与创建、限量读取及同目录临时文件原子替换（`PrivateDirectory`、`PrivateFileError`、`PrivateFileOperation`） |
| `src/durability.rs` | 文件与目录的 `fsync` 同步屏障 |
| `src/lock.rs` | Linux `flock` 操作锁与同进程票号 FIFO 严格锁凭据 `FlockGuard` |
| `src/probe.rs` | 目标状态目录的文件系统与原语能力探测 |
| `src/error.rs` | 类型化存储与持久性错误 `StoreError` |
| `src/bin/lattice-store-probe.rs` | 在指定目标目录执行位置能力探测的命令行工具 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [私有存储与持久化](../../docs/architecture/storage.md)、[宿主平台](../../docs/architecture/platform.md)、[工程规范](../../docs/development/engineering.md) |
| 状态根布局 | [私有状态根](../../docs/architecture/storage.md#私有状态根) |
| 私有目录与单分量私有文件读写 | [凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md#authjson)、[ADR 0011](../../docs/adr/0011-im-gateway-inside-backend.md) |
| 锁顺序与票号排队 | [锁与并发语义](../../docs/architecture/storage.md#锁与并发语义)、[历史诊断解读](../../docs/development/invocation-store-diagnostics.md) |
| 位置探测与持久性屏障 | [首次创建](../../docs/architecture/storage.md#首次创建)、[能力探测](../../docs/architecture/storage.md#能力探测)、[测试工程](../../docs/development/testing.md) |

## 约束

- `Cargo.toml` 保持零工作区内部 crate 依赖。
- `layout` 维护通用状态根拓扑，具体会话库文件名、会话锁文件名与表结构由 `lattice-session` 定义，见[私有状态根](../../docs/architecture/storage.md#私有状态根)。
- `private_file` 保持为私有子模块，接口只处理目录句柄、单个普通路径分量名称与字节切片，不包含 JSON 或上层业务语义；目标目录与文件均通过 `rustix` 以 `NOFOLLOW`、`NONBLOCK` 核验本进程有效 UID 与 `mode & 0o077 == 0`，不自动修改权限或属主；重命名成功后的目录同步失败返回专用 `PrivateFileError::DirectorySyncAfterReplace`，见 [`auth.json`](../../docs/architecture/llm/credentials.md#authjson)。
- 跨进程互斥由 Linux `flock` 保证；`acquire_flock` 提供独立操作锁（会话执行锁使用），`acquire_flock_strict` 提供同进程票号 FIFO 严格锁（会话库首次创建锁使用），排队期间的 `NonBlocking` 获取直接返回忙碌，锁序规则见[锁与并发语义](../../docs/architecture/storage.md#锁与并发语义)。
- `fsync_file` 与 `fsync_dir` 同步失败返回 `StoreError::Durability`（打开失败返回 `StoreError::Io`），供会话库首次创建与位置探测使用，见[首次创建](../../docs/architecture/storage.md#首次创建)。
- `lattice-store-probe` 仅在明确指定的目标状态目录运行，验证同挂载点临时目录、`fsync`、`renameat2(RENAME_NOREPLACE)` 与本地 `flock`，见[能力探测](../../docs/architecture/storage.md#能力探测)与[验证入口与承接](../../docs/architecture/storage.md#验证入口与承接)。
- 本 crate 之外：全局会话库 `lattice.redb`、redb 表与领域事务及调用槽位持久化由 `lattice-session` 负责；模型凭据格式与校验由 `lattice-llm` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-store` 与 `cargo clippy -p lattice-store --all-targets -- -D warnings` |
| 修改 `Cargo.toml` 依赖 | `cargo tree -p lattice-store --edges normal`（确认无工作区内部 crate 依赖） |
| 修改私有目录与文件原语 | `cargo test -p lattice-llm` |
| 修改布局、锁、屏障或探测 | `cargo test -p lattice-session`、`cargo test -p lattice-server` |
