# lancet-audit 工作指引

## 职责

维护项目级 Git `InvocationStore` 和 query/tail/replay/status 的类型化投影。append-only data ledger 是预留、分发、终态、材料状态和 Cell 证据的唯一生产权威。

`invocation_store.rs` 维护拓扑、批量预留、精确准入、发布、固定视图、高水位 witness 和认证的派生 checkpoint；`operator.rs` 从一个 pinned view 投影 DTO，tail 游标为排他的 commit OID，replay 按类型化 SessionId/TurnId 和结构绑定顺序读取。

## 上位文档

修改前读取 [LANCET 的调用账本与操作员接口](../../docs/architecture/lancet.md)、[Cell 证据](../../docs/architecture/cell.md)、[私有存储](../../docs/architecture/storage.md)、[测试工程](../../docs/development/testing.md)。诊断活动见 [InvocationStore 诊断指南](../../docs/development/invocation-store-diagnostics.md)。

## 局部约束

- 生产不依赖 runtime 或 LATTICE session/agent/profile；不组合 Tool runtime 即可操作员读取。不在此执行实时策略、准入或工作区决策。
- InvocationId 是唯一公开调用身份；不恢复数字 call/audit/evidence ID、SQLite、双写、导入器或兼容适配。
- DTO 不包含参数、结果秘密或 Tool schema，只投影身份、生命周期、摘要、长度和证据。
- 通用 Git、锁、CAS 与持久机制由 lattice-store 提供；生命周期及投影语义归本 crate。witness 与派生 checkpoint 是可修复辅助状态，不构成第二权威，其允许状态及修复顺序按 LANCET 合同执行。
- `src/test_support.rs` 的 `ScopedXdgStateHome` 是工作区唯一进程 XDG 测试 guard，通过 `test-support` 使用。
- `tests/invocation_qualification.rs` 可通过 dev-dependency 将 pinned view 投影为 session pending facts；生产源码不出现 session 类型。

## 验证入口

- `cargo test -p lancet-audit`、`cargo xtask lancet-boundary`。
- 操作员变更关联 lancet-api、`direct_operator` 和显式 `release_operator`。
- 性能诊断不替代正确性验证，历史 bundle/verdict 保留要求按测试合同执行；是否运行实验须有具体授权。
