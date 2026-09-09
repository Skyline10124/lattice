# lancet-api 工作指引

## 职责

提供独立于 LATTICE session/agent/profile 的 LANCET 公共门面，转发 Component 组成、执行和操作员读取。生产调用仅由 `lattice-api` 发起；低层入口供预留门控测试，不是独立 Tool 执行产品。

- `runtime.rs` 的 `LancetRuntime::build` 使用进程共享生成目录，执行委托 `RuntimeExecution`；配置无调用方自供目录或 transport seam。
- `config.rs` 承载策略、能力准入、严格打开的项目 InvocationStore 和 WorkspaceHistory。
- `audit.rs` 重导出 audit DTO，将 `OperatorAuditError` 映射为 `LancetError` 且不改消息。
- `status.rs` 从一个 pinned ledger view 取最新 20 条，返回实际 `ledger_oid`；分页、过滤及投影归 audit。
- `rewind.rs` 提供工作区恢复及独立 append-only 审计的门面；`error.rs` 和 `lib.rs` 维护公开错误与重导出。

## 上位文档

修改前读取 [LANCET 合同](../../docs/architecture/lancet.md)、[Cell 证据](../../docs/architecture/cell.md)、[工作区 Rewind](../../docs/architecture/workspace.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- 不依赖 lattice-agent/session/profile/cli。输入使用 LANCET 类型，profile→policy 转换归 `lattice-api`；不另建 Component 组装路径、独立调用上下文或 Rewind 适配器。
- 生产分发携带精确持久 `InvocationBinding` 并通过预留准入。准入失败后重读同一 InvocationId：仍在 pre-dispatch 才返回 `NotExecuted`；DispatchStarted、terminal 或不可读状态返回 `ReconciliationRequired`，保留原 retriability。
- 准入前同步检查取消，DispatchStarted 后再次同步到 call-scoped sender，polling 仅负责之后的 in-flight 区间。
- 操作员读取直达 InvocationStore，不作为 Agent Tool。DTO 只暴露类型化身份、生命周期、摘要、长度和无内容 Cell 证据；不暴露参数、结果、provider payload、内部元数据或兼容指针。
- 不恢复 Turn/audit provenance 重建、attestation 或旧行导入；artifact identity 和 Bundle provenance 留在其所属契约。
- plan/apply Rewind 接收 acquire 产生的 opaque `HeldWorkspaceOperation`，历史访问前核验精确 project/store/lock 绑定，apply 再核 canonical root，不重新取得项目锁。会话图移动归上层，Rewind 审计链归 runtime。

## 验证入口

`cargo test -p lancet-api`。组成变化关联 runtime/API Component 测试；操作员变化关联 `cargo test -p lancet-audit` 及使用当前二进制的独立 Direct-Local 操作员矩阵。
