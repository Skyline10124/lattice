# LANCET Dogfood Benchmark

This suite turns the old live smoke and dogfood scripts into a repeatable
real-provider benchmark for the current LANCET catalog.

It is intentionally outside the release gate. The benchmark uses the production
CLI turn path with a real LLM provider, so results depend on model behavior,
provider availability, and token cost.

## Scenarios

| Scenario | Purpose | Expected current tools |
|---|---|---|
| `smoke` | Minimal envelope, observation, create, and command path. | `session.envelope`, `observe.workspace`, `mutate.create`, `exec.command_run` |
| `mutation` | Hashline patch with post-mutation verify. | `session.envelope`, `observe.workspace`, `mutate.patch` |
| `command` | Declared command execution without workspace mutation. | `session.envelope`, `exec.command_run` |

The runner writes one JSONL stream and one `summary.json` per scenario, then
aggregates them into a Markdown report.

## Run

```sh
ANTHROPIC_API_KEY=... benchmarks/lancet-dogfood/run.sh
```

Run one scenario:

```sh
ANTHROPIC_API_KEY=... benchmarks/lancet-dogfood/run.sh --scenario smoke
```

Use an OpenAI-compatible provider:

```sh
OPENAI_COMPATIBLE_API_KEY=... \
OPENAI_COMPATIBLE_BASE_URL=https://example.invalid/v1 \
LANCET_DOGFOOD_MODEL=compat:deepseek-v4-pro \
benchmarks/lancet-dogfood/run.sh
```

## Artifacts

Default output lives under:

- `build/tmp/benchmarks/lancet-dogfood/report.md`
- `build/tmp/benchmarks/lancet-dogfood/<run-id>/<scenario>/events.jsonl`
- `build/tmp/benchmarks/lancet-dogfood/<run-id>/<scenario>/summary.json`

Set `LANCET_DOGFOOD_KEEP=1` or pass `--keep` to retain per-run workspaces.

## Legacy Scripts

The old Phase 22/27 scripts live in `legacy/` for historical reproduction.
They are not the current LANCET catalog contract. Prefer `run.sh` for current
benchmark comparisons.
