# #31 切片 B：MutationEvidence（protocol + SQLite additive 迁移）

- 父计划：`2026-08-01-issue31-mutate-component-tools.md`
- 票：#31 · 决议 D3（经 Standards/Spec 审阅修订）
- 前置：可与 A 并行；C 的 host 接线依赖本切片类型
- 对齐 #30：`truncated` / visibility 的 **additive 列迁移** 模式，不破坏旧库可读性

## Goal

让 create/edit 的 before/after Local Revision 与 precondition/installation
outcome 以 `lancet_protocol::evidence` 显式字段承载，并写入 #112 交付的
versioned evidence 表（分列、可查询）；禁止平行结构，禁止塞进 audit
`metadata_json`。

**成功路径**与**失败路径**都必须能显式读出 revision / precondition /
installation（验收 7/11/12）。

## Protocol 变更（additive Options + 枚举；不 bump record version）

> Standards #1：当前 `check_version` 只接受精确版本。若把
> `OBSERVED_EFFECTS_RECORD_VERSION` 升到 2 而不做 v1/v2 分支读取，旧库会全部
> `UnsupportedRecordVersion`。本切片采用与 #30 `truncated` **相同**的策略：
> **保持** `OBSERVED_EFFECTS_RECORD_VERSION == 1` 与
> `DENIED_OPERATIONS_RECORD_VERSION == 1`；新字段为 `Option` /
> `skip_serializing_if`；靠 `EVIDENCE_SCHEMA_VERSION` 与 `ALTER TABLE` 迁移列。

```rust
// crates/lancet-protocol/src/evidence.rs
// DO NOT bump OBSERVED_EFFECTS_RECORD_VERSION / DENIED_OPERATIONS_RECORD_VERSION.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MutationPrecondition {
    Satisfied,
    StaleRevision,
    TargetExists,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MutationInstallation {
    Installed,
    NotAttempted,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MutationEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_revision: Option<String>,
    /// Post-install revision when installed; for denials may be absent or the
    /// observed current revision (stale case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_revision: Option<String>,
    pub precondition: MutationPrecondition,
    pub installation: MutationInstallation,
}

pub struct ObservedEffect {
    // existing fields unchanged...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation: Option<MutationEvidence>,
}

pub struct DeniedOperation {
    pub seq: u32,
    pub cell: Cell,
    pub scope: ObservedScope,
    pub target: ObservedTarget,
    pub reason: String,
    /// Explicit mutation outcome on refused host ops (stale / exists / denied).
    /// Absent on non-mutation denials and on pre-#31 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation: Option<MutationEvidence>,
}
```

约束：

- **无** `deny_unknown_fields`
- 旧行缺列 / 缺字段 → `mutation: None`（与 #30 `truncated: None` 同语义）
- 导出更新 `lancet_protocol::lib.rs` re-exports

## SQLite 迁移（必做）

```rust
// lancet-audit/src/evidence.rs
pub const EVIDENCE_SCHEMA_VERSION: i64 = 5; // was 4
pub const EVIDENCE_SCHEMA_VERSION_PRE_MUTATION: i64 = 4;

// ADDITIVE columns (nullable), NOT in REQUIRED_* sets — same pattern as
// ADDITIVE_EVIDENCE_EFFECT_COLUMNS for truncated/visibility.
// evidence_effects:
//   mutation_before_revision TEXT
//   mutation_after_revision  TEXT
//   mutation_precondition    TEXT  -- snake_case enum label
//   mutation_installation    TEXT
// evidence_denials: same four columns
```

迁移步骤（对齐 #30）：

1. 打开库时若 `user_version` ∈ {2,3,4} 且表存在 → `ALTER TABLE ... ADD COLUMN`
   对 effects 与 denials 各加四列（缺哪加哪）。
2. stamp `EVIDENCE_SCHEMA_VERSION = 5`。
3. 已是 v5 的库：对 required ∪ additive 做 shape 断言，缺列则报错不改写。
4. 读写路径：`INSERT`/`SELECT` 带新列；旧行 NULL → protocol `None`。
5. **不**把 protocol record version 写成 2；`effects_version` /
   `denials_version` 列继续写 `1`。

Corrupt enum label → `UnreadableField`（与现有 ctx_op 等一致），不静默 coerce。

## 写入路径

Host 是唯一写入者。`read-for-edit(expected-revision)` 在 guest 编译前由 host
记录 stale denial；成功编译后 `replace-with-expected` 在安装操作内再检查一次。

```rust
// success install — operation cell is mutate:write:workspace (slice C)
evidence.record_effect(ObservedEffect {
    seq,
    cell: mutate_write_cell,      // mutate:write:workspace
    scope: workspace_scope,
    target: ObservedTarget::Path { path: resolved.into() },
    source: EffectSource::Operation,
    context_measure: None,       // operation effect returns no value (issue #28 D12)
    truncated: None,
    visibility: None,
    mutation: Some(MutationEvidence {
        before_revision,          // None for create
        after_revision: Some(post.hash),
        precondition: MutationPrecondition::Satisfied,
        installation: MutationInstallation::Installed,
    }),
});
// result effect separately uses alloc:write:workspace (measure), mutation: None

// precondition fail — no filesystem change
evidence.record_denied(DeniedOperation {
    seq,
    cell: mutate_write_cell,
    scope: workspace_scope,
    target: ObservedTarget::Path { path: requested.into() },
    reason: "stale-revision".into(),
    mutation: Some(MutationEvidence {
        before_revision: Some(observed_current_revision),
        after_revision: None,
        precondition: MutationPrecondition::StaleRevision,
        installation: MutationInstallation::NotAttempted,
    }),
});
```

`TargetExists` / policy `Denied` 同形：`installation = NotAttempted`，
`precondition` 对应枚举变体。

## Tests

```bash
cargo test -p lancet-protocol evidence
cargo test -p lancet-audit evidence
```

1. `MutationEvidence` / 枚举 round-trip；缺字段旧 JSON 仍可 parse
2. v4 库打开 → ALTER 加列 → stamp v5；旧 effects 行 `mutation == None`
3. create success：effects 行四列有值；`before_revision` NULL；installation=installed
4. stale edit：denials 行带 mutation 四列；effects 无 installed mutation；文件字节不变
5. 查询走分列，不依赖 `metadata_json`
6. 非法 enum label → `UnreadableField`

## Out of scope

- WIT / Component guests（C）· harness 产品矩阵（D）
- 不改 `AdmissionEvidence` version
- 不做 `OBSERVED_EFFECTS_RECORD_VERSION = 2`（若未来真要 bump，另开票做
  双版本 reader；本票禁止无分支的硬升）
