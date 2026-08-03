# #31 实现计划：将 mutate.create/edit 迁为 Atomic Local Mutation Component Tools

- 票：<https://github.com/Skyline10124/lattice/issues/31>
- 上级：#24
- 前置依赖（均已 CLOSED）：#30（observe Component）、#64（Rewind）、#112（versioned evidence 表）
- 工作树：`/home/astrin/Dev/lattice-wt-issue31` · 分支 `feat/issue-31-mutate-component`
- 固定对照点（开工前确认）：`main` @ `4b53081b`
- 参考实现：#30 `tools/observe-workspace/` + `lancet_runtime::host_workspace`

## 审阅修订（2026-08-01）

Standards/Spec 两轴审阅后已回写 plan（本文件 + 切片 B/C；A/D 轻触）：

1. Evidence：**不** bump protocol record version；`EVIDENCE_SCHEMA_VERSION` 5 + additive 列；denial 也带 `MutationEvidence`
2. Guest 不自行分类 stale：host `read-for-edit(expected-revision)` 在编译前拒绝，
   `replace-with-expected` 在安装操作内最终重检；semantic diagnostics 走
   非安装的 `refuse-edit`
3. Descriptor **禁止** legacy `FailureSemantics`；只用 Cell vocabulary
4. Operation cell=`mutate:write:workspace`，result cell=`alloc:write:workspace`
5. 丢弃 HashLine **传输**但保留 no-op / trailing newline / empty-insert / WriteLimits 行为
6. `precondition`/`installation` 改为可版本化枚举


## 0. 现状与目标边界

**当前：**

- `mutate.create` / `mutate.edit` 仍是 `lancet-catalog` 原生方法，由
  `lancet-native::handlers::mutation` 执行。
- 安装已经走 `LocalWorkspace::{create_excl, install_atomic}`（经
  `EditStorage`）；语义 edit / HashLine compile 仍在 native handler。
- Embedded Components 仅有 `observe.workspace` 与 `session.todo_write`。
- Domain import 词汇只有 `workspace-observe`（`xtask` `DomainImport`）。
- Host bindings 绑定最宽 WIT：`tools/observe-workspace/generated/tool-host.wit`。
- `ObservedEffect` 有 path / truncated / visibility，但没有 before/after Local
  Revision 或 precondition/installation outcome 字段。

**目标：**

- 两个独立 Source Tool（两个 descriptor / 两个 Component），共享 guest 侧语义库。
- Component 解析 structured edit、计算候选内容、组织 diagnostics；**不能**直接写
  filesystem。
- 所有 mutation 只经 Tool Host Structured Workspace Operations 提交给 concrete
  Local Workspace。
- Host 继续强制：Local Revision、Mutation Preconditions、System Path Exclusion、
  write denial、effect region、atomic installation、durable audit、private gix
  history、Rewind contracts。
- Native mutation handlers / catalog Create·Edit / native fallback 删除；
  Component 是唯一 production path。
- Tool call evidence 使用 `lancet_protocol::evidence` 显式字段，写入 #112
  versioned evidence 表；不塞进 `metadata_json` blob。

**不在范围：**

- `mutate.batch`（#37 已退役）
- `fetch.web` 迁移（#32）
- Whole-workspace snapshot / automatic merge / Agent commands / user processes
- 弱化 host-side path 或 precondition checks

---

## 1. 关键技术决策（已确认 2026-08-01）

> 用户确认 D1–D4 全部采用推荐项；D5 按票面与现行为锁定。
> 切片独立 plan：`2026-08-01-issue31-slice{A,B,C,D}-*.md`。

### D1. Domain import 形状 — **已确认：新建 `workspace-mutate`**

与 `workspace-observe` 对称，新增闭集 `DomainImport::WorkspaceMutate`，不把写
操作塞进 observe 接口。

```wit
interface workspace-mutate {
    variant mutate-error {
        not-found(string),
        already-exists(string),
        stale-revision(string),
        semantic-replan(string),
        validation(string),
        path-rejected(string),
        denied(string),
        limit-exceeded(string),
        cancelled(string),
        io-error(string),
        invalid-request(string),
    }

    enum edit-refusal-kind { validation, replan }

    record file-bytes {
        /// Exact Local bytes (UTF-8 text Tools only, same as today).
        content: string,
        /// Host-computed `blake3:<64 hex>` Local Revision.
        local-revision: string,
    }

    record create-result {
        path: string,
        bytes-written: u64,
        /// Post-install Local Revision.
        local-revision: string,
        created-dirs: list<string>,
    }

    record replace-result {
        path: string,
        bytes-written: u64,
        /// Post-install Local Revision of the installed candidate.
        local-revision: string,
    }

    /// Host validates the planning revision before guest edit compilation.
    read-for-edit: func(
        path: string,
        expected-revision: string,
    ) -> result<file-bytes, mutate-error>;

    /// ADR 0003 decision 8: O_EXCL / no-replace. Existing or raced target →
    /// `already-exists` → host maps to Replan Required.
    create-if-absent: func(
        path: string,
        content: string,
        create-parents: bool,
    ) -> result<create-result, mutate-error>;

    /// Recheck `expected-revision` against current Local bytes, then
    /// tempfile→fsync→rename atomic replace. Stale → `stale-revision` →
    /// Replan Required; target unchanged.
    replace-with-expected: func(
        path: string,
        expected-revision: string,
        content: string,
    ) -> result<replace-result, mutate-error>;

    /// Preserve semantic validation/replan class without installing bytes.
    refuse-edit: func(
        path: string,
        kind: edit-refusal-kind,
        field: string,
        reason: string,
    ) -> mutate-error;
}
```

备选（不推荐）：扩展 `workspace-observe` 加写函数 — 违反观察/变更通道分离，且
会强迫只读 Tool 的 world 变宽。

### D2. Guest 共享语义库位置 — **已确认：`tools/mutate-shared/` + 候选 bytes**

两个 guest crate 共享：

- semantic replace / insert occurrence 解析
- diagnostics 构造（ambiguous / missing anchor 等）
- 与现 native `apply_semantic_edit_operations` / `find_semantic_occurrence`
  行为对齐的纯函数

```text
tools/
  mutate-shared/          # no_std 纯逻辑，无 WIT、无 I/O
    Cargo.toml            # crate-type = ["lib"], no_std
    src/lib.rs
  mutate-create/
    manifest.toml
    src/lib.rs            # params → create-if-absent
  mutate-edit/
    manifest.toml
    src/lib.rs            # read-for-edit → semantic apply → replace-with-expected
```

备选：把语义库放进 `lattice-tool-prelude`（会把 edit 算法绑进所有 guest SDK）；
或继续从 `lancet-native` 引用（guest 不能依赖 host crate）。

HashLine 中间表示：guest 只产出最终候选 bytes，host `replace-with-expected`
只验证 expected-revision + atomic install。**已确认丢弃 guest→host 的 HashLine
传输**。但 HashLine 路径上的行为不变量必须保留在 `mutate-shared` / host：
no-op 拒绝、trailing newline 保持、空文件 insert 限制、WriteLimits、diagnostics
（见切片 C）——丢弃传输格式 ≠ 丢弃语义。

### D3. Evidence 扩展 — **已确认后修订（审阅 2026-08-01）**

`ObservedEffect` / `DeniedOperation` 都要能显式承载 revision + precondition +
installation。对齐 #30：
- **不** bump `OBSERVED_EFFECTS_RECORD_VERSION` / `DENIED_OPERATIONS_RECORD_VERSION`
  （reader 精确版本匹配；硬升会拒读旧库）
- 新字段为 `Option` + 可版本化枚举
- `EVIDENCE_SCHEMA_VERSION` 4→5，对 `evidence_effects` **与** `evidence_denials`
  `ALTER TABLE` 加 nullable 列
- 细节与示例见切片 B

成功安装：`ObservedEffect.mutation`（operation cell=`mutate:write:workspace`）。
Precondition 失败：`DeniedOperation.mutation`（`installation=not_attempted` +
对应 precondition 枚举），目标不变。Readers 无 `deny_unknown_fields`；旧行缺列
→ `None`。完整类型与 SQLite DDL 见切片 B。

### D4. 切片切分 — **已确认：四切片独立 plan**

按偏好不同切片写独立 plan。本文件是总控；切片文件已拆出：

| 切片 | 计划文件 | 交付 |
|---|---|---|
| A | `2026-08-01-issue31-sliceA-workspace-mutate-host.md` | `DomainImport::WorkspaceMutate` + WIT + mutate host + LocalWorkspace 接线 + unit tests（尚无 production Tool） |
| B | `2026-08-01-issue31-sliceB-evidence-mutation.md` | `MutationEvidence` + enums + EVIDENCE_SCHEMA_VERSION 5 additive 列迁移（effects+denials） |
| C | `2026-08-01-issue31-sliceC-create-edit-components.md` | 两个 Source Tools + shared lib（保留 HashLine 行为不变量）+ 双 Cell + 删 native/catalog |
| D | `2026-08-01-issue31-sliceD-acceptance-harness.md` | harness/product 场景：success、stale、race、ambiguous、path denial、fault injection、history、Rewind、trap/cancel、audit、session-commit-failure 不回滚 |

顺序：A → B → C → D。A/B 可部分并行，但 C 依赖两者。

### D5. 安装语义 — **已修订（禁止 legacy FailureSemantics 进 descriptor）**

单文件 create/edit：安装前失败 → 目标不变；安装成功后不因后续 Tool/Turn
failure 回滚。该语义由 host Structured Workspace Operations + private history
强制。
Component descriptor **只**使用 ADR 0004 Cell / scopes / capabilities 词汇；
**禁止**在 descriptor 中表达或复述 legacy `FailureSemantics` /
`ImpactKind` catalog 字段（Standards #3）。不宣称 batch AllOrNothing。

---

## 2. 架构落点（贴近现有结构）

### 2.1 Host mutate facade（对齐 `WorkspaceObservationHost`）

```rust
// crates/lancet-runtime/src/host_workspace.rs 旁新增 host_mutate.rs（或同文件分区）
pub struct WorkspaceMutationHost { /* LocalWorkspace, region, evidence, history */ }

impl WorkspaceMutationHost {
    pub fn read_for_edit(&self, ctx: &MutCallCtx<'_>, path: &str)
        -> Result<FileBytes, MutateError> { /* resolve WritePath intent, read, hash */ }

    pub fn create_if_absent(&self, ctx: &MutCallCtx<'_>, path: &str, content: &str, create_parents: bool)
        -> Result<CreateInstalled, MutateError> {
        // preflight → create_excl → record MutationEvidence + private history (#63)
    }

    pub fn replace_with_expected(&self, ctx: &MutCallCtx<'_>, path: &str, expected: &str, content: &str)
        -> Result<ReplaceInstalled, MutateError> {
        // read current hash; if != expected → stale (no write);
        // else install_atomic → evidence + history
    }
}
```

`StoreState` 实现 WIT `Host`（与 observe 相同模式）。`already-exists` /
`stale-revision` 经 `error_map` 升为 CEP `ReplanRequired`。

### 2.2 Manifest 骨架（create）

```toml
# tools/mutate-create/manifest.toml
imports = ["workspace-mutate"]

[tool]
name = "mutate.create"
version = "0.1.0"
description = "Create a new file under the workspace write scope. ..."
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
    "created_dirs": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["path", "bytes_written"],
  "additionalProperties": false
}
'''

# capabilities：operation = mutate:write:workspace；result = alloc:write:workspace
# （Spec P0 — 精确 Cell 授权；禁止只用 alloc:write 授权 mutate import）

```

`mutate.edit` 同理：params 含 `path` / `expected_hash` / `edits[]`；guest
`run` 调 `read-for-edit(expected_hash)` → shared semantic apply → 成功时
`replace-with-expected`；semantic failure 调非安装的 `refuse-edit`。

### 2.3 Guest edit 示意

Guest 不比较 revision 来分类 stale（Standards #2）：`read-for-edit` 已由 host
验证 planning revision 并记录 stale denial；安装时 `replace-with-expected`
再在同一 Local Workspace 操作内最终重检。

```rust
// tools/mutate-edit/src/lib.rs（示意，贴近 observe-workspace）
fn run(params_json: String) -> Result<ToolOutput, String> {
    if host_control::cancelled() { return Err("cancelled".into()); }
    let params: EditParams = serde_json::from_str(&params_json).map_err(|e| e.to_string())?;
    let file = mutate::read_for_edit(&params.path, &params.expected_hash)
        .map_err(describe)?;
    let after = mutate_shared::apply_edits(&file.content, &params.edits)
        .map_err(|diag| {
            let kind = if diag.replan { EditRefusalKind::Replan }
                else { EditRefusalKind::Validation };
            describe(mutate::refuse_edit(
                &params.path, kind, &diag.field, &diag.reason,
            ))
        })?;
    // Host rechecks expected_hash, records evidence, then installs.
    let _installed = mutate::replace_with_expected(
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

### 2.4 Native / catalog 拆除清单（切片 C）

- 删 `lancet_catalog::methods::workspace::{Create,Edit}` 与 `CATALOG` 注册；
  catalog 断言仅剩 `fetch.web`（直到 #32）。
- 删 `register_mutation_methods` 与 `handlers/mutation.rs` production 路径；
  edit 纯逻辑迁到 `tools/mutate-shared` 后，native `edit/` 中仅 host 仍需的
  部分（若有）再评估删除或下沉 `lancet-runtime`。
- `validate_no_component_native_collision`：两方法名只能出现在 Component 侧。
- 更新 `EXPECTED_BUILTIN_TOOLS`、AGENTS.md、docs/LANCET.md、dogfood / harness
  中 “native mutate” 假设。

### 2.5 Codegen / composition

- `xtask`：`DomainImport::WorkspaceMutate` + WIT 常量 + channel →
  `ChannelKind::Workspace`。
- Host `bindings.rs` 改为绑定**最宽** world（含 `workspace-mutate` 的 Tool，
  通常是 mutate-edit 或 mutate-create）。
- `cargo xtask tool-codegen` + `tool-build` + composition gate：四 builtin
  Tools（observe / create / edit / todo_write；noop 仍为 fixture）。

---

## 3. 验收 → 测试映射

| 票面验收 | 测试落点 |
|---|---|
| manifests 独占 create/edit contracts 并生成 artifacts | codegen check + composition gate + embedded catalog 含两名 |
| create → create-if-absent；已存在/竞争 → Replan Required | runtime unit + `direct_mutation` / `direct_local_acceptance` |
| edit：read+revision → candidate → expected-revision install；stale → Replan | 同上 + host_mutate unit |
| patch/semantic/occurrence/diagnostics 在 Component；path/install 在 host | mutate-shared 单测 + guest 不链接 wasi/fs |
| 安装前失败不改目标；安装成功不因后续 failure 回滚 | fault injection + `direct_failures` session/provider failure |
| 成功 mutation 写 private gix history；Rewind unrelated/same-path | `direct_workspace_history` / `rewind_acceptance` / `direct_rewind` |
| operation/result Cells、paths、revisions、precondition/installation → evidence | evidence 表断言（非 metadata_json blob） |
| 删 native handlers / fallback / ExecutionBackend assumptions | catalog 3→1、collision gate、compile 无 mutation handler 注册 |
| product scenario：embedded Components 改 Local；session commit fail 不撤销 | system-harness product 场景 |
| evidence 用 `lancet_protocol::evidence` 显式字段 | protocol 单测 + audit evidence_tests |

---

## 4. 建议落地顺序

1. 确认 §1 D1–D5。
2. 拆写切片 A–D 独立 plan（示例代码级）。
3. 按 A→B→C→D 在本工作树 TDD 实现；每切片可独立 commit。
4. 全量 `cargo test` + harness；`/review`（Standards / Spec 两轴，固定点
   `4b53081b` 或切片起点）。
5. 开 PR 指向 `main`。

## 5. 验证命令（实现期）

```bash
cargo xtask tool-codegen --check
cargo xtask tool-build
cargo test -p lancet-protocol evidence
cargo test -p lancet-runtime
cargo test -p lancet-catalog
cargo test -p lancet-native
cargo test -p lattice-api
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --test direct_mutation -- --test-threads=1
```
