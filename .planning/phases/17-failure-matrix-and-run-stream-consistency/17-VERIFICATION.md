---
phase: 17-failure-matrix-and-run-stream-consistency
verified: 2026-05-22
status: passed
score: 4/4 success criteria verified
review_fixes: included
---

# Phase 17: Failure Matrix and Run/Stream Consistency — Verification Report

**Phase Goal:** Close semantic gaps between live streaming, buffered execution, failure behavior, and session history.
**Status:** passed

## Goal Achievement

| # | Success Criterion | Status | Evidence |
|---|-------------------|--------|----------|
| 1 | Tests cover error after partial tokens, cancellation paths, receiver drop, and commit failure | VERIFIED | `crates/lattice-api/src/turn/live_tests.rs`, `crates/lattice-agent/tests/live_sink_and_cancel.rs`, and `crates/lattice-api/tests/consistency.rs` cover these paths. |
| 2 | `stream_turn` emits live activity before commit | VERIFIED | `stream_turn_emits_activity_before_commit` asserts `ModelStarted` and `TokenDelta` arrive before `TurnCommitting` / `TurnCommitted`. |
| 3 | Final status, usage, tool summaries, and transcript delta match between `run_turn` and `stream_turn` | VERIFIED | `NormalizedTurnObservation` now reads durable transcript, usage, and terminal reason from committed session state. |
| 4 | HEAD movement is asserted for Completed, Error, Cancelled, and CommitFailed cases | VERIFIED WITH ALPHA CAVEAT | Completed/Error live paths and CommitFailed are covered on the public live path. Cancelled commit behavior is covered through the buffered seam because alpha `stream_turn` has no public cancellation handle. |

## Review Fixes

| Issue | Status | Evidence |
|-------|--------|----------|
| Timeout did not cover tool execution | FIXED | Tool execution awaits through the same cooperative deadline helper as model stream awaits. |
| Expired deadline could lose to a ready stream event | FIXED | Deadline expiry is checked before awaiting, and deadline sleeps use `sleep_until`. |
| Committed `terminal_reason` not compared | FIXED | Consistency observations read terminal reason from the last committed log entry. |
| Committed usage not compared | FIXED | Consistency observations read usage from the last committed log entry and normalize zero usage. |
| Receiver-drop test used fixed sleep | FIXED | Test now polls for commit completion with an outer timeout. |
| Live channel backpressure outside timeout | ACCEPTED GAP | Recorded in `STATE.md`; not expanded in Phase 17 to avoid redefining live mirror reliability semantics. |

## Behavioral Verification

| Command | Result |
|---------|--------|
| `cargo fmt --all --check` | pass |
| `cargo test -p lattice-agent --features test-utils timeout` | pass |
| `cargo test -p lattice-api stream_turn` | pass |
| `cargo test -p lattice-api --test consistency` | pass |
| `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` | pass |
| `cargo clippy -p lattice-api --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | pass |

## Accepted Gaps

- `emit_agent_event` awaits bounded live-channel `send`, so an attached receiver that stops draining can still apply backpressure outside the cooperative timeout.
- `RunTurnStage` is still a coarse breadcrumb field and should not be used as the Phase 18 live rendering source.
- Public live cancellation remains out of scope until a cancellation handle exists on `stream_turn`.

## Phase 18 Handoff

Phase 18 should move the maintained CLI `run` path from `run_turn` to `stream_turn` and render public `TurnEvent` values directly.
