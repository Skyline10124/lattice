---
status: active
---

# InvocationStore 诊断指南

## 目的与边界

InvocationStore qualification 当前为 diagnostic-only。所有者在 [#139 裁决](https://github.com/Skyline10124/lattice/issues/139#issuecomment-5380769508)批准 #139-A 的正确性、有界冷读、strict-reopen 与结构回归证据，解除 #139-B 阻塞；未改变持久顺序、失败语义、阈值、样本形状或 FinalizeBudget。旧 campaign 的 FAIL 仍为 FAIL。

[#153](https://github.com/Skyline10124/lattice/issues/153)维护测量模型与性能后续设计。[后续复核](https://github.com/Skyline10124/lattice/issues/153#issuecomment-5424871210)指出 schema/数字校验、baseline estimator 一致性、campaign 路径隔离、构建 provenance 封存、累计成本与持久屏障仍需处理。本指南不宣称这些缺口已经修复，也不恢复旧 qualification 为产品接线前提。未来 blocking gate 必须另行提案和批准，以独立同实现 campaign 的可重复性与已知退化 fixture 的辨别能力支持裁决。

产品的 [FinalizeBudget](../architecture/session.md#resultdraft-与关闭材料)、[ledger 与冷读边界](../architecture/lancet.md#invocationstore-与操作员接口)及[存储屏障](../architecture/storage.md#屏障覆盖)分别由对应合同维护。本文只说明诊断输入与工具，不改变产品协议。

## 输入与版本来源

[`qualification-contract-v2.json`](../../benchmarks/invocation-store/qualification-contract-v2.json)是阈值、run 数、sample shape、预算耦合检查与 baseline provenance 的唯一数值来源。不要手改下方生成表，也不要在运行命令另设一套样本数。

实际采集和读取形状分别见：

- [`run-qualification.sh`](../../benchmarks/invocation-store/run-qualification.sh)：environment 与 run-conditions、预构建和采样流程。
- [`invocation_qualification.rs`](../../crates/lancet-audit/tests/invocation_qualification.rs)：各组 JSONL 测量字段与计时边界。
- [`summarize_qualification.py`](../../benchmarks/invocation-store/summarize_qualification.py)：输入校验、estimators、summary 编码和命令退出行为。
- [`qualification_isolation.py`](../../benchmarks/invocation-store/qualification_isolation.py)：被测二进制定位与运行目录保留。

当前 contract version 为 2，harness shape 为 2，attribution estimator revision 为 2，environment schema 为 5，summary schema 为 4。这些版本分别表示 campaign 约定、被测流程、分类器、环境记录与汇总编码，不能相互替代。shape 2 在计时前建立一个全局 `sessions.git`、一个 `invocations.git` 和会话夹具，不沿用 shape 1 的逐步 `sessions-N.git` 成本基线。

baseline 绑定 source SHA、harness shape、contract 和 bundle identity，具体身份从 manifest 的 `baseline_provenance` 读取。shape 改变使相对成本基线失效；定义 baseline 的 campaign 不同时接受该 baseline 的合格判定。manifest provenance 及 environment 字段记录不等于已经证明采样期间 executable 字节被封存，相关缺口仍由 #153 核验。

## 命令

从仓库根运行。`BUNDLE` 应是明确选定的证据目录，不自动挑选最有利的 bundle。

```sh
python3 benchmarks/invocation-store/summarize_qualification.py report "$BUNDLE"
python3 benchmarks/invocation-store/summarize_qualification.py baseline "$BUNDLE"
python3 benchmarks/invocation-store/summarize_qualification.py render-gates
python3 benchmarks/invocation-store/summarize_qualification.py check-doc \
  docs/development/invocation-store-diagnostics.md
```

`report` 只读，不修改保留 bundle；对旧版本缺少的新证据给出 `insufficient_evidence`，不是回写旧 verdict。`baseline` 输出 family-T reference fragment，不作通过判定。输入损坏或不完整仍须失败；已知校验覆盖缺口不因工具退出成功而被视为已消除。

以下命令会写 `summary.json` 和 `summary.md`，只用于获准的新 campaign 或独立评估副本，不对历史原件运行：

```sh
python3 benchmarks/invocation-store/summarize_qualification.py benchmark "$BUNDLE"
python3 benchmarks/invocation-store/summarize_qualification.py qualify "$BUNDLE"
```

`benchmark` 记录诊断 verdict，但性能门失败不阻塞产品；`qualify` 保留严格 adjudication，失败或证据不足返回非零，不表示它重新取得批准权。

完整采样入口是 `benchmarks/invocation-store/run-qualification.sh`。它要求 Btrfs 工作区及临时存储位置、干净工作区，保留 source-history bundle，先构建 release 测试二进制再执行 manifest 指定的各组样本，最后调用 `benchmark`。这会构建、创建存储并消耗较多资源，必须另按任务授权运行；文档或路径消费者验证不运行它。

## 证据保留与解读

bundle 位于忽略的 `target/invocation-store-bundles/<stamp>-<sha>/`，不是提交的运行产物。环境、原始 JSONL、run conditions、日志、source-history 身份及原 summary 一起保留。缺文件、错版本、错误数量或非数值序列不能通过调阈值或补造数据修复。

不删样本、不丢 outlier、不因 host load、I/O PSI 或 stall 到来而判环境无效并择优重跑。不把 host stall 追溯改为实现 PASS。新分类器对旧证据的结果是外部评估，与原记录并存；原批准、撤回、数学模型、样本与 verdict 不能重写。

下表是现有 manifest 的原样生成输出，包括其英文 basis。这是机器可比较的现行诊断参数展示，不是本指南重新批准统计解释或产品时限。预算耦合仍按会话合同，旧样本不构成产品保证。

## 等待指标解读

local-ledger 当前使用 8 个写者。`record_write` 在调用写操作前读取全局完成数，在操作返回并记录计时样本后递增该计数；两次数值之差（饱和减法）是这个窗口内其他写操作的完成数。每个 run 的 `max_consecutive_wait` 取各操作差值的最大值，manifest 再取各 run 的最大值，与现有阈值 16 比较。

这个窗口包含入队前和操作返回后的调度时间。例如一个写者在读取起始计数后、取得 ticket 前暂停，其他写者可以完成多次合法写入。因此，[同进程 ticket FIFO](../architecture/storage.md#锁与并发语义)不能推出该端到端字段必然不超过 `N−1`。

所有者在本次文档收尾中取消该字段无条件 `<= N−1` 的旧要求；8 个写者时测得 7 保留为历史观测。现行票号 FIFO 保证和 manifest 的诊断阈值 16 不变，历史样本、summary 与 verdict 不变，不据本次裁决重判旧结果。生成参数和历史 basis 中的结构性解释应结合上述实际窗口阅读，不作为该字段的上界证明。若要验证精确的排队界限，须另行明确入队、获锁和计数边界，不在本次迁移中替换测量器或丢弃样本。

## 生成的诊断参数

<!-- markdownlint-disable MD032 MD060 -->
<!-- BEGIN GENERATED GATE TABLE -->
Contract v2, harness shape 2, attribution estimator revision 2, FinalizeBudget 60 s.

| Gate | Family | Group | Estimator | Limit |
|---|:---:|---|---|---|
| `D-local-invocations` | D | local-ledger | exact_every_run | = `1280` |
| `D-local-per-writer-completed` | D | local-ledger | exact_every_run | = `[340, 340, 340, 340, 340, 340, 340, 340]` |
| `D-local-indeterminate` | D | local-ledger | sum_over_runs | = `0` |
| `D-local-cas-retries` | D | local-ledger | sum_over_runs | = `0` |
| `D-finalize-invocations` | D | result-finalize | exact_every_run | = `160` |
| `D-finalize-draft-classes` | D | result-finalize | exact_every_run_tuple | = `[60, 60, 40]` |
| `D-finalize-repository-creates` | D | result-finalize | exact_every_run_tuple | = `[1, 1, 0]` |
| `D-finalize-indeterminate` | D | result-finalize | sum_over_runs | = `0` |
| `D-finalize-cas-retries` | D | result-finalize | sum_over_runs | = `0` |
| `D-cold-commits` | D | cold-reopen | exact_every_run | = `2561` |
| `D-cold-checkpoint-plans` | D | cold-reopen | exact_every_run | = `1224` |
| `D-cold-writer-handle-lifetime` | D | cold-reopen | exact_every_run | = `'one_open_per_write'` |
| `T-finalize-full-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 100.000 ms |
| `T-finalize-full-p90` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 120.000 ms |
| `T-finalize-terminal-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 50.000 ms |
| `T-finalize-journal-pin-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 50.000 ms |
| `T-finalize-dispatch-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 50.000 ms |
| `T-finalize-reserve-batch-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 50.000 ms |
| `T-finalize-draft-publish-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 25.000 ms |
| `T-finalize-revalidation-p50` | T | result-finalize | median_of_run_quantile | baseline +10%, absolute <= 25.000 ms |
| `T-cold-read-with-checkpoint-p50` | T | cold-reopen | median_of_run_quantile | baseline +15%, absolute <= 60.000 ms |
| `T-cold-checkpoint-speedup` | T | cold-reopen | ratio_of_median_of_run_quantile | >= 5.00x |
| `S-finalize-revalidation-drift` | S | result-finalize | within_run_drift | <= 1.15x |
| `S-finalize-full-drift` | S | result-finalize | within_run_drift | <= 1.15x |
| `S-finalize-journal-pin-drift` | S | result-finalize | within_run_drift | <= 1.15x |
| `S-finalize-dispatch-drift` | S | result-finalize | within_run_drift | <= 1.15x |
| `B-finalize-full-max` | B | result-finalize | max_over_samples | <= 10.000 s |
| `B-finalize-revalidation-max` | B | result-finalize | max_over_samples | <= 25.000 ms |
| `B-local-lock-wait-max` | B | local-ledger | max_of_run_field | <= 20.000 s |
| `F-local-writer-ratio` | F | local-ledger | max_of_run_field | <= 1.5 |
| `F-local-max-consecutive-wait` | F | local-ledger | max_of_run_field | <= 16 |
| `F-local-wall-median` | F | local-ledger | median_of_run_field | <= 120.000 s |
| `F-local-wall-max` | F | local-ledger | max_of_run_field | <= 170.000 s |
| `A-finalize-stall-attribution` | A | result-finalize | stall_attribution | <= 0 unattributed clusters |

Run counts:
- `local-ledger`: 10 runs — writer ratio cv 0.05 %, max-consecutive-wait cv 0 %, lock-wait upper values within 0.9 %: three of four gates are structurally deterministic and the fourth keeps a 1.7x margin
- `result-finalize`: 20 runs — family B needs stall exposure: P(observe >=1 host stall window) = 1 - 0.6^n, 0.92 at n=5 and ~1.00 at n=20, for ~5.4 min of campaign time
- `cold-reopen`: 8 runs — commits and checkpoint_plans are exact integers; each run carries 20 samples, so n=8 yields 160 samples per distribution
<!-- END GENERATED GATE TABLE -->
<!-- markdownlint-enable MD032 MD060 -->

## 指南与消费者验证

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s benchmarks/invocation-store -p 'test_*.py'
```

该组单元测试包括生成表一致性、summarizer 和目录隔离逻辑，不运行 qualification campaign，也不替代产品持久化验证。
