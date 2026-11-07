# lattice-store 工作指引

提供 Linux 私有存储布局、`ProjectStoreKey`、文件锁、位置能力探测及工作区历史专用的类型化 Git 存储内核，被 `lattice-session` 与 `lancet-runtime` 使用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出布局、键、锁、Git 存储仓库、同步屏障、探测与错误类型 |
| `src/layout.rs` | XDG 私有状态根解析与 `StateLayout` 目录拓扑 |
| `src/key.rs` | 规范工作区根路径原始字节的域分离 SHA-256 `ProjectStoreKey` 推导 |
| `src/meta.rs` | Git 仓库身份 `StoreMeta`、`ExpectedMeta` 与 `refs/lattice/meta` 校验 |
| `src/repo.rs` | `StoreRepo` 首次创建、严格打开、不可变对象与提交读写及单引用 `cas_ref` |
| `src/durability.rs` | 对象文件、fanout 目录、引用及父目录的 `fsync` 同步屏障 |
| `src/lock.rs` | Linux `flock` 操作锁与同进程票号 FIFO 严格写锁凭据 `FlockGuard` |
| `src/canon.rs` | 规范字节辅助函数 |
| `src/probe.rs` | 目标状态目录的文件系统与原语能力探测 |
| `src/fault.rs` | `test-fault-injection` feature 控制的创建与同步屏障故障注入点 |
| `src/error.rs` | 类型化存储与持久性错误 `StoreError` |
| `src/bin/lattice-store-probe.rs` | 在指定目标目录执行位置能力探测与最小仓库冒烟的命令行工具 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [私有存储与持久化](../../docs/architecture/storage.md)、[宿主平台](../../docs/architecture/platform.md)、[工程规范](../../docs/development/engineering.md) |
| Git 对象、引用发布与同步屏障 | [仓库首次创建](../../docs/architecture/storage.md#仓库首次创建)、[发布顺序](../../docs/architecture/storage.md#发布顺序)、[屏障覆盖](../../docs/architecture/storage.md#屏障覆盖) |
| 锁顺序与票号排队 | [锁与并发语义](../../docs/architecture/storage.md#锁与并发语义)、[历史诊断解读](../../docs/development/invocation-store-diagnostics.md) |
| 故障注入测试与位置探测 | [能力探测](../../docs/architecture/storage.md#能力探测)、[测试工程](../../docs/development/testing.md) |

## 约束

- `Cargo.toml` 保持零工作区内部 crate 依赖；新增通用机制以至少两个无关领域按同一语义复用为前提，并通过 `StoreRepo` 的窄类型化接口封装内部 `gix::Repository`。
- `layout`、`key` 与 `meta` 维护通用状态根拓扑、原始路径字节域分离摘要和仓库身份校验，具体库文件名、schema 与引用名由各领域自行定义，见[职责分工](../../docs/architecture/storage.md#职责分工)与 [ProjectStoreKey](../../docs/architecture/storage.md#projectstorekey)。
- 初始化统一走 `StoreRepo::create`（同级临时目录预建全部 fanout、同步后以 `RENAME_NOREPLACE` 发布，创建锁与仓库写锁分离）；`StoreRepo::open` 只做严格核验，缺失返回 `StoreError::NotInitialized`，拓扑、身份、版本或作用域不符返回 `StoreError::Corruption` 并保留磁盘现场，数据重建仅按维护者单独授权执行，见[打开](../../docs/architecture/storage.md#打开)与[失败分类与恢复责任](../../docs/architecture/storage.md#失败分类与恢复责任)。
- 稳态写入只同步本次新对象与对应 fanout 目录，纯引用更新只覆盖引用及其父目录；成功 `cas_ref` 已包含挂起对象屏障，跳过 CAS 的精确重试显式补齐所需屏障，见[发布顺序](../../docs/architecture/storage.md#发布顺序)与[屏障覆盖](../../docs/architecture/storage.md#屏障覆盖)。
- 对象与引用按只追加方式保留，`write_commit_at` 直接使用调用方传入的时间戳，使存储内核保持无后台维护和无时钟副作用，见[保留、索引与演化](../../docs/architecture/storage.md#保留索引与演化)。
- 跨进程互斥由 Linux `flock` 保证，同进程严格写锁等待者按票号 FIFO 顺序获锁并让排队期间的 `try_acquire` 直接返回忙碌，锁序规则见[锁与并发语义](../../docs/architecture/storage.md#锁与并发语义)；旧 Git 账本的等待指标背景见[历史诊断解读](../../docs/development/invocation-store-diagnostics.md)。
- `test-fault-injection` 仅在测试中启用以验证创建与屏障失败路径；`lattice-store-probe` 仅在明确指定的目标状态目录运行，见[能力探测](../../docs/architecture/storage.md#能力探测)。
- 本 crate 之外：全局会话库 `lattice.redb`、redb 表与领域事务、调用槽位与证据持久化由 `lattice-session` 负责；工作区历史的快照与效果写入由 `lancet-runtime` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-store` 与 `cargo clippy -p lattice-store --all-targets -- -D warnings` |
| 修改 `Cargo.toml` 依赖 | `cargo tree -p lattice-store --edges normal`（确认无工作区内部 crate 依赖） |
| 修改创建、屏障故障点 | `cargo test -p lattice-store --features test-fault-injection` |
| 修改布局、锁或 Git 存储原语 | `cargo test -p lattice-session`、`cargo test -p lancet-runtime`、`cargo test -p lattice-api` |
