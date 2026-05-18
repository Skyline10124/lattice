---
phase: 06-crate-topology-and-main-path-skeleton
plan: 03
subsystem: lattice-cli
tags: [cli-command, run-v11, skeleton-path, boundary-enforcement, v1.1, additive-refactor]
requires: [lattice-api-facade, lattice-llm-stub, run-turn-api]
provides: [run-v11-command, end-to-end-skeleton-path]
affects: [lattice-cli, cargo-workspace]
tech-stack:
  added: []
  patterns: [clap-derive-command, thin-handler, tdd-red-green, boundary-enforcement, no-credential-execution]
key-files:
  created:
    - crates/lattice-cli/src/commands/run_v11.rs
  modified:
    - crates/lattice-cli/Cargo.toml
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/commands/mod.rs
    - Cargo.lock
decisions:
  - "Registered run-v11 as separate clap command rather than modifying legacy run (D-15, D-23)"
  - "Handler directly constructs RunTurnRequest and calls lattice_api::run_turn (D-24, D-25)"
  - "Factored render_result as pure helper for testability without stdout capture"
  - "Used TDD RED/GREEN cycle for handler behavioral tests"
metrics:
  duration_seconds: 648
  completed_date: "2026-05-16T08:42:46Z"
  task_count: 3
  file_count: 5
---

# Phase 06 Plan 03: Add Isolated CLI run-v11 Command Through lattice-api

Observable end-to-end skeleton path: CLI `run-v11` command builds a `RunTurnRequest`, calls `lattice_api::run_turn`, receives a `RunTurnResult` with stub assistant output from `lattice-llm::StubLlm`, and prints the response to stdout — without provider credentials, legacy runtime construction, or old `commands::run` imports.

## Completed Tasks

| Task | Name | Type | Commit | Key Artifacts |
|------|------|------|--------|---------------|
| 1 | Register the isolated run-v11 command | auto | `24ccd00` | crates/lattice-cli/Cargo.toml, crates/lattice-cli/src/main.rs, crates/lattice-cli/src/commands/mod.rs |
| 2 | Implement the run-v11 handler through lattice-api | auto (tdd) | `c79930b` (RED), `8fc3d80` (GREEN) | crates/lattice-cli/src/commands/run_v11.rs |
| 3 | Run the Phase 6 observable smoke and boundary gate | auto | (verification only) | — |

## Verification Results

| Check | Result |
|-------|--------|
| `cargo check --workspace` | PASS (no errors, no warnings) |
| `cargo test -p lattice-llm` | PASS (18/18) |
| `cargo test -p lattice-api` | PASS (15/15) |
| `cargo test -p lattice-cli run_v11` | PASS (9/9) |
| `cargo test -p lattice-cli run_v11_parses_prompt` | PASS (1/1) |
| `cargo run -p lattice-cli -- run-v11 "hello"` | PASS — prints `stub response: hello` |
| Dependency boundary check (cargo metadata + jq) | PASS — only `lattice-api -> lattice-llm` (allowed) |
| Forbidden re-export/type check (rg) | PASS (0 matches in lattice-llm, lattice-api, run_v11.rs) |
| `run_v11.rs` forbidden import check | PASS — no `lattice_runtime`, `lattice_core`, `model_runtime`, or `commands::run` |
| Legacy `run` command still compiling | PASS — `Commands::Run` dispatch unchanged |

## TDD Gate Compliance

Task 2 followed the RED/GREEN cycle:

1. **RED** (`c79930b`): Created `run_v11.rs` with `todo!()` stubs and 4 behavioral tests. 2 failed (handler stubs), 2 passed (tested `lattice_api::run_turn` contract directly).
2. **GREEN** (`8fc3d80`): Implemented `run()` constructing `RunTurnRequest` and calling `lattice_api::run_turn`, and `render_result()` extracting `assistant_output`. All 9 tests pass (4 behavioral + 5 parser).

## Deviations from Plan

None — plan executed exactly as written with no bugs, missing functionality, or blocking issues encountered.

## Known Stubs

| Stub | File | Line | Reason |
|------|------|------|--------|
| `run()` calls `lattice_api::run_turn` which calls `StubLlm` | crates/lattice-cli/src/commands/run_v11.rs | 21 | Intentional Phase 6 stub. Real provider HTTP in Phase 7. |
| `RunTurnResult.commit_id` is always `None` | crates/lattice-cli/src/commands/run_v11.rs | — | Intentional per D-16. Real commits in Phase 8. |

## Threat Surface Scan

No new security-relevant surface beyond what is documented in the plan's threat model. The `run_v11` handler:
- Does not read provider credentials or environment variables
- Does not make HTTP calls (dispatches through `StubLlm`)
- Does not access the filesystem
- Does not import or re-export legacy runtime types
- Prints only the stub assistant output to stdout

## Self-Check: PASSED
