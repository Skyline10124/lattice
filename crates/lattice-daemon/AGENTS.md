# lattice-daemon 工作指引

提供 LATTICE 本地后台的独立宿主二进制 `lattice-daemon`：解析 `--state-root` 与 `--drain-timeout`，初始化 `tracing-subscriber` 日志与 Tokio 运行时，注册 `SIGTERM` 与 `SIGINT`，对接 systemd `$NOTIFY_SOCKET`（`READY=1` 与 `STOPPING=1`），调用 `lattice_server::serve` 并按托管合同映射进程退出码，同时将实例锁保持至进程退出。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/main.rs` | 中文命令行参数解析与校验、日志与异步运行时初始化、`SIGTERM` / `SIGINT` 信号监听、状态根占用时的 `SO_PEERCRED` 对端进程号诊断、`ServeExit` 退出码映射与实例锁生命周期保持 |
| `src/notify.rs` | 基于 `rustix::net` 的 systemd `$NOTIFY_SOCKET` 非阻塞数据报发送（支持文件系统路径与 `@` 抽象命名空间地址，保留原始字节） |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [Crate 职责与依赖边界](../../docs/architecture/crates.md)、[工程规范](../../docs/development/engineering.md) |
| 启动参数、`$NOTIFY_SOCKET`、退出码与锁释放时序 | [实现模块与公开操作](../../docs/architecture/session/operations.md)、[重开、取消与未完成状态](../../docs/architecture/session/recovery.md)、[私有存储与持久化](../../docs/architecture/storage.md#本地后台与访问边界) |

## 约束

- 只提供二进制目标 `lattice-daemon`，不公开库目标或内部模块；后台业务、准入控制与存储协调仍在 `lattice-server`。
- `Cargo.toml` 的内部依赖仅限 `lattice-server`、`lattice-ipc`、`lattice-store`（由 `cargo xtask crate-boundary` 门禁检查），不得依赖 `lattice-local`、`lattice-cli` 或 `lattice-tui`。
- 不定义测试转发 feature；需要假工具或崩溃屏障时直接以 `--features lattice-server/test-fake-tools,lattice-server/test-crash-barriers` 构建 `lattice-daemon`。
- 帮助与用户可见输出使用中文；系统调用统一经 `rustix` 并以 `std::io::Error::from(errno)` 转换错误。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-daemon` 与 `cargo clippy -p lattice-daemon --all-targets -- -D warnings` |
| 修改参数、通知或退出码 | `TMPDIR=/tmp LATTICE_BIN=$PWD/target/debug/lattice LATTICE_DAEMON_BIN=$PWD/target/debug/lattice-daemon cargo test -p lattice-system-harness` |
| 修改 `Cargo.toml` 依赖 | `cargo xtask crate-boundary` 与 `cargo xtask harness-boundary` |
