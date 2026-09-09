# #31 切片 A：`workspace-mutate` Tool Host

- 父计划：`2026-08-01-issue31-mutate-component-tools.md`
- 票：#31 · 决议 D1 已确认
- 前置：无（可先于 B；C 依赖本切片）
- 本切片**不**交付 production Component Tool

## Goal

落地闭集 domain import `workspace-mutate` 与 host 实现，使 guest（测试用
fixture 或后续 mutate Tools）只能通过四个 Structured Workspace Operations
触达 Local Workspace：`read-for-edit`、`create-if-absent`、
`replace-with-expected`、非安装的 `refuse-edit`。


## Host authority (Standards #2)

`read-for-edit(expected-revision)`、`replace-with-expected` / `create-if-absent`
是 precondition 与 evidence 的唯一权威：前者在 guest 编译前拒绝 stale，后者
在安装点最终重检。Guest semantic diagnostics 通过 `refuse-edit` 回到 host，
不安装任何 bytes，并保留 `SchemaMismatch` / `ReplanRequired`。

Operation 授权 Cell 在切片 C 钉为 `mutate:write:workspace`；本切片 fixture
descriptor 须声明该 cell，否则精确授权会拒写。

## Deliverables

1. `xtask` `DomainImport::WorkspaceMutate` + WIT 常量 + codegen 渲染
2. `lancet-runtime` mutate host（对齐 `host_workspace` observe 模式）
3. `StoreState` WIT `Host` 实现；错误映射到 CEP `ReplanRequired` /
   path denial / limit
4. Host bindings 指向含 `workspace-mutate` 的最宽 world（可用临时 fixture
   Tool 或先只扩展 codegen，待切片 C 换绑）
5. Unit tests：create 成功 / 已存在 / 竞争、edit 成功 / stale、path denial、
   fault injection 安装前失败零效果、history append on success

## WIT（写入 `tool_codegen.rs`，与总控 D1 一致）

```rust
// xtask/src/tool_codegen.rs
enum DomainImport {
    WorkspaceObserve,
    WorkspaceMutate, // NEW
}

impl DomainImport {
    fn wit_interface(self) -> &'static str {
        match self {
            Self::WorkspaceObserve => "workspace-observe",
            Self::WorkspaceMutate => "workspace-mutate",
        }
    }
}
```

`composition::channels_for_domain_imports`：`workspace-mutate` →
`ChannelKind::Workspace`（与 observe 同通道）。

## Host 骨架

```rust
// crates/lancet-runtime/src/host_mutate.rs
pub struct WorkspaceMutationHost {
    workspace: Arc<LocalWorkspace>,
    // write-deny / authorized region / write limits from call admission
}

impl WorkspaceMutationHost {
    pub fn read_for_edit(
        &self,
        ctx: &MutCallCtx<'_>,
        path: &str,
    ) -> Result<FileBytes, MutateError> {
        // PathIntent::Read (or Write-scoped read): resolve_secure, refuse
        // symlink/escape/system exclusion; read bytes; content_hash.
        // Record DeniedOperation on refuse; no MutationEvidence yet.
    }

    pub fn create_if_absent(
        &self,
        ctx: &MutCallCtx<'_>,
        path: &str,
        content: &str,
        create_parents: bool,
    ) -> Result<CreateInstalled, MutateError> {
        // preflight Create intent + write-deny
        // LocalWorkspace::create_excl(..., create_parents)
        // TargetExists → MutateError::AlreadyExists
        // on Ok: record_mutation history (#63); MutationEvidence wired in B
    }

    pub fn replace_with_expected(
        &self,
        ctx: &MutCallCtx<'_>,
        path: &str,
        expected_revision: &str,
        content: &str,
    ) -> Result<ReplaceInstalled, MutateError> {
        // resolve + read current hash
        // if current != expected → StaleRevision, no write
        // else install_atomic; history with before/after bytes
    }
}
```

错误映射：

| `MutateError` | CEP |
|---|---|
| `AlreadyExists` / `StaleRevision` | `ReplanRequired` |
| `PathRejected` / system exclusion / write-deny | existing Guard/policy denial codes |
| `LimitExceeded` | write size limit (reuse #120 messaging) |
| `Cancelled` | `CallCancelled` (#70) |
| `Io` | tool execution / IO mapping |

## History

复用 `WorkspaceHistory::record`，输入对齐现 native `MutationRecord`。
History 失败不得回滚已安装 mutation。

## Tests

```bash
cargo test -p lancet-runtime host_mutate
```

1. `create_if_absent` 成功 → 文件存在 + history recorded（若启用）
2. 目标已存在 → `AlreadyExists`，字节不变
3. `replace_with_expected` 正确 revision → 原子替换 + 新 revision
4. stale revision → 目标不变
5. symlink / `..` / write-deny → 拒绝且无写
6. install fault injection → 无部分文件

## Out of scope

- `MutationEvidence`（B）· Component manifests（C）· harness（D）· 删 native（C）
