# lattice-session 工作指引

## 职责与模块

维护会话图、不可变 TurnCommit、分支和 detached HEAD、diff、usage、GitSessionGraphStore、PendingTurn/ResultDraft 和纯协调 reducer。对话及 usage 类型归 lattice-conversation，原样重导出，持久化归本 crate。

- `graph.rs`/`lib.rs`：图接口、模型、错误、分支校验与公开重导出。
- `git_graph/session.rs`/`state.rs`：SessionState 与 session ref 发布；commit/refs 模块：树、commit、transcript、manifest、分支/checkout/reset/diff。
- `git_graph/pending.rs` 与 `pending/`：CAS 日志、材料分类、候选修订、最终引用事务与纯 reducer。
- `git_graph/validation.rs`：认证到会话 ref 集合的派生 strict-open 投影；helpers：序列化辅助。
- tests/pending_journal、pending_crash、pending_fixtures：日志/崩溃/拓扑与确定性构造；test_utils 仅为下游测试提供设施。

## 上位文档

修改前读取[会话合同](../../docs/architecture/session.md)、[存储合同](../../docs/architecture/storage.md)、[工作区 checkpoint](../../docs/architecture/workspace.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- Git 是唯一生产存储；无 SQLite、可变 sidecar、importer、git2 或自制持久化机制。会话永久绑定组成层传入的 ProjectStoreKey，不从进程 cwd 推导。
- 不负责 provider、profile 发现、CLI、Tool 执行或 InvocationStore I/O，不依赖 audit。Invocation facts 由 API 投影，reconcile 只消费数据；不能把候选修订请求当成闭合 TurnOutcome。
- committed serde 形状、canonical JSON、commit 身份及类型化 checkpoint 是持久契约，不就地改 conversation 重导出形状。`Empty` 仅来自权威历史 ref 缺失，OID 必须规范校验。
- refs/锁/严格打开与最终发布遵循会话、存储合同。open 不初始化或修复；普通读取不增加全局写瓶颈，strict-open 在仓库锁内验证跨引用不变量。损坏无关会话也不能被忽略。
- 派生校验投影与领域 ref 变更在同一内核事务发布，open 不写投影；缺失或认证不符转完整会话对象校验，不使投影成为第二权威。不为 refs/lattice/** 配置远端 push refspec。
- CAS 成功已含所需对象/引用屏障，不再追加重复屏障；已发布但未确认持久的精确重试仍完成同步。任意 I/O 失败的部分效果按存储合同处理，不承诺普通错误时所有引用必然未动。
- journal payload predecessor 与 Git parent 一致，根接冻结 conversation tip；每个 binding/draft/candidate 地址与 SessionId 交叉核验。strict-open 验证完整 active-turn 和永久 Turn pin 的 manifest 身份。
- ContinueModel 的 model_step_ordinal 使两份快照即可验证步连续性与首次 attempt 为零。无 journal committer time 时 reconcile fail closed；advance_pending_turn 先识别精确已发布候选，再判断是否非法迁移。
- 累积 post_user_transcript 是唯一 authority；live/reopen 共用候选基础。重开只关闭、不请求模型或重执行 Tool；最终发布仅经 PendingTurn 事务。
- 直接 commit 构造和 fixture builder 仅测试可见；下游只在 dev-dependencies 启用 test-utils，生产依赖不得取消 cfg 隔离。保留候选修订的结构不变、精确计划及所有权校验。

## 验证入口

- `cargo test -p lattice-session`，保留 pending correctness、strict-open、lineage、pin 与真实 SIGKILL fixture。
- transcript/ref 变化覆盖 linear、merge、diamond、branch reset、detached HEAD，不能漏 merge 侧消息。
- schema/ref/topology 变化关联 API/CLI；性能变化按实际影响验证竞争或分块，不将 diagnostic-only 活动当作正确性豁免。
- `cargo xtask lancet-boundary` 检查生产 test-utils 隔离与依赖。
