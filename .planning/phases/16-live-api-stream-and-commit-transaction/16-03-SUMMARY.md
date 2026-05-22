---
phase: 16-live-api-stream-and-commit-transaction
plan: 03
subsystem: lattice-api/turn
tags: [testing, live-events, integration-tests, behavioral-tests]
dependency_graph:
  requires: [16-01, 16-02]
  provides: [live_tests.rs, stream_turn_live behavioral coverage]
  affects: []
tech_stack:
  added: []
  patterns: [real-session-api-testing, fake-streaming-llm-client, stale-parent-conflict-injection]
key_files:
  created:
    - crates/lattice-api/src/turn/live_tests.rs
  modified:
    - crates/lattice-api/src/turn/mod.rs
    - crates/lattice-api/Cargo.toml
decisions:
  - "Tests use real SessionApi + AgentLoop with FakeStreamingLlmClient (no internal mocking)"
  - "Commit failure tested via racing concurrent commit to create stale parent conflict"
  - "NoOpToolExecutor rejects all calls since tests don't exercise tool paths"
metrics:
  duration: "7m 37s"
  completed: "2026-05-22T05:15:48Z"
  tasks: 1
  files_changed: 3
---

# Phase 16 Plan 03: Live stream_turn Behavioral Tests Summary

Write behavioral tests for the live stream_turn path covering startup failure, event ordering, all three terminal statuses with commit, and commit failure.

## One-liner

7 integration tests proving live path startup validation, event ordering, terminal status commit, delta-only persistence, branch population, and commit failure handling

## Tasks Completed

| Task | Name | Commit | Key Changes |
|------|------|--------|-------------|
| 1 | Create live_tests.rs with behavioral tests for the live stream_turn path | e67e46a | 7 tests, mod.rs updated, Cargo.toml dev-dep added |

## Changes Made

### Task 1: live_tests.rs (e67e46a)

- Created `crates/lattice-api/src/turn/live_tests.rs` (556 lines) with 7 behavioral tests
- Added `#[cfg(test)] mod live_tests;` to `mod.rs`
- Added `lattice-agent = { workspace = true, features = ["test-utils"] }` to `[dev-dependencies]`

**Tests implemented:**

1. `stream_turn_live_basic_completed` -- Happy path: TurnStarted -> TokenDelta -> ModelFinished -> TurnCommitting -> TurnCommitted(Completed), session_id consistency
2. `stream_turn_live_error_commits_error` -- Error path: TurnFailed before TurnCommitting, TurnCommitted with Error status, no CommitFailed
3. `stream_turn_live_event_ordering` -- Strict ordering: TurnStarted first, ModelStarted -> TokenDelta -> ModelFinished -> TurnCommitting -> TurnCommitted last
4. `stream_turn_live_startup_failure_no_stream` -- Non-existent session_id returns Result::Err (API-04/API-05)
5. `stream_turn_live_commit_stores_delta_only` -- Committed messages = user + agent only (COMM-01), commit_message <= 72 chars (COMM-04)
6. `stream_turn_live_branch_populated` -- TurnStarted.branch is Some("main") when session is on a branch
7. `stream_turn_live_commit_failure_emits_commit_failed` -- Racing concurrent commit creates stale parent; CommitFailed emitted, no TurnCommitted (COMM-03)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Commit failure test strategy changed**
- **Found during:** Task 1 initial implementation
- **Issue:** Deleting SQLite DB file does not cause commit failure because WAL mode keeps the connection alive in memory
- **Fix:** Used racing concurrent commit to create a stale parent conflict, which reliably triggers CommitFailed
- **Files modified:** crates/lattice-api/src/turn/live_tests.rs
- **Commit:** e67e46a

## Verification

- `cargo test -p lattice-api stream_turn_live`: 7 tests pass
- `cargo test -p lattice-api`: 41 tests pass (3 suites, no regressions)
- `cargo clippy -p lattice-api -- -D warnings`: no warnings
- `cargo fmt -p lattice-api --check`: formatted

## Self-Check: PASSED

- [x] `crates/lattice-api/src/turn/live_tests.rs` exists (556 lines, exceeds 150 minimum)
- [x] Commit e67e46a verified in git log
