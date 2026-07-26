# #59 实现计划：production Turn 直连 Local Workspace

- 票：<https://github.com/Skyline10124/lattice/issues/59>（原 Codeberg #37）
- 前置依赖：无。全局约束与验证矩阵见
  [总览](2026-07-17-direct-local-overview.md)。
- 对应 ADR 0003 Migration Sequence step 1 + step 3 的 production cutover 部分。

## 1. concrete Local Workspace module（`lancet-native`）

新增 `crates/lancet-native/src/workspace/`（`mod.rs` + `local.rs` + `types.rs`
预留）：

```rust
// crates/lancet-native/src/workspace/local.rs
// 唯一 concrete Local Workspace（ADR 0003 决定 3）。内部深模块：
// 安全路径解析 / bounded read / 原子安装 / 错误归一化全部藏在这里。
pub(crate) struct LocalWorkspace {
    /// canonical project root（构造时 canonicalize，之后不变）
    root: PathBuf,
}

impl LocalWorkspace {
    pub(crate) fn new(root: &Path) -> Result<Self, LocalWorkspaceError> { /* canonicalize */ }

    /// 逐段解析：任何一段是 symlink 即拒绝（leaf 与 parent 同判），
    /// 解析结果必须仍在 root 之下。移植自
    /// lattice-workspace/src/testing/mod.rs 的 DeterministicWorkspace 实现。
    fn resolve_secure(&self, path: &WorkspacePath, intent: PathIntent)
        -> Result<PathFacts, LocalWorkspaceError>;

    pub(crate) async fn read_file(&self, path: &WorkspacePath) -> Result<Vec<u8>, _>;
    pub(crate) async fn list_dir(&self, path: &WorkspacePath) -> Result<Vec<DirectoryEntry>, _>;
    pub(crate) async fn grep(&self, req: &GrepRequest) -> Result<Vec<GrepMatch>, _>;
    pub(crate) async fn find_files(&self, req: &FindRequest) -> Result<Vec<WorkspacePath>, _>;

    /// mutate.create：no-replace 安装（ADR 决定 8）
    pub(crate) async fn create_excl(&self, path: &WorkspacePath, content: &[u8])
        -> Result<CreatedFile, _>;

    /// mutate.edit 安装：同目录 tempfile → fsync → rename 原子替换
    pub(crate) async fn install_atomic(&self, path: &WorkspacePath, content: &[u8])
        -> Result<InstalledFile, _>;

    pub(crate) async fn remove_file(&self, path: &WorkspacePath) -> Result<(), _>;
}
```

关键实现点（指导性示例）：

```rust
// no-replace create：竞争者先建 → AlreadyExists → 上层映射 ReplanRequired
let mut opts = std::fs::OpenOptions::new();
opts.write(true).create_new(true);           // O_CREAT|O_EXCL
match opts.open(&full_path) { /* AlreadyExists → LocalWorkspaceError::TargetExists */ }

// atomic install：临时文件必须与目标同目录（同文件系统，rename 才原子）
let tmp = tempfile::Builder::new()
    .prefix(".lattice-tmp-")
    .tempfile_in(parent_dir)?;
tmp.as_file().write_all(content)?;
tmp.as_file().sync_all()?;                    // 数据落盘
tmp.persist(&full_path)?;                     // rename(2)，原子替换
fsync_dir(parent_dir)?;                       // 目录项持久化
```

```rust
// 逐段 symlink-safe 解析（伪代码骨架；leaf 用 symlink_metadata 判断）
let mut cur = self.root.clone();
for comp in rel.components() {
    cur.push(comp);
    match std::fs::symlink_metadata(&cur) {
        Ok(m) if m.file_type().is_symlink() => return Err(SymlinkRejected(cur)),
        Ok(_) => {}
        Err(e) if e.kind() == NotFound => { /* 记录 parent_exists 事实，剩余段只做词法检查 */ }
        Err(e) => return Err(Io(e)),
    }
}
debug_assert!(cur.starts_with(&self.root));   // 词法层已由 WorkspacePath 保证无 ..
```

- `content_hash` 直接复用 `edit/hash.rs`（blake3），观察结果携带 hash =
  **Local Revision**。
- grep/find/list 的 entry/depth/byte/match 限制沿用 `ObservationPolicyConfig`
  现值，截断显式上报（ADR 决定 6）。
- I/O 放 `tokio::task::spawn_blocking`（对齐现有 handler 习惯）。

**module 级测试**（ADR step 1 钦定八类，全部 tempdir）：root escape、symlink
parent/leaf 拒绝、System Path Exclusion 占位（#61 前先只测 root escape）、
limits、expected-hash conflict、no-replace create、atomic replacement、
partial-write failure（正式 fault injection 归 #60，本票先用"写只读目录"这类
真实故障近似）。

## 2. crate-internal migration adapter

`execution_backend.rs` 内追加（**不导出类型，只导出工厂**——满足票文
"crate-internal、无新增注入面"）：

```rust
// crates/lancet-native/src/execution_backend.rs
struct LocalExecutionBackend(Arc<LocalWorkspace>);      // private

impl ExecutionBackend for LocalExecutionBackend {
    fn backend_name(&self) -> &str { "direct-local" }
    // read/write/list/grep/find/resolve_path → 委托 LocalWorkspace
    // changed_paths：无 production caller（实测仅 cfg(test)），返回 Ok(vec![])
}

/// #59 过渡期唯一构造点；#60 连同本文件一起删除。
pub fn direct_local_routing(root: &Path) -> Result<ExecutionRouting, LancetError> {
    let ws = Arc::new(LocalWorkspace::new(root)?);
    Ok(ExecutionRouting {
        backend: Arc::new(LocalExecutionBackend(ws)),
        upper: UpperContext {
            execution_backend: "direct-local".into(),
            // 过渡值：填真实 canonical root，audit 不再声称 Upper。#60 删字段。
            upper_workspace_id: root.display().to_string(),
        },
    })
}
```

## 3. `lancet-api`：runtime 内部构造，删公共注入面

- `LancetRuntimeConfig` **删除 `execution` 字段**（ADR Module Seams：
  "`LancetRuntimeConfig` no longer receives `ExecutionRouting`"）。
- `LancetRuntime::build`：
  `let execution = lancet_native::direct_local_routing(&policy.project_root)?;`
  塞进 `ServerRunConfig`（`ServerRunConfig.execution` 字段保留到 #60）。
- 改写 `runtime_config_defaults_have_null_execution` 测试 → 断言 build 出的
  routing 为 `direct-local`。
- `NullExecutionBackend` 自此无 production 引用，留给 #60 删。

## 4. `lattice-api`：切换并删除 Upper 链（no-compat 硬切）

实测删除波及面（2026-07-17 复核）比票面直觉大：`turn/mod.rs` 公开
`promotion`/`resume`/`snapshot_journal`/`upper`/`workspace_backend` 五个模块与
`promote_verified_candidate`；`lib.rs` 对外 re-export upper/promotion/
lifecycle/lattice-workspace 四组符号；production 执行路径 `live.rs` 直接引用
lifecycle 终态分类与 `UpperPrepareError`；`lattice-cli` 有两个建立在 retained
snapshot 上的子命令。按下述清单处理，全部在本票完成（编译强制，无法推迟）。

### 4.1 模块删除与裁剪

整文件删除：

- `turn/upper.rs`（`prepare_upper`/`UpperLease`/`resolve_upper_config`/
  credential/egress 翻译）
- `turn/workspace_backend.rs`（`WorkspaceExecutionBackend`）
- `turn/promotion.rs`（~553 行）+ `turn/mod.rs` 的 `promote_verified_candidate`
  （guest-git promotion 已被 ADR 0001/0003 双重否决）
- `turn/resume.rs`（~449 行：`ResumeMode`/`ResumeError`/`resume_turn`/
  `resolve_resume_mode`/`prune_session_snapshots`——全部绑 retained snapshot）
- `turn/snapshot_journal.rs`（~391 行；ADR step 3 点名 snapshot
  journal/reconciliation）

裁剪 `turn/lifecycle.rs`（~468 行，**不能整删**——`live.rs` 的终态分类在用）：

- 保留：`TurnLifecycleOutcome`、`LifecycleFailureKind`、
  `classify_lifecycle_failure`、`outcome_workspace_less`、`finalize_turn`、
  `ResourceAction`、`UnfinalizedTurn`（Turn commit 终态证据，与 Upper 无关）
- 删除：`run_terminal_verification`、`outcome_promoted`、
  `commit_promoted_with_retry`、`WorkspaceDirtyHook`、
  `PromotionLifecycleError`、`PromotionSessionMetadata`、`PromotionTurnTarget`

production 代码同步修改：

- `live.rs`：删 `use super::upper::UpperPrepareError` 与对应错误分支；删
  `upper_lease` 持有与 teardown 序（约 :258-:348 的 lease
  destroy/retain 分支）
- `RunTurnRequest.workspace_override` 字段；`ProductionTurnDeps.upper_lease`
  字段
- `build_production_turn_deps` 中 `resolve_upper_config`/`prepare_upper`/lease
  destroy 分支——runtime 构建失败不再有 Upper 需要清理

```rust
// turn/cep.rs（#59 后）
let runtime = LancetRuntime::build(LancetRuntimeConfig {
    project_root: PathBuf::from(project_root),
    enabled_tools: enabled_tools.to_vec(),
    policy,            // policy.project_root 即 canonical root，runtime 内部据此建 LocalWorkspace
    cep_mode,
    audit,
    model_id: model_id.to_string(),
    runtime_version: runtime_version.to_string(),
})
.map_err(map_lancet_err)?;
```

### 4.2 公共 API 收缩（`lib.rs` + `turn/mod.rs`）

- `lib.rs:31-33`：`CandidateExport`、`PinnedCandidate`、`PromotionLimits`、
  `PromotionOutcome`、`VerificationResult`（lattice-workspace re-export）
- `lib.rs:40/:46` 区段：`PromotionLifecycleError`、`PromotionSessionMetadata`、
  `PromotionTurnTarget`、`prepare_promotion_target`、
  `promote_preexported_candidate`、`retry_pinned_candidate`、
  `outcome_promoted`、`run_terminal_verification`、`commit_promoted_with_retry`
  （保留 `LifecycleFailureKind`、`outcome_workspace_less` 等终态分类符号）
- `lib.rs:53`：`resolve_upper_config`、`ResolvedUpperConfig`、
  `UpperPrepareError`
- `turn/mod.rs`：`pub mod promotion/resume/snapshot_journal/upper/
  workspace_backend` 五行、`pub use promotion::{..}`、`pub use resume::{..}`、
  `pub use lifecycle::{..}` 按上述保留/删除名单裁剪

### 4.3 CLI 命令面（编译强制，本票内完成）

- `lattice-cli/commands/session.rs`：`resume_turn`（:753）与
  `prune_session_snapshots`（:820）两个子命令及其渲染/参数面整删——retained
  snapshot 实体已不存在
- `lattice-cli/commands/run_tests.rs` 内嵌测试：删
  `DeterministicWorkspace`/`workspace_override` 用法，改 temp root；连带删
  `lattice-cli` 对 `lattice-workspace` 的依赖声明

### 4.4 测试处置表

| 文件 | 处置 |
|---|---|
| `tests/turn_target_tests.rs` | 裁剪：`resolve_upper_config`/`UpperPrepareError` 用例删，`resolve_turn_target` 其余用例保留 |
| `tests/turn_lifecycle_tests.rs` | 删（全部绑 promotion/verification/`DeterministicWorkspace`） |
| `tests/resume_tests.rs` | 删（绑 `SnapshotRef`/`UpperSpec`；direct-local session-reopen 场景由 #62 harness 骨架与 #46 重建） |
| `tests/upper_routing.rs`、`tests/promotion_wiring.rs` | 删（契约已不存在） |
| `tests/consistency.rs`、`tests/release_e2e.rs`、`tests/usage_turn.rs` | 机械改：删 `workspace_override: None` 字段行 |
| `tests/support/mod.rs` | 删 `deterministic_workspace` helper 与自动注入分支 |
| `tests/cep_round_trip.rs`、`tests/audit_turn_provenance.rs`、`tests/dogfood_impact_matrix.rs` | 改 temp Local root 直跑（不再需要 override） |
| `src/turn/{test_support.rs, run_tests.rs, live_tests.rs, cep.rs(turn_wiring)}` | 删 override/`DeterministicWorkspace` 用法，temp root 直跑 |

- `[workspace]` profile 段：schema 保持必填但 runtime 完全不读（#62 删）。
  `profile_with_workspace` 测试 helper 继续存在。
- Cargo.toml：本票结束时 `lattice-api` 与 `lattice-cli` 对 `lattice-workspace`
  的依赖应清零并删除声明；crate 本体删除仍归 #62。

## 5. 验收 → 测试映射

| 验收 | 测试 |
|---|---|
| read/list/grep/find 看到调用时状态与外部修改 | runtime 级：Tool call 之间外部 `std::fs::write`，下一次 observe 可见 |
| create 竞争 → Replan Required | 预先建目标文件再 `mutate.create` |
| edit precondition + 原子安装 | stale hash → ReplanRequired 且目标未变；成功后内容原子替换 |
| 成功 mutation 不回滚 | mutation 成功后 LLM 步骤失败，断言文件仍在 |
| fetch.web/todo_write/catalog 五方法 | 现有 smoke 沿用 + `ToolExposure` 断言 |
| policy/Guard L1/effect/audit 覆盖 | 现有 effect_contract_tests 全绿 |
| 不联系 KVM/MSB/OCI/guest Git | 无 `[workspace]` 运行时依赖 + 测试环境无 msb 二进制仍全绿 |
| audit 记录真实 Local 证据 | 断言 metadata `execution_backend == "direct-local"` 且含真实路径/hash |
| focused + product tests | 上述全部 |
| adapter crate-internal | `direct_local_routing` 是唯一导出；`LocalExecutionBackend` 不可见 |
