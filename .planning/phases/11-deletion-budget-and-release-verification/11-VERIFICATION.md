---
phase: 11-deletion-budget-and-release-verification
status: passed
created: 2026-05-17
release_gate: passed
---

# Phase 11 Release Verification Evidence

This file records the complete Phase 11 release gate evidence.

## Phase 10 Handoff

Phase 10 D-25 is closed by
`.planning/phases/10-cli-cutover/10-06-SUMMARY.md`.

Do not carry the older partial D-25 verifier finding forward as an open Phase 11
gap. Phase 11 starts from the `SessionApi`-scoped session command behavior
recorded in that summary.

## Full Release Gate

| Gate | Command | Status | Evidence |
|---|---|---|---|
| Formatting | `cargo fmt --all --check` | passed | 0 diffs, exit 0 (2026-05-17) |
| Clippy | `cargo clippy --workspace -- -D warnings` | passed | 0 errors, exit 0 (2026-05-17) |
| Workspace check | `cargo check --workspace` | passed | 0 errors, exit 0 (2026-05-17) |
| Workspace tests | `cargo test --workspace` | passed | 504 tests passed, exit 0 (2026-05-17) |
| Focused LLM routing tests | `cargo test -p lattice-llm` | passed | 128 tests passed (2026-05-17) |
| Focused profile parsing tests | `cargo test -p lattice-profile` | passed | 55 tests passed (2026-05-17) |
| Focused session graph tests | `cargo test -p lattice-session` | passed | 46 tests passed (2026-05-17) |
| Focused agent/tool loop tests | `cargo test -p lattice-agent && cargo test -p lattice-tools` | passed | 12 + 43 tests passed (2026-05-17) |
| Focused API facade tests | `cargo test -p lattice-api` | passed | 33 tests passed (2026-05-17) |
| Focused CLI command tests | `cargo test -p lattice-cli --test cli_cutover` | passed | 9 tests passed (2026-05-17) |
| Isolated CLI smoke | `cargo test -p lattice-cli --test release_smoke` | passed | 11 tests passed (2026-05-17) |
| Boundary check | `./scripts/release-boundary-check.sh` | passed | 7/7 groups PASS, exit 0 (2026-05-17) |
| Documentation drift | `./scripts/release-boundary-check.sh --docs` | passed | PASS, exit 0 (2026-05-17) |
| Strict LOC budget | `./scripts/source-loc-budget.sh --max 20000` | passed | TOTAL 19961 lines within max 20000 (2026-05-17) |
| Final source audit | `cargo metadata --no-deps --format-version 1` | passed | 8 workspace members: lattice-agent, lattice-api, lattice-cli, lattice-hooks, lattice-llm, lattice-profile, lattice-session, lattice-tools (2026-05-17) |

## Strict LOC

Required command:

```sh
./scripts/source-loc-budget.sh --max 20000
```

Counting rule: every Rust line under `crates/*/src/**/*.rs`, including inline
tests, doc comments, helpers, and all active crate source.

Current status: **passed**

```
Strict Rust LOC budget: crates/*/src/**/*.rs
MAX 20000
CRATE                                     FILES    LINES
crates/lattice-agent                          2     1301
crates/lattice-api                            3     2030
crates/lattice-cli                           10     2513
crates/lattice-hooks                          1      404
crates/lattice-llm                           12     3899
crates/lattice-profile                        6     1802
crates/lattice-session                        3     3513
crates/lattice-tools                         11     4499
TOTAL                                         8    19961
PASS: TOTAL 19961 is within max 20000
```

## Boundary Check

Required command:

```sh
./scripts/release-boundary-check.sh
```

Current status: **passed**

```
PASS [workspace metadata old crate identities]
PASS [active crate source old crate identities]
PASS [removed old crate directories]
PASS [unsupported CLI/TUI files]
PASS [API migration-home files]
PASS [lockfile old workspace packages]
PASS [release docs drift]

Boundary check passed
```

## Focused Tests

Required focused areas:

| Area | Command | Status |
|---|---|---|
| LLM routing | `cargo test -p lattice-llm` | passed (128 tests) |
| Profile parsing/loading | `cargo test -p lattice-profile` | passed (55 tests) |
| Session graph operations | `cargo test -p lattice-session` | passed (46 tests) |
| Agent/tool loop | `cargo test -p lattice-agent && cargo test -p lattice-tools` | passed (12 + 43 tests) |
| API facade | `cargo test -p lattice-api` | passed (33 tests) |
| CLI commands | `cargo test -p lattice-cli --test cli_cutover` | passed (9 tests) |

## Isolated CLI Smoke

Required smoke coverage:

| Command Shape | Status | Evidence |
|---|---|---|
| `lattice run <prompt> --model <alias>` | passed | release_smoke: run without profile fails with clear error mentioning agent.toml/profile |
| `lattice models --auth` | passed | release_smoke: models --auth lists aliases or reports missing credentials |
| `lattice resolve <alias>` | passed | release_smoke: resolve shows provider details without credential values |
| `lattice session status <session_id>` | passed | release_smoke: session status shows specific session from seeded store |
| `lattice session log <session_id>` | passed | release_smoke: session log shows turn messages from seeded store |
| `lattice doctor` | passed | release_smoke: doctor reports config, credentials, session store status |

Smoke evidence does not print credential values. Missing credentials produce
output naming the missing env vars clearly.

## Documentation Drift

Required command:

```sh
./scripts/release-boundary-check.sh --docs
```

Current status: **passed**

Targets: `README.md`, `AGENTS.md`, `CLAUDE.md`, and `.planning/codebase`.
Main docs describe the final v1.1 crates and supported CLI surface. Removed
capabilities appear only as deferred/future work, not as supported behavior.

## Workspace Members

```sh
cargo metadata --no-deps --format-version 1
```

Result: 8 workspace members matching the v1.1 target crate set:
- lattice-agent
- lattice-api
- lattice-cli
- lattice-hooks
- lattice-llm
- lattice-profile
- lattice-session
- lattice-tools

## Notes

- All Phase 11 release gates pass. The phase is complete.
- LOC total 19,961 is within the 20,000 budget.
- Boundary checks confirm no removed crate identities, unsupported files, or
  migration tables in active source.
- Docs drift check confirms main documentation describes only the final v1.1
  reality with deferred capabilities clearly marked.
