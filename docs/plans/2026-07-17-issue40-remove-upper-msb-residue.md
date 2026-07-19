# #40 实现计划：删除 Upper 配置、Microsandbox 与 lattice-workspace 残留

> **⚠️ 过时词汇警告（2026-07-19）**：#40 已完成。本计划文件作为历史记录保留，
> 但仍含已退役的 KVM-lane 词汇（Microsandbox、MSB 等）。这些词汇已从 active
> 代码、machine ledger 与 `docs/TEST-TRIAGE.md` 中完全移除。本文件待重写或
> 归档以替换为当前 Direct-Local 词汇。重写工作由 codeberg issue 跟踪。

- 票：<https://codeberg.org/AstRin/LATTICE/issues/40>
- 前置依赖：#39（[计划](2026-07-17-issue39-policy-visible-read.md)）。全局约束
  与验证矩阵见 [总览](2026-07-17-direct-local-overview.md)。
- 对应 ADR 0003 Migration Sequence step 4 的 `[workspace]` 删除 + step 5 +
  step 6 的骨架部分（完整 direct-Local 场景覆盖归 #24）。

按依赖方向从外向内删，每步保持编译：

## 1. profile

`AgentProfile.workspace` 字段与 `WorkspaceSection`/`EgressPolicyConfig`/
`CredentialRef` 等类型全删；`deny_unknown_fields` 下带 `[workspace]` 的旧
profile 现在**解析失败**——这是 strict schema 的预期终态（票文："不再声明
或要求"）。同步 `docs/CONFIGURATION.md`、generated profiles/examples、
`profile_with_workspace` 类测试 helper。

## 2. lattice-api / lattice-session 残余 snapshot 面

> `turn/{resume,snapshot_journal,promotion}.rs` 模块与 `resume_tests.rs`
> 等测试已在 #37 因编译连锁一并删除（见 #37 计划 4.1/4.4）。本票剩余：

- `resolve_turn_target` 的 baseline 注入注释与残留参数。
- `lattice-session` graph 契约的 `set_snapshot_lost`/`get_snapshot_lost`
  方法、`SessionGraphStore` trait 对应条目与 `GitSessionGraphStore` 实现
  （retained-snapshot 标记链的存储侧残余）。
- `tests/{release_e2e,turn_target_tests,usage_turn}.rs` 中残余的
  `[workspace]` profile fixture（字段随第 1 步 schema 删除一并清）。

## 3. lattice-session promotion 残留

`promotion.rs` + `tests/promotion_commit.rs`（promotion residue，票文验收
第 2 条）。

## 4. 删 crate 与依赖

- `crates/lattice-workspace/` 整目录。
- 根 `Cargo.toml`：members 与 `workspace.dependencies` 移除 `lattice-workspace`、
  `microsandbox`。
- `lattice-api`/`lattice-cli` 对 `lattice-workspace` 的依赖与
  `run_tests.rs` 内嵌测试已在 #37 清理（见 #37 计划 4.3），此处仅
  `cargo tree -i lattice-workspace` 验证为零后删 crate。

## 5. harness

`lattice-system-harness` 删 MSB/KVM 场景（含 no-tool MSB egress 场景）、
`layer_guard` 更新层图；补 direct-Local 场景骨架（外部竞争、policy denial、
session reopen——完整覆盖归 #24）。

## 6. docs

`AGENTS.md` 分层图删 Layer 0 的 `lattice-workspace` 与 Layer 4 边、
`docs/ARCHITECTURE.md`、受影响 crate 的 `AGENTS.md`；historical ADR/FIX
records 不重写。

## 7. 验证

- CI/本机不装 msb、无 KVM 权限下全套 gate 通过（普通 text-only Turn 与五个
  catalog methods）。
- `rg -i "microsandbox|upper|msb" crates --type rust` 复查（放行 CONTEXT/ADR
  引语与注释中的历史表述）。
- 网络 policy 回归：`[lancet.network]` 与 `fetch.web` 的 URL/DNS safety
  行为不变（删除的是 workspace egress，不是 lancet.network）。
- #24 与 #25 自此可开工。
