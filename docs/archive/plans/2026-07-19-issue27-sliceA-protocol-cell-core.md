# #27 切片 A：`lancet-protocol` Cell effect 封闭核

- 父计划：`2026-07-19-issue27-wasm-component-tool-substrate.md` §2
- add-only：新模块 `cell.rs` 与 `effect_types.rs` 并存；旧 `ImpactKind` 族本切片零改动
  （production handler 仍消费，删除归 #28/#34）。

## 新文件 `crates/lancet-protocol/src/cell.rs`（核心类型示例）

```rust
// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell effect model (ADR 0004 / spec 2026-07-01 §5).
//!
//! Closed core: `CtxOp` × `ExtImpact` over tool-declared scopes with
//! fail-closed trust. This vocabulary is additive; the legacy
//! `ImpactKind` machinery in `effect_types.rs` is retired by #28/#34.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Effect of an operation on the agent context, managed like memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CtxOp {
    /// Result enters context (charged against the context budget by #28).
    Alloc,
    /// Context release. Vocabulary only this cycle (PRD R10): no ToolHost seam.
    Free,
    /// Mutates existing context state (not charged).
    Mutate,
}

/// Effect of an operation on the external world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtImpact {
    None,
    Read,
    Write,
}

/// Trust boundary. Absence of an explicit `in` declaration means `ex`
/// (fail-closed): a deny-`ex` turn rejects the capability.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    In,
    #[default]
    Ex,
}

/// Physical data-flow channel a scope is served by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Workspace,
    Network,
    Session,
}

/// A tool-declared scope: open vocabulary, closed attributes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopeDecl {
    pub name: String,
    #[serde(default)]
    pub trust: Trust,
    pub channel: ChannelKind,
}

/// One dense effect cell: `ctx_op × ext_impact × scope-name`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub ctx_op: CtxOp,
    pub ext_impact: ExtImpact,
    pub scope: String,
}

/// A capability names one cell; one cell may carry several capability
/// aliases (admission is per-capability, enforcement per-cell).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub name: String,
    pub cell: Cell,
}
```

## Cell reference 与 audit key：严格分层

- manifest 与 guest result 使用 **3 段 trust-free CellRef**：`ctx:ext:scope`。
- host 从 descriptor scope 派生 trust，audit 才形成 **4 段 key**：`ctx:ext:scope:in|ex`。
- guest 永远不能自行声称 `in`。

```rust
impl Cell {
    pub fn reference(&self) -> String {
        let ctx = match self.ctx_op {
            CtxOp::Alloc => "alloc",
            CtxOp::Free => "free",
            CtxOp::Mutate => "mutate",
        };
        let ext = match self.ext_impact {
            ExtImpact::None => "none",
            ExtImpact::Read => "read",
            ExtImpact::Write => "write",
        };
        format!("{ctx}:{ext}:{}", self.scope)
    }

    /// Only the host may add trust, after descriptor lookup.
    pub fn audit_key(&self, trust: Trust) -> String {
        let trust = match trust { Trust::In => "in", Trust::Ex => "ex" };
        format!("{}:{trust}", self.reference())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CellKeyError {
    #[error("cell reference must have 3 segments, got {0}")]
    SegmentCount(usize),
    #[error("unknown segment value: {0}")]
    UnknownSegment(String),
}

/// Used by manifest parsing and guest result decoding.
pub fn parse_cell_ref(key: &str) -> Result<Cell, CellKeyError> {
    let parts: Vec<&str> = key.split(':').collect();
    let [ctx, ext, scope] = parts.as_slice() else {
        return Err(CellKeyError::SegmentCount(parts.len()));
    };
    let ctx_op = match *ctx {
        "alloc" => CtxOp::Alloc,
        "free" => CtxOp::Free,
        "mutate" => CtxOp::Mutate,
        other => return Err(CellKeyError::UnknownSegment(format!("ctx={other}"))),
    };
    let ext_impact = match *ext {
        "none" => ExtImpact::None,
        "read" => ExtImpact::Read,
        "write" => ExtImpact::Write,
        other => return Err(CellKeyError::UnknownSegment(format!("ext={other}"))),
    };
    if scope.is_empty() {
        return Err(CellKeyError::UnknownSegment("scope=<empty>".into()));
    }
    Ok(Cell { ctx_op, ext_impact, scope: (*scope).to_owned() })
}
```

## `ToolDescriptor` + 注册期一致性校验

```rust
/// Sole host-side representation of a Source Tool manifest (ADR 0004 §2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolDescriptor {
    pub name: String,
    pub version: String,
    pub description: String,
    /// JSON Schema for params/result, generated from the manifest.
    pub params_schema: serde_json::Value,
    pub result_schema: serde_json::Value,
    pub scopes: Vec<ScopeDecl>,
    pub capabilities: Vec<Capability>,
    pub artifact: ArtifactIdentity,
    pub abi_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIdentity {
    pub sha256: String,
    /// Toolchain fingerprint recorded at build time (rustc + wit-bindgen).
    pub toolchain: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    #[error("capability {capability} references undeclared scope {scope}")]
    UndeclaredScope { capability: String, scope: String },
    #[error("duplicate capability name {0}")]
    DuplicateCapability(String),
    #[error("duplicate scope name {0}")]
    DuplicateScope(String),
}

impl ToolDescriptor {
    /// Registration-time consistency: every capability cell must root in a
    /// declared scope; names must be unique. Channels derive from scopes —
    /// there is no independent channels field (spec §11.3).
    pub fn validate(&self) -> Result<(), DescriptorError> { /* … */ Ok(()) }

    /// The channel set this tool may be handed, derived from `scopes`.
    pub fn required_channels(&self) -> std::collections::BTreeSet<ChannelKind> {
        self.scopes.iter().map(|s| s.channel).collect()
    }

    pub fn trust_of(&self, scope: &str) -> Trust {
        self.scopes
            .iter()
            .find(|s| s.name == scope)
            .map(|s| s.trust)
            .unwrap_or(Trust::Ex) // fail-closed: undeclared == ex
    }
}

/// A tool call's reported output cell + value (budget seam lands in #28).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolOutput {
    pub result_cell: Cell,
    pub value: serde_json::Value,
}
```

## 测试（`cell.rs` 内嵌 `#[cfg(test)]`）

| 用例 | 断言 |
| --- | --- |
| `undeclared_scope_defaults_to_ex` | `trust_of("nope") == Trust::Ex` |
| `serde_trust_default_is_ex` | 缺 `trust` 字段的 `ScopeDecl` 反序列化为 `Ex` |
| `cell_ref_round_trips` | `parse_cell_ref(&cell.reference()) == Ok(cell)`，全枚举笛卡尔积 |
| `cell_ref_rejects_guest_supplied_trust` | `"alloc:read:ws:in"` → `SegmentCount(4)` |
| `host_derives_audit_trust` | descriptor scope `in` → `cell.audit_key(Trust::In)`；guest 无 trust 输入 |
| `descriptor_rejects_undeclared_scope` | capability 引未声明 scope → `UndeclaredScope` |
| `descriptor_rejects_duplicate_names` | 重复 capability/scope 名 → 对应错误 |
| `multi_cell_call_representation` | 一次调用 `Vec<Cell>` 含 `(Mutate,None,session)` + `(Alloc,None,session)` 可表示（spec §5.1 todo_write 例） |

## 验收

- `cargo test -p lancet-protocol` 全绿；`cargo clippy --workspace --all-targets -- -D warnings`。
- `effect_types.rs`/现有 re-export 零改动（`git diff` 只含新文件 + `lib.rs` 一行 `pub mod cell;`）。
