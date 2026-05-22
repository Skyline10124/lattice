---
phase: 16-live-api-stream-and-commit-transaction
plan: 01
subsystem: lattice-api/turn
tags: [dependency, refactor, shared-helpers]
dependency_graph:
  requires: []
  provides: [tokio-stream, build_commit_input, TurnCommitContext, derive_terminal_event]
  affects: [16-02, 16-03]
tech_stack:
  added: [tokio-stream]
  patterns: [shared-helper-extraction, pub-super-visibility]
key_files:
  created:
    - crates/lattice-api/src/turn/live.rs
  modified:
    - Cargo.toml
    - crates/lattice-api/Cargo.toml
    - crates/lattice-api/src/turn/execute.rs
    - crates/lattice-api/src/turn/mod.rs
decisions:
  - "tokio-stream added as workspace dep adjacent to tokio (D-19)"
  - "tokio promoted to runtime dep for lattice-api (live path needs tokio::spawn, mpsc)"
  - "build_commit_input is the single commit construction path for both buffered and live (D-16)"
  - "derive_terminal_event derives status from AgentTurnResult.status, not ModelFinished presence (D-10, WR-01)"
metrics:
  duration: "2m 33s"
  completed: "2026-05-22T04:50:08Z"
  tasks: 2
  files_changed: 5
---

# Phase 16 Plan 01: Shared Infrastructure for Live Stream Summary

Add tokio-stream dependency, promote tokio to runtime dep, and extract shared commit construction helpers so both buffered and live paths share one code path for TurnCommitInput assembly and terminal event derivation.

## One-liner

tokio-stream workspace dep + build_commit_input/derive_terminal_event shared helpers extracted from buffered path

## Tasks Completed

| Task | Name | Commit | Key Changes |
|------|------|--------|-------------|
| 1 | Add tokio-stream workspace dep and promote tokio | 49e8711 | Cargo.toml, crates/lattice-api/Cargo.toml |
| 2 | Extract shared helpers | b2cdecd | execute.rs refactored, mod.rs updated, live.rs placeholder |

## Changes Made

### Task 1: Dependency changes
- Added `tokio-stream = "0.1"` to workspace `[workspace.dependencies]`
- Added `tokio = { workspace = true }` to lattice-api `[dependencies]` (was dev-only)
- Added `tokio-stream = { workspace = true }` to lattice-api `[dependencies]`

### Task 2: Shared helper extraction
- `TurnCommitContext` (pub(super)): 5-field struct holding session_id, parent_ids, model, provider, profile_snapshot_id
- `build_commit_input(user_input, ctx, agent_result) -> TurnCommitInput` (pub(super)): prepends user message, extends with agent messages, derives commit message, maps status, builds TurnCommitInput
- `derive_terminal_event(result) -> Option<TurnEvent>` (pub(super)): Error -> TurnFailed, Cancelled -> TurnCancelled, Completed -> None
- `execute_turn_inner` refactored to construct TurnCommitContext and call build_commit_input
- `stream_turn_with_deps` refactored to call derive_terminal_event instead of inline match
- `mod live;` added to mod.rs with placeholder live.rs file

## Deviations from Plan

None - plan executed exactly as written.

## Verification

- `cargo test -p lattice-api`: 34 tests pass (3 suites)
- `cargo clippy -p lattice-api -- -D warnings`: no warnings
- `cargo fmt -p lattice-api --check`: formatted

## Self-Check: PASSED
