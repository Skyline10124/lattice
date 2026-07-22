# #43：重写含退役 KVM-lane 词汇的历史文档（ADR/plan/spec）

- 票：<https://codeberg.org/AstRin/LATTICE/issues/43>（`ready-for-agent`）
- 覆盖范围：本票列出的六份文档覆盖 #40「historical ADR/FIX records 不重写」旧约束；只限本票文档范围，不改 #40 产品行为。
- 词汇源：根 `CONTEXT.md` + ADR 0003（Direct-Local）+ ADR 0004（Tool Host / Source Tool）。
- 迁移历史归属：`FIX.md`（append-only）；历史文档只保留理解决策所需的因果，不保留退役字段值作现行概念。

## 0. 已确认取舍

1. **#40 完成计划**：移入 `docs/archive/plans/`，加 Direct-Local 归档头；不留在 `docs/plans/`。
2. **#23 父计划 + 切片 3/4**：就地标 completed + Direct-Local 重写；保留大写 MSB 仅作「词汇退役」meta 叙述。
3. **切片 3 文件名**：`…-slice3-msb-vocabulary-retirement.md` → `…-slice3-retired-lane-vocabulary.md`，并更新父计划链接。
4. **ADR 0003**：最高优先级现行 ADR；Context / Migration / Rejected / Consequences 用 Direct-Local 重写，保留「为何放弃投影/沙箱」因果，不用 Microsandbox/MSB 作现行概念。
5. **#5 effect-model spec**：逻辑模型（Cell / 通道 / ToolHost）保留；Upper/MSB/guest 物理假设对齐 ADR 0003/0004：
   - `in` = Tool Host 域可控；`ex` = 外溢到外部世界（非 Upper containment）。
   - `network` = 宿主侧 Tool Host 通道 + `net-guard` / `[lancet.network]`（非 Upper egress / SDK 网关）。
   - 原「模式 B = Upper 内宿养领域后端」与 ADR 0004「禁止 hosted backend processes」冲突 → 标为 **superseded；若再引入需新 ADR**。
   - 承载面：ADR 0004 已取代「v1 = 进程内 trait」分代；终局 WASM Component 即现行目标。
6. **允许保留的 meta**：描述退役本身的大写 `MSB`（如「MSB 词汇退役」「no KVM, no MSB daemon」）；禁止 `microsandbox`、`msb_full_turn`、`planned_*_msb`、`LATTICE_HARNESS_MSB`、`real_microsandbox` 作为现行字段/场景值。
7. **不做**：不重写已 superseded/archive-only 的 ADR 0001/0002、`docs/archive/**`；不改产品代码/测试；不恢复 Upper/KVM lane。

## 1. 执行顺序

1. ADR 0003
2. 归档 #40 计划 + 更新 `docs/archive/README.md`
3. #23 父计划 → 切片 3（含改名）→ 切片 4
4. #5 effect-model / ToolHost spec
5. 验收 `rg` 门 + 移除全部 `⚠️ 过时词汇警告`
6. `FIX.md` 追加本票条目；`/review`；commit

## 2. 重写模式（贴近现有文档结构）

### 2.1 ADR 0003 Context 示例

```markdown
## Context

ADR 0002 made the Local Workspace authoritative but still copied it into a
turn-scoped isolated workspace mirror, snapshotted each Tool call, watched
Local changes, merged two filesystem histories, and wrote approved results
back. Evaluation of that projection-and-isolation stack showed that faithfully
projecting project trees, preserving metadata, establishing consistent
savepoints, and operating the required VM infrastructure creates more machinery
than the Agent's closed structured catalog requires.
```

Migration step 3/5/6 同类替换：

```markdown
3. … Prove plain-text turns and every catalog Tool run without KVM or an MSB
   daemon.
5. Delete guest-Git baseline delivery, … capable-host probes, KVM guards,
   and the `lattice-workspace` crate …
6. Replace retired-lane harness scenarios with Direct-Local scenarios …
```

（此处大写 MSB 仅作 meta「无 MSB daemon」；不引入 `real_microsandbox` 等字段值。）

### 2.2 #23 切片 3 标题/开场示例

```markdown
# #23 切片 3：退役 KVM-lane 词汇与 assertion-level #24 映射

- 状态：已完成（2026-07-19；ledger/prose 已落地，见 FIX.md #23 slice 3/4）
- 父计划：`2026-07-19-issue23-fast-residual-ledger-closeout.md` §3
```

脚本示例把现行字段值换成历史叙述 + Direct-Local 目标：

```python
# Historical pre-closeout filter (no longer present after slice 3).
rows = [e for e in entries if e.get("target_layer") == "real_microsandbox"]
# After rewrite, acceptance asserts the filter is empty:
assert not any(e.get("target_layer") == "real_microsandbox" for e in entries)
```

计划正文描述「当时有 57 条 stale layer」时，用过去时 + 「现已清零」；示例代码若演示**已完成**的迁移脚本，可保留被删除字段名，但必须明确标注为 historical closeout script，不得表现为待执行计划。

### 2.3 #5 §6.6 网络通道示例（对齐 ADR 0004）

```markdown
### 6.6 网络通道（宿主 Tool Host + net-guard）

**决议：`fetch.web` 字节从宿主 Tool Host 的 `network` 通道发出。**
强制面 = `net-guard` + `[lancet.network].allowed_hosts`（URL/DNS/redirect/SSRF），
不是已删除的 turn-scoped workspace egress 网关。

- effect 记账取通道调用参数（url/host），与 workspace 轴取 path 对称。
- allowlist 唯一事实源：`[lancet.network].allowed_hosts`（`[workspace].allowed_egress_*` 已随 #40 删除）。
```

### 2.4 #40 归档头示例

```markdown
# [Archived] #40 实现计划：删除 Upper 配置与 lattice-workspace 残留

> Archived 2026-07-22 under Codeberg #43. Completed work; historical
> migration detail lives in FIX.md. Do not treat retired lane names in the
> body as current requirements. Canonical workspace model: ADR 0003
> (Direct-Local) + ADR 0004 (Tool Host).
```

## 3. 验收

```bash
rg -i 'microsandbox|msb_full_turn|planned_[a-z_]+_msb|LATTICE_HARNESS_MSB|real_microsandbox' \
  docs/adr/0003-lancet-operates-directly-on-local-workspace.md \
  docs/plans/2026-07-19-issue23-fast-residual-ledger-closeout.md \
  docs/plans/2026-07-19-issue23-slice3-retired-lane-vocabulary.md \
  docs/plans/2026-07-19-issue23-slice4-ledger-prose-consistency.md \
  docs/specs/2026-07-01-issue5-effect-model-toolctx-design.md
# 期望：零命中（#40 已归档；ADR 0001/0002 按 issue 明确不处理）

rg '过时词汇警告' \
  docs/adr/0003-lancet-operates-directly-on-local-workspace.md \
  docs/plans/2026-07-19-issue23-*.md \
  docs/specs/2026-07-01-issue5-effect-model-toolctx-design.md
# 期望：零命中；#40 原路径已不存在。
```

人工检查：大写 MSB 仅出现在 meta「词汇退役 / no MSB daemon」语境。

## 4. 门禁

文档-only 变更：不要求 `cargo xtask ci`。落地后 `/review`（Standards + Spec）再 commit。
