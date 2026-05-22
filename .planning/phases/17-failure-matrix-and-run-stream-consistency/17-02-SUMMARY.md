---
phase: 17-failure-matrix-and-run-stream-consistency
plan: 02
subsystem: lattice-api/turn
tags: [testing, commit-failure, cancellation, receiver-drop, VER-10, VER-12]
dependency_graph:
  requires: [phase-16-live-api-stream]
  provides: [deterministic-commit-failure-proof, cancelled-buffered-seam-proof, receiver-drop-proof]
  affects: [crates/lattice-api/src/turn/live_tests.rs, crates/lattice-api/src/turn/tests.rs]
tech_stack:
  added: []
  patterns: [BlockingLlmClient-unfold-state-machine, tokio-sync-Notify-synchronization]
key_files:
  created: []
  modified:
    - crates/lattice-api/src/turn/live_tests.rs
    - crates/lattice-api/src/turn/tests.rs
decisions:
  - "BlockingLlmClient uses futures::stream::unfold with Pre/Post phase enum and tokio::sync::Notify for deterministic blocking"
  - "Receiver drop test uses block-scoped stream lifetime instead of explicit drop() to satisfy clippy"
  - "cancelled_result() factory already existed; reused for stream_turn_cancelled_commits_cancelled"
metrics:
  duration: 30m
  completed: 2026-05-22
  tasks: 2
  files_modified: 2
---

# Phase 17 Plan 02: Failure Matrix Behavioral Tests Summary

Deterministic commit failure with real SQLite conflict, receiver drop tolerance, cancelled commit path through buffered seam, and error-after-partial preservation.

## Commits

| Task | Name | Commit | Key Changes |
|------|------|--------|-------------|
| 1 | Deterministic commit failure and receiver drop tests | 0645be0 | BlockingLlmClient, replaced racy test, receiver drop assertion |
| 2 | Cancelled buffered seam test | a976701 | stream_turn_cancelled_commits_cancelled, stream_turn_error_after_partial_commits_error |

## What Was Built

### Task 1: Deterministic Commit Failure and Receiver Drop

- **BlockingLlmClient**: A streaming LLM fake using `futures::stream::unfold` with a `Pre`/`Post` phase state machine. Yields pre-block events, then awaits a `tokio::sync::Notify` signal before emitting post-resume events. Gives tests deterministic control over when the agent loop progresses past model consumption.

- **stream_turn_live_commit_failure_deterministic** (replaces racy `stream_turn_live_commit_failure_emits_commit_failed`): Uses BlockingLlmClient to guarantee the racing commit is inserted while the agent is blocked. Asserts: CommitFailed present with non-empty reason, TurnCommitted absent, HEAD has exactly 2 commits (first + racing), TokenDelta events appear before CommitFailed proving live activity before commit.

- **stream_turn_live_receiver_drop_does_not_cancel**: Proves VER-12 — background task completes and commits despite the stream being dropped after receiving only TurnStarted.

### Task 2: Cancelled Buffered Seam and Error After Partial

- **stream_turn_cancelled_commits_cancelled** (VER-10/D-05): Exercises Cancelled path through `stream_turn_with_deps`. Asserts: TurnCancelled emitted with reason "cancelled by caller", TurnCommitting emitted, TurnCommitted with status Cancelled, correct ordering (TurnCancelled < TurnCommitting < TurnCommitted), commit input status is TurnCommitStatus::Cancelled, terminal_reason preserved.

- **stream_turn_error_after_partial_commits_error**: Proves partial assistant content is preserved in commit messages on Error path. Asserts: TurnFailed emitted with error reason, TurnCommitted with Error status, commit messages contain user + partial assistant message, terminal_reason preserved.

## Deviations from Plan

None - plan executed exactly as written.

## Self-Check: PASSED

- [x] crates/lattice-api/src/turn/live_tests.rs exists
- [x] crates/lattice-api/src/turn/tests.rs exists
- [x] Commit 0645be0 exists in git log
- [x] Commit a976701 exists in git log
- [x] All 44 lattice-api tests pass (40 lib + 4 integration)
- [x] Clippy clean with -D warnings
