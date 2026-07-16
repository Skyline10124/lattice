# Direct-Local 迁移总览：#37 → #38 → #39 → #40 + gix 收口

本文件是执行顺序、全局约束、验证矩阵与风险的唯一维护点；各票的实现细节在对应的
per-issue 计划中，不在此复制。

## 计划文件

| 顺序 | Issue | 计划 |
|---|---|---|
| 1 | #37 | [issue37-direct-local-turn](2026-07-17-issue37-direct-local-turn.md) |
| 2 | #38 | [issue38-remove-execution-backend](2026-07-17-issue38-remove-execution-backend.md) |
| 3 | #39 | [issue39-policy-visible-read](2026-07-17-issue39-policy-visible-read.md) |
| 4 | #40 | [issue40-remove-upper-msb-residue](2026-07-17-issue40-remove-upper-msb-residue.md) |
| 5 | 待开票 | [gix-only-cleanup](2026-07-17-gix-only-cleanup.md) |

## 规范源

- `docs/adr/0003-lancet-operates-directly-on-local-workspace.md`（Migration
  Sequence 1–6 与 #37–#40 一一对应；step 7 的 gix rewind 归 #41/#42，不在本批）。
- 根 `CONTEXT.md` 词汇表（Local Workspace / Local Revision / Mutation
  Precondition / Atomic Local Mutation / Policy-Visible Workspace / System Path
  Exclusion / Replan Required）。
- 四张票的验收清单。冲突时以票文验收为准。
- 上级：#16 Workstream A（`M-LOCAL-01`），执行顺序第 1 条。

## 用户拍板（2026-07-17）

1. 四票连续实现。中间态的 `[workspace]` 必填怪相可接受，不提前动 schema。
2. **gix-only**：仓库内全部 git 操作（production、测试、harness）迁移到
   `gix`（0.85.x），`git2` 与 `Command::new("git")` 清零。该约束超出 #40 票面，
   编排为第 5 份计划，建议补开一张票挂 #16。
3. no-compat alpha：切换即删旧路径，不留 fallback / marker 门控。

## 全局纪律

- **禁止 `cargo test --workspace`**。逐 crate、必要时逐测试文件，`timeout` +
  单线程（`--test-threads=1`）跑。
- 每份计划完成标准：全 workspace 编译通过、该票验收逐条勾掉、Agent-visible
  catalog 恰好五方法（`observe.workspace`、`mutate.create`、`mutate.edit`、
  `fetch.web`、`session.todo_write`）。
- gix 具体 API 以**锁定后的 crate 源码**为准，动手前先 spike（Phase 33 幻觉
  API 教训）。

## 验证矩阵（每份计划收尾执行；串行、限时）

```bash
cargo build --workspace --all-targets                       # 编译门
for c in lancet-protocol lancet-catalog lancet-dispatch lancet-cep \
         lancet-audit lancet-authority lancet-native lancet-api \
         lattice-profile lattice-session lattice-agent lattice-llm \
         lattice-api lattice-tui lattice-cli net-guard; do
  timeout 600 cargo test -p "$c" -- --test-threads=1 || break
done
# #40 之后（无 KVM 环境）产品级 smoke：
timeout 600 cargo test -p lattice-system-harness -- --test-threads=1
```

## 风险与对策

1. **gix 0.85 API 漂移**（写路径/tree editor/excludes 演进快）→ #39 与 gix 收口
   各自先 spike 锁 API，再写主代码；一切以 vendored 源码为准。
2. **grep/find 每文件 visibility 判定的性能** → 先正确后快：budget 上限
   （1000 match/entry）天然封顶；exclude 栈按目录前缀增量下降（gix Stack 即此
   设计），非逐文件全量重算。
3. **audit 断言面大改**（#38 字段置换牵动 dogfood/provenance 断言）→ 集中一个
   commit 内完成字段+断言，避免半改状态。
4. **`lattice-session` 迁移是行为敏感区**（session graph 是持久数据）→ gix 收口
   保留 git_graph 全部现有测试作为迁移回归门，先测后迁；gix 写出的对象必须能被
   旧断言读回（对象格式同为 git，天然兼容）。
5. **ADR step 7（gix rewind history）不在本计划**——#41/#42。#37 不要顺手记
   mutation history。
