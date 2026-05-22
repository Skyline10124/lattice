---
phase: 16-live-api-stream-and-commit-transaction
plan: 02
subsystem: lattice-api/turn
tags: [streaming, live-events, tokio-spawn, mpsc, commit-transaction]
dependency_graph:
  requires: [16-01]
  provides: [stream_turn_live, PreparedTurn, run_live_turn_task, emit_turn_event]
  affects: [16-03]
tech_stack:
  added: []
  patterns: [tokio-select-concurrent-drain, fire-and-forget-send, spawn-and-return-stream]
key_files:
  created: []
  modified:
    - crates/lattice-api/src/turn/live.rs
    - crates/lattice-api/src/turn/execute.rs
    - Cargo.lock
decisions:
  - "stream_turn delegates to live::stream_turn_live (no buffered replay in production)"
  - "stream_turn_with_deps preserved as buffered replay seam for tests (D-01)"
  - "map_agent_event is pub(super) so live.rs can import it"
  - "tokio::select! concurrent drain loop is the mandatory architecture (D-20)"
  - "Receiver drop does not cancel the turn; send errors silently ignored (D-03)"
metrics:
  duration: "3m 12s"
  completed: "2026-05-22T05:08:00Z"
  tasks: 2
  files_changed: 3
---

# Phase 16 Plan 02: Live stream_turn Implementation Summary

Implement the live `stream_turn` path with PreparedTurn startup validation, a spawned background task using tokio::select! concurrent drain, and rewrite stream_turn to delegate to the live path.

## One-liner

Live stream_turn with tokio::spawn + select! drain loop + commit transaction, replacing buffered replay in production

## Tasks Completed

| Task | Name | Commit | Key Changes |
|------|------|--------|-------------|
| 1 | Implement live.rs with PreparedTurn, prepare_live_turn, and run_live_turn_task | dfafd51 | Full live path: PreparedTurn struct, prepare_live_turn, run_live_turn_task with select! loop, emit_turn_event, stream_turn_live |
| 2 | Rewrite stream_turn to delegate to live path, make map_agent_event pub(super) | 9426eaf | stream_turn delegates to live path, doc comments updated, live.rs import formatting fix, Cargo.lock included |

## Changes Made

### Task 1: live.rs implementation (dfafd51)
- `PreparedTurn` (pub(crate)): 9-field struct holding all startup validation state
- `prepare_live_turn`: resolves model/provider, creates/opens session, reads transcript, extracts branch from status
- `run_live_turn_task`: emits TurnStarted, builds agent request, runs select! concurrent drain loop, post-loop drain, terminal event, TurnCommitting, commit with TurnCommitted/CommitFailed
- `emit_turn_event`: fire-and-forget `let _ = sender.send(event).await` (D-03)
- `stream_turn_live`: calls prepare_live_turn, spawns task, returns ReceiverStream
- `TURN_EVENT_CHANNEL_CAPACITY = 256` (D-17)

### Task 2: execute.rs delegation (9426eaf)
- `stream_turn` body: `super::live::stream_turn_live(session, agent_loop, request).await`
- `stream_turn` doc comment: "Execute a turn and return its events as a live stream"
- `stream_turn_with_deps` doc comment: "Buffered replay seam for tests" (D-01)
- `#[allow(dead_code)]` on `stream_turn_with_deps` and `ExecutedTurn` (no longer called from production)
- `live.rs` import formatting fix (rustfmt compliance)
- `Cargo.lock` included (tokio-stream lockfile entry missed from 16-01)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Cargo.lock missing tokio-stream entry**
- **Found during:** Task 2
- **Issue:** Plan 16-01 commit 49e8711 added tokio-stream to Cargo.toml but did not include the Cargo.lock update
- **Fix:** Included Cargo.lock in Task 2 commit
- **Files modified:** Cargo.lock
- **Commit:** 9426eaf

**2. [Rule 1 - Bug] live.rs import line too long for rustfmt**
- **Found during:** Task 2 verification
- **Issue:** Single-line import of 4 items from `super::execute` exceeded rustfmt line width
- **Fix:** Split into multi-line import block
- **Files modified:** crates/lattice-api/src/turn/live.rs
- **Commit:** 9426eaf

## Verification

- `cargo test -p lattice-api`: 34 tests pass (3 suites)
- `cargo clippy -p lattice-api -- -D warnings`: no warnings
- `cargo fmt -p lattice-api --check`: formatted

## Self-Check: PASSED
