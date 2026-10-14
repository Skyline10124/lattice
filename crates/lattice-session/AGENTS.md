# lattice-session 工作指引

## 职责与模块

维护全局会话库 `lattice.redb`：Session、会话树（Step 节点内含尝试与调用槽位）、配置表、TurnId 索引、InvocationId 索引和调用变更日志。提供同步的领域事务、由阶段给出结论、历史读取与操作员只读投影；对话与 usage 类型归 `lattice-conversation`。

- `store/mod.rs`、`types.rs`：SessionStore、锁凭据、领域类型、错误与公开接口。
- `store/codec.rs`、`open.rs`、`validate.rs`：表与值版本、创建/严格打开、形状及跨记录一致性校验。
- `store/write.rs`、`close.rs`：事务写入与纯关闭规划；`read.rs`：快照、树、Step 详情、分页和操作员投影。
- `store/config.rs`、`hex_digest.rs`：冻结事实与摘要编码；`local.rs`：按 Session/Turn 等范围加载事务记录。
- `store/tests.rs`：记录层回归。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)、[存储合同](../../docs/architecture/storage.md)、[调用与证据](../../docs/architecture/lancet.md)及[测试工程](../../docs/development/testing.md)。涉及文件关联时读取[工作区历史](../../docs/architecture/workspace.md)。旧 Git 会话、独立调用账本与跨库协调的取代范围见 [ADR 0006](../../docs/adr/0006-tree-session-and-global-redb-store.md)。

## 局部约束

- 会话库格式与事务由本 crate 拥有，不暴露原始表或写事务。redb 版本、文件后端、独占打开与 Immediate 提交遵循存储合同；不引入双写、旧库迁移或兼容读取。
- Session 永久绑定组成层传入的 ProjectStoreKey，不从进程 cwd 推导。不负责 provider、profile 发现、CLI 或 Tool 执行，不依赖 runtime/API/agent/profile；操作员读取保持 runtime-free。
- open 不初始化、不修复，损坏无关 Session 也不能忽略。类型、值版本、规范 JSON、绑定摘要与证据版本都须校验，派生索引和变更日志随权威记录在同一事务更新。
- 树只追加节点，只有末端未关闭的 Step 可改写，每次推进一个事务。Step 在开始前预留，最后一个 Step 关闭即 Turn 关闭。结论由 `progress` 给出，不从缺少结果推定未执行。
- 执行者消失的 Turn 不被补写；读取只读，不修改未关闭的 Step。退回只移动当前位置，不写入节点，文件及工作区历史保持原样。
- 锁范围、失败后停写与关闭重开遵循会话和存储合同。写事务内不等待外部操作；同步接口由异步调用方放入阻塞执行边界。
- 操作员 DTO 不含参数、结果正文、schema 或冻结秘密。测试设施仅在 `cfg(test)` 或显式测试 feature 下可用，下游通过 dev-dependencies 启用，生产不得绕过领域事务。

## 验证入口

- `cargo test -p lattice-session`；须隔离测试的部分按[测试合同](../../docs/development/testing.md)先列出全部失败方式，再观察红后实现。
- 格式、树、事务或读取变化关联 API/CLI；真实提交、退回分叉、旧库拒绝和崩溃关闭使用当前二进制的外部 harness，不以纯 planner 测试代替提交窗口。
- `cargo xtask lancet-boundary` 检查 session 的 runtime-free 边界与生产测试 feature 隔离。
