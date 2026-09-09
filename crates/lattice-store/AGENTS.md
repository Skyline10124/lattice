# lattice-store 工作指引

## 职责

领域中立的 Linux 私有 Git 存储内核，提供状态布局、ProjectStoreKey、类型化 create/open、身份记录、规范字节与对象写入、引用事务、锁、同步屏障、首次创建、保留 pin、位置能力探测及类型化错误。

作为 Layer-0 叶子，不依赖工作区内部 crate。Session、Invocation/证据及工作区历史的语义分别归 session、audit、runtime。

## 上位文档

修改前读取[私有存储与持久化](../../docs/architecture/storage.md)、[宿主平台](../../docs/architecture/platform.md)及[测试工程](../../docs/development/testing.md)。诊断解释见 [InvocationStore 诊断指南](../../docs/development/invocation-store-diagnostics.md)。

## 维护边界

- layout/key/meta 模块维护通用拓扑、规范根路径原始字节的域分离 SHA-256 和身份核验；领域拥有其 store 文件名、schema 和 ref 名，不在内核建中央 schema、业务 put/get、全存储注册表或可插拔后端。
- repo/durability 维护严格打开、create、对象与引用发布、retention 和屏障。锁、FIFO、显式 guard 重入和精确重试的同步要求遵循存储合同；成功 CAS 已完成 pending 对象屏障，跳过 CAS 的精确重试仍须完成所需屏障。
- 首次创建预建并同步全部对象 fanout，再发布目录；strict-open 验证而不修复拓扑。steady-state 只同步对象文件及精确 fanout 目录，ref-only 屏障不触碰对象空间。
- 缺失 store 返回 `NotInitialized`；kind/version/scope 或 fanout 错误返回 `Corruption`。禁止 open-failed-init、静默修复、隔离后替换、旧路径回退或空库替代；数据重建需单独授权。
- `create*` 是指定组成入口唯一初始化路径，使用同级临时目录与 `RENAME_NOREPLACE`，create lock 与 repository write lock 不重叠。
- 锁序、禁止共享升级、最多一把 repository write lock 按合同执行。同进程 strict 等待者按 ticket FIFO，非阻塞获取不能插队；跨进程排斥仍由 flock 保证，不能退回普通广播争抢。
- local-ledger 的观测窗口和诊断阈值见[等待指标解读](../../docs/development/invocation-store-diagnostics.md#等待指标解读)；不要把端到端观测量当成 FIFO 入队等待上限。
- 不自动 GC、repack、维护或级联删除；内核不生成时钟，`write_commit_at` 使用调用方时间。
- 新机制只有在两个无关领域能相同使用时才进入内核。优先窄类型化 accessor，不暴露 raw `gix::Repository`。

## 验证入口

- `cargo test -p lattice-store`。
- `cargo clippy -p lattice-store --all-targets -- -D warnings`。
- `cargo tree -p lattice-store --edges normal`：不出现 lattice/lancet/net-guard 工作区依赖。
- 机制变化按影响验证 session、audit、runtime 及协调层；fault seam 仅用于测试，`lattice-store-probe` 仅在另外获准的位置运行。
