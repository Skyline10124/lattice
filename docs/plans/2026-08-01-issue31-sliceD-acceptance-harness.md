# #31 切片 D：验收场景与 harness

- 父计划：`2026-08-01-issue31-mutate-component-tools.md`
- 票：#31 验收标准收口
- 前置：切片 A+B+C 已合入本工作树分支

## Goal

产品级证明：embedded Components 直接修改 Local Workspace；precondition /
policy / trap / cancel 失败不改目标；安装成功后 session/Tool failure 不撤销；
history + Rewind contracts 保持；evidence 走 versioned 表。

## Scenario matrix

| # | 场景 | 主要落点 |
|---|---|---|
| 1 | create success | `direct_mutation` / `direct_local_acceptance` |
| 2 | create existing → Replan Required | 同上 |
| 3 | create race（双发）一胜一败 | `direct_mutation` |
| 4 | edit success（observe hash → edit） | `direct_mutation` |
| 5 | edit stale / external writer | `direct_mutation` |
| 6 | ambiguous edit（缺 occurrence） | unit + harness |
| 7 | path denial / system exclusion / write-deny | `direct_mutation` / policy |
| 8 | atomic failure injection 无部分写 | runtime + harness |
| 9 | history recorded on success | `direct_workspace_history` |
| 10 | Rewind unrelated-change / same-path-divergence | `direct_rewind` / `rewind_acceptance` |
| 11 | trap / cancellation 安装前 → 无写 | `direct_failures` |
| 12 | 成功 mutation 后 provider/session commit failure → 文件仍在 | `direct_failures` |
| 13 | audit evidence 表含 MutationEvidence 字段 | audit / harness 交叉断言 |
| 14 | Tool call evidence 非 metadata_json 平行 blob | audit query |
| 15 | no-op / trailing-newline / empty-insert diagnostics 保持 | mutate-shared + harness |
| 16 | v4→v5 evidence 库 additive 迁移可读 | lancet-audit evidence_tests |

既有 #46 slice3 场景在 Component 路径上全部回归绿，并补 evidence 断言。

## Evidence assertions（示例）

```rust
use lancet_protocol::{MutationInstallation, MutationPrecondition};

// success: operation effect carries MutationEvidence on mutate:write:workspace
let effects = load_observed_effects(call_id)?;
let write = effects.effects.iter()
    .find(|e| e.mutation.is_some())
    .expect("installed mutation effect");
assert_eq!(write.cell.reference(), "mutate:write:workspace");
let m = write.mutation.as_ref().unwrap();
assert_eq!(m.precondition, MutationPrecondition::Satisfied);
assert_eq!(m.installation, MutationInstallation::Installed);
assert!(m.after_revision.as_ref().unwrap().starts_with("blake3:"));

// stale: DeniedOperation.mutation explicit; no installed mutation effect
let denied = load_denied_operations(call_id)?;
let d = denied.denied.iter().find(|d| d.mutation.is_some()).unwrap();
let dm = d.mutation.as_ref().unwrap();
assert_eq!(dm.precondition, MutationPrecondition::StaleRevision);
assert_eq!(dm.installation, MutationInstallation::NotAttempted);
assert!(dm.before_revision.as_ref().unwrap().starts_with("blake3:"));
// columns live in evidence_denials — not metadata_json
```

另测：打开 v4 旧库 → 迁移到 EVIDENCE_SCHEMA_VERSION 5；旧行 `mutation == None`。
另测：no-op edit / trailing-newline 破坏 / 空文件 insert → 与 native 同等拒绝。

## Docs / AGENTS 收尾

- `docs/LANCET.md` / `ARCHITECTURE.md`：mutate 标为 Component Tools
- 相关 `crates/*/AGENTS.md`：native 残留仅 `fetch.web`
- catalog：四个 embedded + 一个 native（至 #32）

## Verification

```bash
cargo xtask tool-codegen --check
cargo xtask ci
cargo test -p lattice-api
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness \
    --test direct_mutation \
    --test direct_failures \
    --test direct_rewind \
    --test direct_workspace_history \
    -- --test-threads=1
```

## Review

全部切片完成后：`/review` 两轴；固定点建议 `4b53081b` 或用户确认点；
Spec = #31 正文 + 本系列 plan。
