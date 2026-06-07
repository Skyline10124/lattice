# Legacy Maintenance Benchmark

This benchmark is historical: its phase prompts still use pre-v1.3.1 CEP tool
names. Keep it for comparison unless the whole benchmark is revalidated against
the current 10-method LANCET catalog.

This benchmark targets the product shift from code-specialized assistance to a
general session-centric maintenance runtime.

The benchmark runs one pinned, real upstream project through several phases in a
single LATTICE session:

1. Review the stale project and issue context.
2. Reproduce or narrow the reported problem.
3. Apply a minimal repair under CEP supervision.
4. Add or update regression coverage.
5. Produce a maintainer handoff.

Each phase may switch the active agent profile while preserving one gitlike
session. The report records turn commits, LLM request errors, tool calls, CEP
events, token usage, and final workspace diff.

## Target Project Rules

Use a real GitHub project only after freezing it into a reproducible target
record:

- The repository must have a permissive license.
- The revision must be pinned by commit SHA.
- The issue or maintenance task must be copied into the target record.
- The project must build or test locally with common tooling.
- The task should require code understanding, not just formatting.
- The repository should be stale or archived enough to represent maintenance
  work, but small enough for repeated live-model runs.

Do not benchmark against a moving branch or a live issue URL alone. GitHub may
change; the benchmark must remain reproducible.

## Quick Start

Create a target env file from `benchmark.env.example`, then run:

```sh
set -a
. benchmarks/legacy-maintenance/benchmark.env
set +a
benchmarks/legacy-maintenance/run.sh
```

Required provider environment:

```sh
OPENAI_COMPATIBLE_BASE_URL=https://api.deepseek.com
OPENAI_COMPATIBLE_API_KEY=...
```

Useful options:

- `BENCH_MODEL` — model alias, default `compat:deepseek-v4-pro`.
- `BENCH_PHASE` — run one phase slug.
- `BENCH_KEEP=1` — keep cloned workspace and session database.
- `BENCH_VERIFY_COMMAND` — external verification command run after all phases.
- `BENCH_STRICT_PHASES=1` — fail the run when any individual phase fails.

By default the script exits non-zero only when the final external verification
fails. Individual phase failures remain recorded as benchmark metrics.

Reports are written to:

```text
build/tmp/benchmarks/legacy-maintenance/report.md
```

## Included Target

`targets/rslint-rust-189/` freezes `https://github.com/rslint/rslint` at commit
`7bc011e57c76ee114c440521a51907089584b10a` and uses the Rust 1.89 compatibility
task from `rslint/rslint#165`.

To run that target, copy its example env:

```sh
cp benchmarks/legacy-maintenance/targets/rslint-rust-189/benchmark.env.example \
  benchmarks/legacy-maintenance/benchmark.env
```
