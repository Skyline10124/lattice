# lancet-api 工作指引

## 职责

提供 LANCET 公共门面，转发 Component 组成与执行，并公开 `lattice-session` 的操作员只读查询。生产调用由 `lattice-api` 协调；低层入口不是独立 Tool 执行产品。

- `runtime.rs` 的 `LancetRuntime::build` 使用进程共享生成目录，`dispatch` 消费已提交的 `StartedInvocation`，执行委托 `RuntimeExecution`；配置无调用方自供目录或 transport seam。
- `config.rs` 承载策略、能力准入、冻结 TLS 根和严格打开的 WorkspaceHistory。
- `audit.rs`、`status.rs` 接收后台已有 SessionStore 的读视图，转发分页、过滤、replay 和近期状态；领域投影归 session。
- `error.rs`、`lib.rs` 维护类型化错误与公开重导出。

## 上位文档

修改前读取 [LANCET 合同](../../docs/architecture/lancet.md)、[会话调用槽位](../../docs/architecture/session.md#调用槽位)、[Cell 证据](../../docs/architecture/cell.md)、[工作区历史](../../docs/architecture/workspace.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- 不依赖 lattice-agent/profile/cli。profile→policy 转换归 `lattice-api`；不另建 Component 组装路径或独立调用上下文。
- 生产分发须核验精确持久绑定与请求，并消费 session 在 Started 提交后产生的窄凭据。不自行登记槽位、写终态或绕过 TurnCoordinator 分发；结果与证据经 receipt 交回协调层。
- 分发前取消由协调层处理；Started 后、runtime 接管前再次同步取消到 call-scoped sender，polling 仅负责之后的 in-flight 区间。
- 操作员读取不构建 runtime、不自行打开会话库、不作为 Agent Tool。DTO 只暴露类型化身份、生命周期、摘要、长度和无内容 Cell 证据；不暴露参数、结果正文、provider payload、内部元数据或兼容指针。
- 不恢复 Turn/audit provenance 重建、attestation 或旧行导入；artifact identity 和 Bundle provenance 留在其所属契约。会话退回归上层，不恢复文件或建立恢复审计链。

## 验证入口

`cargo test -p lancet-api`。组成变化关联 runtime/API Component 测试；操作员变化关联 `cargo test -p lattice-session` 及使用当前二进制的外部 `direct_operator` 场景。生产测试 feature 隔离由 `cargo xtask lancet-boundary` 检查。
