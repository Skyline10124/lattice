# Issue #139-A — qualification contract v2

Date: 2026-08-21
Issue: Skyline10124/lattice#139-A
ADR: `docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`
Supersedes the *gate semantics* of
`docs/plans/2026-08-18-issue139a-qualification-approval.md`. That record is not
rewritten: every measurement, withdrawal, and verdict it contains stays exactly
as recorded, and the bundles it cites stay byte-identical.

## STATUS: #139-A approved; performance qualification diagnostic-only; #139-B unblocked; redesign tracked by GitHub #153

Maintainer disposition on 2026-08-22: the implementation's correctness and
structural regression coverage are accepted for #139-A. The current campaign
contract is not mature enough to remain a production-wiring prerequisite, so
its verdicts are diagnostic until #153 defines a replacement. No threshold,
durable ordering rule, `FinalizeBudget`, or historical verdict is rewritten.
The official runner now writes ignored local bundles below
`target/invocation-store-bundles` and invokes `benchmark`; strict `qualify`
remains available to the follow-up.

`FinalizeBudget` stays at 60 seconds monotonic with its 25 ms → 1 s backoff and
its coupling rule. No durable ordering, failure semantics, or strict-reopen rule
changed. What changed is how a campaign is *adjudicated*.

## 1. Why v1 was replaced

Contract v1 gated on run incidence: at most K of 20 process runs may have a
per-run p99 above 100 ms (full finalize) or 50 ms (terminal publish). Four facts
from the retained evidence retire it.

**A per-run p99 over 160 samples is the second-largest sample.**
`ceil(160 × 0.99) − 1 = 158`, so the gate is exactly "count the runs holding
≥ 2 of 160 samples over the threshold". It is a max-like statistic, not a
percentile.

**The verdict tracked clustering, not slowness.** `da1d2ac1` (approved) had 14
super-threshold samples in 4 flagged runs; `3ee216ba` (approved) had 15 in 4,
with a 8.497 s maximum. `d474236` has **13** in 5, with a 4.163 s maximum and
the lowest median-of-run p99 ever recorded — and fails.

**A same-code control already existed.** `ca768c80` → `3ee216ba` differ only in
benchmark files (`git diff --stat` over `crates/` is empty). With the measured
product byte-identical:

| Statistic | `ca768c80` | `3ee216ba` | delta |
|---|---:|---:|---:|
| median-of-run p50 | 41.86 ms | 41.49 ms | +0.9 % |
| median-of-run p90 | 47.22 ms | 47.07 ms | +0.3 % |
| median-of-run p99 | 91.81 ms | 62.97 ms | +45.8 % |
| pooled p99 | 952.78 ms | 66.14 ms | +1340 % |
| v1 incidence | 10/20 FAIL | 4/20 PASS | verdict flip |

**The verdict depended on where a host stall landed.** In `d474236`, eight runs
absorbed an ~8 s stall. Run 10's stall arrived while the harness timer was
stopped (process 21.46 s, harness wall 11.98 s, untimed 9.47 s against
1.26–1.93 s everywhere else) and it scored clean.

The two costs are symmetric and both were realised: v1 failed a healthy bundle
at roughly even odds per campaign, and — because every quiet sample sits at
33 ms while the gate needs a *second* sample over 100 ms — it would pass a
uniform 2× slowdown of the whole finalize path.

## 2. The error model

| Generator | Signature | Family |
|---|---|---|
| deterministic structure | exact integers, identical in every run | **D** |
| deterministic algorithmic cost | central quantiles; per-run p50 varies 0.56 % | **T** |
| scaling with accumulated state | monotone drift *within* a run | **S** |
| shared kernel/device durability stalls | consecutive multi-second clusters with stages co-elevated and the barrier raised | **B** + **A** |
| harness artefacts | wall excess inside or outside the timer | evidence, recorded |
| arbitrary grouping (processes, arrays) | nothing physical | diagnostic only |

A `run` is a runner implementation detail. It is not a Turn, a session, a
durability transaction, or an SLO unit, and it carries no verdict.

## 3. Versioning and evidence compatibility

```text
qualification_contract_version = 2     # environment.json, written by the runner
harness_shape_version          = 2     # every JSONL record, written by the harness
attribution_estimator_revision = 2     # Family A classifier only; not a harness or contract bump
environment schema_version     = 5     # adds contract + harness shape
summary.json schema_version    = 4     # cluster records carry component shares
```

* Harness shape 1 is the retired per-step `sessions-N.git` benchmark. Shape 2
  first-creates one global `sessions.git` and one `invocations.git` and
  initializes every session fixture before the timer.
* **A latency baseline is bound to a harness shape.** A shape change invalidates
  every relative latency baseline; it never carries across.
* `qualify` fails closed (exit 2) on an absent or foreign contract version, a
  shape mismatch, records disagreeing on shape, a wrong run count, a missing or
  short or non-numeric series, an unexpected `cas_retry_mode`, or a missing
  required group.
* `report` opens any retained bundle without writing a byte. Gates whose
  evidence the bundle predates are `insufficient_evidence`. **v2 never converts a
  verdict a bundle already recorded under v1.**
* `baseline` prints the family-T reference fragment and grades nothing.

## 4. Gate manifest

`benchmarks/invocation-store/qualification-contract-v2.json` is the only place a
threshold, run count, or sample shape is written. The table below is generated
from it by `summarize_qualification.py render-gates`;
`summarize_qualification.py check-doc` fails when this block has drifted, and
`cargo test -p lancet-audit --test qualification_summary` runs that check.

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

### Family T — typical cost

Median across runs of each run's own p50 (and p90). Tolerance is +10 % against a
baseline pinned to `(source SHA, harness shape)`, rounded up from the measured
same-code reproducibility of 0.9 % (p50) and 0.3 % (p90) in §1's control. The
absolute ceiling exists so a baseline cannot ratchet upward in 10 % steps.

This estimator is the regression detector. Its historical trace tracks real
algorithmic changes and nothing else: 41.2 → 45.3 → 41.7 → 39.1 → **32.6** ms,
where the last step is the loose-object fanout kernel change.

### Family S — scaling invariance

Median of the last eighth of a run's samples over the median of the first
eighth, pooled across runs. ADR 0005 requires per-operation cost decoupled from
history depth; a monotone rise inside one run falsifies that claim directly, so
the limit is derived from the architecture rather than fitted to a measurement.
Pooling across runs averages host noise out while a monotone rise survives it.

### Family B — safety

Absolute maxima, coupled to the budget by
`lock_wait_gate + 2 × full_finalize_gate + backoff_cap ≤ FinalizeBudget`
(`20 + 2×10 + 1 = 41 ≤ 60`). A test asserts that inequality against the manifest.
Multi-second device stalls belong here and only here: they are evidence of how
bad the device can be, used to check the budget covers it. They are never a
quality score, and no sample is ever deleted.

### Family A — attribution

For every maximal cluster of consecutive `full_terminal_finalize_ns` samples
above 100 ms, partition that same sample:

```text
residual[i] = full[i] - session_journal_pin[i] - terminal_evidence_publish[i]
```

Lengths must match full. `full < pin + terminal` fails closed and is never
clamped to zero. Each component's positive excess is against that run's own
p50. A component contributes substantially iff it holds ≥ 1/4 of the summed
positive excess. The cluster is attributed iff at least two components are
substantial. **Any unattributed cluster fails at count ≥ 1.**

1/4 is the largest dyadic unit fraction at which two components can both be
substantial without either needing a majority. Majority is structurally
unusable: pin and terminal are a two-way split of the instrumented span, so a
shared stall with small residual always gives one side a slight majority.
`dispatch_started_ns` is timed before the finalize span and is unused.
`append_fsync_barrier_max_ns` is a run-wide maximum of a different append path
and is not time-aligned to the sample, so it is unused. A single substantial
component — including residual — is implementation-side.

Estimator revision 2 is a classifier correction, not a harness-shape or
contract-version bump: it consumes the already-aligned 160-sample pin,
terminal, and full series and does not change the timed path. Retained
`summary.json` files keep the verdict their original evaluator wrote. A live
`report` under revision 2 is an external evaluation.

A shared kernel/device stall inside the finalize span lifts pin and terminal
together. A regression inside one code path places a single substantial
component and fails. This is how an intermittent implementation regression is
detected without promoting process grouping to an acceptance unit.

### Diagnostics (recorded, never a verdict)

Run incidence at the v1 thresholds, stall cluster counts and lengths, pooled
quantiles, per-run wall, and the timed/untimed split from `run-conditions.jsonl`
so a stall outside the timer is visible rather than a free pass. K is **void**:
it was the maximum exceedance across shape-1 bundles whose harness drove ~863
Btrfs transaction commits per run against ~5 today, and it is not transferable.

## 5. Sample plan

Measured standard error of the median across n runs (`d474236`):

| Estimator | run-to-run sd | n=5 | n=8 | n=10 | n=20 |
|---|---:|---:|---:|---:|---:|
| per-run p50 | 0.56 % | 0.32 % | 0.25 % | 0.22 % | 0.16 % |
| per-run p90 | 1.63 % | 0.91 % | 0.72 % | 0.65 % | 0.46 % |
| per-run p95 | 2.77 % | 1.55 % | 1.23 % | 1.10 % | 0.78 % |
| per-run **p99** | **1728 %** | 968 % | 766 % | 685 % | **484 %** |
| writer ratio | 0.05 % | — | — | — | — |
| max consecutive wait | 0 % (exactly 7 in 20/20) | — | — | — | — |
| cold `commits` / `checkpoint_plans` | 0 % (exact) | — | — | — | — |

A p99 of 160 samples cannot be estimated at any run count — its run-to-run
standard deviation is 17× its own value. That retires it on statistical grounds
alone, independently of §1.

Counts are 20 / 10 / 8. `result-finalize` keeps 20 because family B needs stall
exposure, not because family T needs it. `local-ledger` and `cold-reopen` were
oversampled at 20: three of four local gates are structurally deterministic and
both cold headline values are exact integers. The reduction takes a campaign
from roughly 52 to 26 minutes and requires the ADR 0005 §20 amendment landing in
the same change.

## 6. Disposition of every v1 gate

| v1 item | v2 disposition |
|---|---|
| full-finalize run incidence, K=4/20 | **retired → diagnostic** |
| terminal-publish run incidence, K=3/20 | **retired → diagnostic** |
| K derived from `4a51b5d4, da1d2ac1, 3ee216ba, e5c23481, 403836c2` | **void** — shape-1 population, not transferable |
| median-of-per-run-p99 (100 / 50 ms) | **demoted to diagnostic**, superseded by T (p50/p90) and B (max) |
| pooled p99 | **retired** |
| full-finalize max ≤ 10 s | **retained unchanged** (`B-finalize-full-max`) |
| terminal-publish p99 ≤ 50 ms | **replaced** by `T-finalize-terminal-p50` |
| uncertain-revalidation p99 ≤ 1 ms | **void for shape 2** — it measured a strict open of a per-step `sessions-N.git`. Replaced by `T-finalize-revalidation-p50` and `S-finalize-revalidation-drift` |
| uncertain-revalidation max ≤ 10 ms | **retained**, tightened to 25 ms (`B-finalize-revalidation-max`) |
| local wall p95 ≤ 170 s | **replaced** by median ≤ 120 s and max ≤ 170 s, both well defined at n=10 |
| max single lock wait ≤ 20 s | **retained unchanged** |
| writer ratio ≤ 1.5, max consecutive wait ≤ 16 | **retained unchanged** |
| CAS retries = 0, unexpected `Indeterminate` = 0 | **retained**, moved to family D |
| `FinalizeBudget` 60 s, backoff, coupling rule | **unchanged** |
| "invalid environment reruns the group" | **narrowed**: only a provable evidence defect may invalidate. Host load, I/O PSI, and stall arrivals never may. No outcome-dependent rerun. |
| sample deletion / outlier dropping | **prohibited, unchanged** — v2 needs none |

The v1 record's `uncertain_draft_revalidation` max gate was never machine
enforced. Bundle `d9744add` measured **20.899 ms** against its stated 10 ms
limit and was never adjudicated; that is the concrete cost of a summarizer that
enforced two of a dozen written gates.

## 7. Prospective and retrospective adjudication

* **Absolute families (D, S, B, F, A)** may be adjudicated on any bundle whose
  evidence carries the required fields. They depend on no baseline, so
  retrospective evaluation is not circular.
* **Family T** requires a baseline established by a dedicated baseline campaign
  at a named `(source SHA, harness shape)`. The manifest stores that binding in
  `baseline_provenance` beside the baseline values. `qualify` fails closed if
  provenance is missing or the recorded harness shape does not match the
  required shape; a shape change with leftover `baseline_ns` values is
  `UNADJUDICABLE`, never silently reused. A baseline campaign produces no
  family-T verdict — it *defines* the reference. A campaign may never both
  establish and be graded against the same baseline.
* Family T is bound to source `742cfd205e0c21608d27d1fcde073d1a7e4c0b3e`, harness
  shape 2, contract 2, bundle `20260821T192437Z-742cfd205e0c`. `qualify` now
  grades family T. The defining campaign is not itself a passing qualification.

## 8. `d474236` under contract v2 — external evaluation

The bundle `target/invocation-store-bundles/20260821T115648Z-d474236ca1e6/`
remains locally, in its own recorded bytes, a **valid contract-v1 5/20 and 5/20
incidence FAIL**. It was not rewritten, relabelled, or rerun; the current Git
tree no longer tracks benchmark bundles.
The evaluation below is external to the bundle and is produced by
`summarize_qualification.py report`.

| Family | Result |
|---|---|
| D | all pass |
| T | `insufficient_evidence` — the bundle predates contract/shape recording |
| **S** | **`S-finalize-revalidation-drift` 2.26× against 1.15× — FAIL** |
| B | all pass; chain `8.278 + 2×4.163 + 1.0 = 17.61 s` against 60 s, **3.41× headroom** |
| F | all pass |
| A | 0 unattributed of 7 clusters — pass under estimator revision 1. Live `report` under revision 2 reclassifies run 20 (pin share 0.76) as unattributed; that is an external evaluation, not a rewrite of these bytes. |

Under v2 estimator r1 the bundle still fails, for a different and actionable reason: every
one of its 13 super-threshold samples is attributed to a shared device
durability stall, its budget chain fits with 3.41× headroom, and what fails is a
monotone rise in strict `GitSessionGraphStore::open` cost. Estimator r2 does not
change that S-gate FAIL.

## 9. Findings this contract surfaced

**L-GH139A-11** — `uncertain_draft_revalidation_ns` measures strict
`GitSessionGraphStore::open` plus `classify_draft`. Median across 20 runs by
occurrence index: 1.01 → 1.49 → 2.00 → 2.57 ms, slope 0.027 ms/op, perfectly
monotone with no sawtooth at step boundaries, against 0.45 → 0.48 ms and slope
0.0004 under shape 1. The only `crates/` delta outside the harness between the
two bundles is a first-create tracer in `lattice-store`. Strict open therefore
costs O(accumulated global `sessions.git` content), and it runs inside
`FinalizeBudget`. The shape-1 harness hid it by giving every step a fresh
repository.

**L-GH139A-12** — `session_journal_pin_ns` rises 12.9 → 15.7 ms across the eight
bindings of a step and resets at every step boundary, in both harness shapes:
the `StepClosing` pin costs more as the Turn's closure list grows. Pre-existing,
bounded at eight invocations per Turn, and never gated. Diagnostic in v2 pending
diagnosis.

## 10. Not done here

A shape-2 baseline is installed from `20260821T192437Z-742cfd205e0c` (source
`742cfd20`). Independent campaign `20260821T221243Z-f9377b0dbf80` (source
`f9377b0d`) is a **valid contract-v2 FAIL**: `T-finalize-full-p90` 193.770 ms
against 120 ms absolute, `B-finalize-full-max` 37.422 s against 10 s,
`B-finalize-revalidation-max` 165.903 ms against 25 ms, `A-finalize-stall-attribution`
4 unattributed of 36 (all pin-only substantial). D/S/F and the T p50s passed,
including `S-finalize-revalidation-drift` 1.06×. The budget chain does not fit
(84.09 s against 60 s). That recorded verdict remains valid historical
evidence, but it no longer adjudicates #139-A or blocks #139-B. Follow-up #153
owns the measurement model, durability-cost attribution, and any future
blocking contract. No wash-green rerun and no threshold rewrite occurred.
