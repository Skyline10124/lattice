---
phase: 17-failure-matrix-and-run-stream-consistency
plan: 01
subsystem: lattice-agent
tags: [timeout, cooperative-deadline, streaming, partial-preservation]
dependency_graph:
  requires: []
  provides: [cooperative-deadline, timeout-partial-preservation]
  affects: [lattice-api-turn-layer]
tech_stack:
  added: []
  patterns: [deadline-aware-await-helper, stream-outcome-timeout-variant]
key_files:
  created: []
  modified:
    - crates/lattice-agent/src/agent_loop/mod.rs
    - crates/lattice-agent/tests/live_sink_and_cancel.rs
    - crates/lattice-agent/tests/agent_loop.rs
decisions:
  - "Added StreamOutcome::Timeout variant to cleanly separate timeout from LLM errors (avoids 'LLM error: timeout' prefix)"
  - "Cooperative deadline computed as Option<Instant> before run_turn_inner, threaded through stream creation, stream consumption, and tool execution"
  - "Expired deadlines are checked before awaiting; selects use sleep_until rather than saturating zero-duration sleeps"
metrics:
  duration: 594s
  completed: "2026-05-22T07:35:00Z"
  tasks: 2
  files_modified: 3
---

# Phase 17 Plan 01: Cooperative Deadline Timeout Summary

Replaced outer `apply_optional_timeout` wrapper with cooperative deadline handling threaded through `run_turn_inner`, `consume_model_stream`, and tool execution, preserving partial state on timeout at stream creation, stream consumption, and tool execution await points.

## One-liner

Cooperative deadline replaces the outer timeout wrapper; partial content, events, and usage are preserved on timeout with live/result event consistency proven.

## Changes

### Task 1: Implement cooperative deadline in agent loop
- Removed `apply_optional_timeout` function entirely
- Added `deadline: Option<Instant>` parameter to `run_turn_inner` and `consume_model_stream`
- Added a shared deadline-aware await helper with a pre-await expiry check
- `run_turn` and `run_turn_live` compute `deadline` from `request.timeout.map(|d| Instant::now() + d)` before calling `run_turn_inner`
- `stream_chat().await` is wrapped against the deadline — timeout before stream creation preserves accumulated events (ModelStarted)
- `stream.next()` loop uses the same helper and `sleep_until`, with timeout winning before any ready event if the deadline has already expired
- `ToolCallExecutor::execute` is wrapped against the deadline — timeout during tool execution returns `Error(timeout)` without emitting a fake `ToolCallFinished`
- Added `StreamOutcome::Timeout` variant with dedicated handling in `run_turn_inner` (clean `terminal_reason: "timeout"`)
- Updated existing `agent_loop_timeout_returns_failed_status` test to expect ModelStarted preservation

### Task 2: Cooperative deadline timeout tests
- Replaced `scenario_08_run_turn_live_honors_timeout` with `scenario_08_timeout_cooperative_preserves_partial`
- Added `scenario_08b_timeout_on_stream_creation` for stream_chat hang path
- Added `scenario_08c_timeout_during_tool_execution_preserves_state` for tool execution deadline coverage
- Added `scenario_08d_expired_deadline_wins_over_ready_stream` for timeout-priority regression coverage
- `SlowStreamLlm`: yields Token("partial "), Token("content") with delays, then hangs
- `HangingStreamChatLlm`: `stream_chat` itself never returns (hangs on `futures::future::pending`)
- D-15 assertions proven: status==Error, terminal_reason=="timeout", events non-empty, partial content preserved, messages flushed, `received == result.events`
- Tool-timeout assertions proven: `ToolCallStarted` may be present, `ToolCallFinished` is not faked, assistant tool-call message is preserved, and no tool-result message is emitted

## Commits

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | Implement cooperative deadline in agent loop | e292a84 | `mod.rs` (deadline threading, select!, remove apply_optional_timeout) |
| 2 | Cooperative deadline timeout tests | f9e5c95 | `live_sink_and_cancel.rs` (scenario_08, 08b), `mod.rs` (StreamOutcome::Timeout) |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] StreamOutcome::Timeout variant added**
- **Found during:** Task 2
- **Issue:** Using `StreamOutcome::Error("timeout")` caused `run_turn_inner` to prepend "LLM error: " to the terminal_reason, producing "LLM error: timeout" instead of the expected "timeout"
- **Fix:** Added dedicated `StreamOutcome::Timeout` variant with its own match arm that returns `terminal_reason: Some("timeout".to_string())` directly
- **Files modified:** `crates/lattice-agent/src/agent_loop/mod.rs`
- **Commit:** f9e5c95

**2. [Rule 1 - Bug] Updated existing timeout test assertion**
- **Found during:** Task 1
- **Issue:** `agent_loop_timeout_returns_failed_status` asserted `result.events.is_empty()` which was the old broken behavior (outer timeout dropped all state). With cooperative deadline, ModelStarted is emitted before stream_chat.
- **Fix:** Changed assertion to verify ModelStarted IS present in events (the correct cooperative behavior)
- **Files modified:** `crates/lattice-agent/tests/agent_loop.rs`
- **Commit:** e292a84

## Verification

```
cargo test -p lattice-agent --features test-utils: 52 passed
cargo clippy -p lattice-agent --features test-utils -- -D warnings: No issues
cargo build (full workspace): success
cargo test -p lattice-agent --features test-utils timeout: passed after review fixes
```

## Self-Check: PASSED

- All modified files exist on disk
- Both task commits (e292a84, f9e5c95) found in git log
- `apply_optional_timeout` confirmed removed (0 occurrences)
- `deadline: Option<Instant>` present in both `run_turn_inner` and `consume_model_stream` signatures
- Deadline helper covers `stream_chat`, `stream.next`, and `ToolCallExecutor::execute`
