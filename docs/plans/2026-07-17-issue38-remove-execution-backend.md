# #38 实现计划：收拢 Structured Workspace Operations，删除 ExecutionBackend 路由层

- 票：<https://codeberg.org/AstRin/LATTICE/issues/38>
- 前置依赖：#37（[计划](2026-07-17-issue37-direct-local-turn.md)）。全局约束与
  验证矩阵见 [总览](2026-07-17-direct-local-overview.md)。
- 对应 ADR 0003 Migration Sequence step 2 + step 3 的删除部分。

## 1. HandlerContext 直持 concrete module

- `ServerRunConfig` 删 `execution`；server run 时从
  `cep_context.policy.project_root` 构造一次 `Arc<LocalWorkspace>`
  （零新增配置面——root 已在 policy 里，满足验收"runtime public configuration
  只通过 canonical project root 和 resolved policy 定位 Local Workspace"）。
- `HandlerContext`：`execution: ExecutionRouting` +
  `routing_invocation: RoutingInvocationState` → `workspace: Arc<LocalWorkspace>`。
- `LocalWorkspace` 保持 `pub(crate)`（handlers 同 crate）。`fetch.web`/
  `session.todo_write` 的 handler 自然不再携带 routing state（验收第 6 条）。

## 2. handlers 与 edit 引擎直连

```rust
// handlers/observation.rs（改后）
let facts = ctx.workspace.resolve_secure(&path, PathIntent::Read)?;   // 原 backend.resolve_path
let bytes = ctx.workspace.read_file(&path).await?;                     // 原 backend.read_file

// edit/storage.rs（改后）
pub(crate) struct EditStorage {
    workspace: Arc<LocalWorkspace>,
    authorized_region: AuthorizedEffectRegion,
}
```

## 3. 删除清单（票文逐项）

- `execution_backend.rs` 整文件：`ExecutionBackend` trait、`ExecutionRouting`、
  `UpperContext`、`NullExecutionBackend`、`RoutingInvocationState`、
  `direct_local_routing`（#37 的过渡工厂）、`changed_paths`（实测无 production
  caller，仅 `workspace_backend.rs` 的 `#[cfg(test)]` 引用，随 #37 已删）。
- 中性类型 `WorkspacePath`/`WorkspacePathError`/`PathFacts`/`PathIntent`/
  `PathKind`/`DirectoryEntry`/`GrepRequest`/`GrepMatch`/`FindRequest` 迁至
  `workspace/types.rs`（这些是 Local 操作词汇，非 routing 词汇）。
- `lib.rs:23-25` 的 public re-export 行同步收缩。
- `observation.rs` 的 `path_facts_to_guard` 桥接函数——`LocalWorkspace` 直接
  产出 guard 层 `PathFacts` 后删除。

## 4. audit evidence 置换

- `registry.rs:70-81`：删 `adapter_preflight_invoked`/`backend_invoked`/
  `execution_backend`/`upper_workspace_id` 写入；`RejectionStage` 词汇去
  Upper 化：`AdapterPreflight` → `PathGuard`、`BackendExecution` → `LocalIo`
  （`VerificationFailure` 若已无 caller 则一并删）。
- `mutation_preflight_metadata`（`enforcement/preflight.rs`）：删
  `backend_invoked`，保留并强化 method-local 证据：guarded normalized path、
  `local_revision`（旧 `expected_hash_status` 语义升级命名）、precondition
  判定、安装后 content hash。
- 受影响断言集中在 `registry.rs` 内嵌测试、`dogfood_impact_matrix.rs`、
  `audit_turn_provenance.rs`——随字段一次性改写，集中一个 commit 完成。

## 5. spy → 真实 module + 窄 fault injection

- 删除：`registry.rs::SpyBackend`、`observation.rs::ObsSpyBackend`、
  `mutation.rs::CreateSpyBackend`（两处）、`edit/apply.rs::HostFsBackend`。
  测试全部改 tempdir + 真 `LocalWorkspace`。
- 唯一允许的 fault injection（证明"安装前失败目标不变"，不得形成通用
  plugin seam）：

```rust
// workspace/local.rs — 不是 trait，不可组合，只能翻转一个布尔
#[cfg(any(test, feature = "test-fault-injection"))]
pub(crate) fn fail_next_install(&self) { self.fail_install.store(true, SeqCst); }
```

## 6. 验收对照

- 产品级 smoke：#37 留下的 direct-Local observation/mutation 行为在删层前后
  输出一致（对同一 tempdir 场景快照对比）。
- cancellation、Turn commit、policy denial、effect contracts、audit provenance
  全绿。
- `rg -n "ExecutionBackend|ExecutionRouting|UpperContext|NullExecutionBackend|RoutingInvocationState" crates --type rust` 为零。
