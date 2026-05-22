---
phase: 17-failure-matrix-and-run-stream-consistency
plan: 03
subsystem: lattice-api
tags: [consistency, run-turn, stream-turn, integration-test, VER-13]
dependency_graph:
  requires: [cooperative-deadline, timeout-partial-preservation]
  provides: [run-stream-consistency-proof, live-ordering-proof]
  affects: []
tech_stack:
  added: []
  patterns: [NormalizedTurnObservation, dual-tempdir-isolation, session-store-anchored-comparison]
key_files:
  created:
    - crates/lattice-api/tests/consistency.rs
  modified: []
decisions:
  - "Both observation builders read assistant_output and transcript from committed session store, not from live events or result fields (D-09)"
  - "Both observation builders read usage and terminal_reason from committed log metadata after review fixes"
  - "Zero-usage committed metadata is normalized to None for consistent comparison between buffered and streaming error paths"
  - "FakeStreamingLlmClient used for both paths to ensure identical LLM behavior through stream_chat"
metrics:
  duration: 250s
  completed: "2026-05-22T08:09:22Z"
  tasks: 1
  files_created: 1
---

# Phase 17 Plan 03: Run/Stream Consistency Summary

Proved semantic consistency between `run_turn` (buffered) and `stream_turn` (live) paths across three scenarios using NormalizedTurnObservation comparison, plus live activity ordering verification.

## One-liner

NormalizedTurnObservation comparison proves run/stream durable-history consistency across content-only, error-partial, and tool-call scenarios; committed usage and terminal_reason metadata are compared directly; live ordering (TokenDelta before TurnCommitting) verified.

## Changes

### Task 1: NormalizedTurnObservation and consistency test infrastructure

- Created `crates/lattice-api/tests/consistency.rs` with 4 integration tests
- `NormalizedTurnObservation` struct: status, usage, assistant_output, tool_results, transcript_roles_and_content, terminal_reason
- `observe_run_turn`: reads committed transcript, usage, and terminal_reason via `SqliteSessionGraphStore`
- `observe_stream_turn`: reads committed transcript, usage, and terminal_reason from the session store; error scenarios also assert public `TurnFailed.reason` matches committed metadata
- `consistency_completed_content_only`: content-only scenario with assert_eq on observations
- `consistency_error_after_partial_token`: both paths commit partial content "partial resp" with Error status
- `consistency_tool_call_path`: tool results (read_file) and multi-round accumulated usage match
- `stream_turn_emits_activity_before_commit`: proves ModelStarted < TokenDelta < TurnCommitting < TurnCommitted ordering

## Commits

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | NormalizedTurnObservation and consistency test infrastructure | 8dd8d00 | `crates/lattice-api/tests/consistency.rs` |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Zero-usage normalization for error path consistency**
- **Found during:** Task 1
- **Issue:** `run_turn` always returns `Some(TurnUsageSummary { 0, 0, 0 })` from `AgentTurnResult.usage` on error (no ModelFinished emitted), while stream observation returns `None` (no ModelFinished event to extract from)
- **Fix:** Normalize zero-usage to `None` in `observe_run_turn` so both sides agree
- **Files modified:** `crates/lattice-api/tests/consistency.rs`
- **Commit:** review closeout

**2. [Rule 1 - Bug] Durable metadata comparison for usage and terminal_reason**
- **Found during:** Task 1
- **Issue:** Initial consistency checks compared public result/event usage and only error event reasons, but did not prove committed metadata consistency.
- **Fix:** Both observation builders now read usage and terminal_reason from the last committed log entry; error stream observations separately assert `TurnFailed.reason` equals the committed terminal reason.
- **Files modified:** `crates/lattice-api/tests/consistency.rs`
- **Commit:** review closeout

## Verification

```
cargo test -p lattice-api --test consistency: 4 passed
cargo test -p lattice-api: 48 passed (4 suites)
cargo clippy -p lattice-api -- -D warnings: No issues
cargo test --workspace: passed after review fixes
```

## Self-Check: PASSED

- `crates/lattice-api/tests/consistency.rs` exists on disk
- Task commit 8dd8d00 found in git log
- NormalizedTurnObservation struct present with all required fields
- Both observation builders read transcript, usage, and terminal_reason from committed session state
- Independent tempdirs confirmed (D-07)
