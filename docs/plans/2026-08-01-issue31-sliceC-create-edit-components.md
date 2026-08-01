# #31 切片 C：mutate.create / mutate.edit Component Tools

- 父计划：`2026-08-01-issue31-mutate-component-tools.md`
- 票：#31 · 决议 D1/D2/D5（经 Standards/Spec 审阅修订）
- 前置：切片 A（workspace-mutate host）、切片 B（MutationEvidence + SQLite 迁移）

## Goal

两个独立 Source Tools + 共享 `tools/mutate-shared`；删除 native mutation
handlers 与 catalog Create/Edit；Component 成为唯一 production path。

## Layout

```text
tools/
  mutate-shared/     # no_std 纯语义库（无 WIT、无 I/O）
  mutate-create/     # imports = ["workspace-mutate"]
  mutate-edit/       # imports = ["workspace-mutate"]
```

Guest **不**传 HashLine 字符串；经 `create-if-absent` /
`replace-with-expected` 安装。但 HashLine 路径上的**行为不变量必须保留**
（见 Shared lib）——丢弃传输格式 ≠ 丢弃语义。

## Cells（Spec P0 — 精确授权）

Host 对 operation 做精确 Cell 授权。观察侧是 `alloc:read:workspace`；变更
**operation** 必须是 `mutate:write:workspace`，否则 admission 拒绝写 import。
Result 进入 context 的度量仍用 `alloc:write:workspace`。

```toml
[[capabilities]]
name = "workspace.mutation"
cell = "mutate:write:workspace"

[[capabilities]]
name = "workspace.mutation.result"
cell = "alloc:write:workspace"
```

- 每个 host mutate import 绑定 **operation** cell `mutate:write:workspace`
- Guest `ToolOutput.result_cell` = `alloc:write:workspace`
- Evidence：operation effect / denial 用 mutate cell；result effect 用 alloc cell

禁止只声明一个 `alloc:write:workspace` 却拿它去授权 mutate import。

## Shared lib — 保留现有 behavior（Spec P1 / 验收 4）

从 `lancet-native` mutation/edit 迁出并对齐**现有测试与错误文案**：

| 行为 | 来源 | 落点 |
|---|---|---|
| semantic replace / insert + occurrence | `apply_semantic_edit_operations` / `find_semantic_occurrence` | `mutate-shared` |
| ambiguous / missing anchor diagnostics | 现 wire 文案 | `mutate-shared` |
| no-op（before == after）→ validation error | `compile_update_fragment` | `mutate-shared`（apply 后检查） |
| trailing newline 必须保持 | 同上 | `mutate-shared` |
| 空文件上的 insert 限制与文案 | `render_insertion_hunk` | `mutate-shared`（同等拒绝） |
| WriteLimits 字节上限 | `WriteLimits` / #120 | **host** `create-if-absent` / `replace-with-expected` 安装前 |

```rust
// tools/mutate-shared/src/lib.rs
#![no_std]
extern crate alloc;

pub fn apply_edits(before: &str, edits: &[EditOp]) -> Result<String, EditDiag> {
    let after = /* semantic ops */;
    if before == after {
        return Err(EditDiag::NoChanges);
    }
    if trailing_newline(before) != trailing_newline(&after) {
        return Err(EditDiag::TrailingNewlineChanged);
    }
    // empty-file insert: same refusal as today
    Ok(after)
}
```

迁移时把 native 里覆盖上述行为的单测一并搬到 `mutate-shared`（或共享 fixture），
防止“格式换了、语义丢了”。

## Manifests

### mutate.create

```toml
imports = ["workspace-mutate"]

[tool]
name = "mutate.create"
version = "0.1.0"
description = "Create a new file under the workspace write scope. The target path must be permitted by policy: use a 'dir/**' pattern to cover a directory. The target file must not already exist. Set create_parents=true to create missing parent directories required for path; otherwise parent directories must already exist."
abi-version = 1
distribution = "builtin"

[params]
schema = '''
{
  "type": "object",
  "properties": {
    "path": { "type": "string", "minLength": 1 },
    "content": { "type": "string" },
    "create_parents": { "type": "boolean" }
  },
  "required": ["path", "content"],
  "additionalProperties": false
}
'''

[result]
schema = '''
{
  "type": "object",
  "properties": {
    "path": { "type": "string" },
    "bytes_written": { "type": "integer", "minimum": 0 },
    "created_dirs": {
      "type": "array",
      "items": { "type": "string" }
    }
  },
  "required": ["path", "bytes_written"],
  "additionalProperties": false
}
'''

[[scopes]]
name = "workspace"
trust = "in"
channel = "workspace"

[[capabilities]]
name = "workspace.mutation"
cell = "mutate:write:workspace"

[[capabilities]]
name = "workspace.mutation.result"
cell = "alloc:write:workspace"

[artifact]
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
toolchain = "rustc-1.96.0+wit-bindgen-0.59+wasm-tools-1.254.0"
```

Descriptor vocabulary = Cell / scopes / capabilities only。**禁止**在 Component
descriptor 中引入或复述 legacy `FailureSemantics` / `ImpactKind` catalog 字段
（Standards #3 / ADR 0004）。原子性与“安装成功不回滚”由 host Structured
Workspace Operations + history 强制，不靠 descriptor 枚举声明。

### mutate.edit

Params/result 对齐现 catalog `EditParams` / `EditResult`。description 保留
“copy observe.workspace content_hash into expected_hash”。capabilities 与
create 相同（mutate + alloc write cells）。

## Guest 示意（Standards #2 — 禁止 stale 短路）

Guest **不得**在比较 `local_revision != expected_hash` 后直接 `return Err`。
那样 host 的 `replace-with-expected` 不会运行，precondition 无法强制，stale
evidence 也无法写入。修订后的流程：

```rust
// tools/mutate-edit/src/lib.rs
fn run(params_json: String) -> Result<ToolOutput, String> {
    if host_control::cancelled() { return Err("cancelled".into()); }
    let params: EditParams = serde_json::from_str(&params_json).map_err(|e| e.to_string())?;

    // Always go through host for bytes + authoritative revision.
    let file = mutate::read_for_edit(&params.path).map_err(describe)?;
    let after = mutate_shared::apply_edits(&file.content, &params.edits)
        .map_err(describe_diag)?;

    // Host rechecks expected_hash, records MutationEvidence / DeniedOperation,
    // and only then installs. Guest does not short-circuit on mismatch.
    let installed = mutate::replace_with_expected(
        &params.path,
        &params.expected_hash,
        &after,
    ).map_err(describe)?;

    Ok(ToolOutput {
        result_cell: "alloc:write:workspace".into(),
        value_json: json!({ "changed_files": [params.path] }).to_string(),
    })
}
```

`describe` 把 host `stale-revision` / `already-exists` 映到对模型可见的
Replan 文案；权威记录仍在 host evidence。

Create guest 同理：只调 `create-if-absent`，不在 guest 侧重做存在性断言来替代
host。

## Codegen / composition / bindings

1. `cargo xtask tool-codegen` + `tool-build`
2. `bindings.rs` 改绑最宽 world（含 `workspace-mutate`）
3. `EXPECTED_BUILTIN_TOOLS` → observe / create / edit / todo_write
4. composition gate：无 native/Component 撞名
5. admission 单测：`mutate:write:workspace` 授权 create/replace import；
   仅有 `alloc:write:workspace` 时 import 被拒

## Native / catalog 拆除

- 删 catalog `Create`/`Edit`；仅剩 `fetch.web`（至 #32）
- 删 `register_mutation_methods` 与 mutation production 路径
- 全库 “native mutate” 测试改走 Component 或 `fetch.web`
- 不把 `FailureSemantics` 搬进 Component descriptor

## Verification

```bash
cargo xtask tool-codegen --check
cargo xtask tool-build
cargo test -p lancet-catalog
cargo test -p lancet-native
cargo test -p lancet-runtime --test embedded
cargo test -p lattice-api
# mutate-shared behavior pins:
cargo test -p mutate-shared   # or whatever package name xtask uses
```

## Out of scope

- 系统级 harness 矩阵扩写（D）
