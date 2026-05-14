---
phase: 13-agent-task-recovery-state
plan: 02
subsystem: lattice-agent
tags: [task-state, recovery, tool-result, todo_write, patch, bash, denial]
requires: [13-01]
provides: []
affects:
  - crates/lattice-agent/src/tool_result.rs
  - crates/lattice-agent/src/turn_engine.rs
tech-stack:
  added: []
  patterns: [tool-result-classification, typed-todo-updates, recovery-record-feed]
key-files:
  created: []
  modified:
    - crates/lattice-agent/src/tool_result.rs (record_todo_result, classify_tool_result, helpers)
    - crates/lattice-agent/src/turn_engine.rs (call sites for tool result capture)
decisions:
  - non-verification bash: "Classify bash failures via output patterns (Error:, CommandError, ERR: prefix) rather than exit codes, since the rendered tool result does not carry structured process status."
  - verification-heuristic: "Use substring matching on command text (cargo test, pytest, npm test, etc.) rather than exhaustively parsing command structure; future plans can refine this with tool-catalog awareness."
  - denial-ordering: "__approval_proposal__ detection precedes DenialReport deserialization because enriched denial JSON carries both markers and serde ignores unknown fields."
  - blocker-model: "Record denials and approval blockers as ToolFailureRecord entries rather than introducing a new BlockerRecord type; the existing type carries sufficient fields for the provider to render blocking context."
metrics:
  duration: 26min
  completed_date: 2026-05-12
---

# Phase 13 Plan 02: Wire Tool Outcomes Into Task/Recovery State Summary

**One-liner:** Tool outcomes (todo_write, patch, bash, denial, verification) feed structured recovery records into live AgentState.task_state for the next turn.

## What Changed

### Task 1: todo_write becomes a durable state update

Added `Agent::record_todo_result` which parses `todo_write` tool-call arguments into typed `TodoItem` values and calls `TaskState::replace_todos` on the live state. Only successful results (those starting with `"Todo list updated:"`) are applied; errors and denials leave prior state unchanged. The method reuses the existing validation in `replace_todos` (at most one `in_progress`, non-empty content).

### Task 2: Classification helper for patch, bash, denial, and verification

Added `Agent::classify_tool_result` as the central dispatch point for turning raw tool result strings into typed recovery records:

- **Patch** (`PatchResultEnvelope`): Success records `ChangedFileRecord` per changed file and marks each path as unverified. Failure records `ToolFailureRecord` with diagnostic error codes in snake_case.
- **Bash**: Commands are classified as "verification" or "generic" via `is_verification_command` heuristic (cargo test/check/clippy/fmt, pytest, go test, npm test, eslint, mypy, etc.). Verification failures produce `VerificationRecord`; generic failures produce `ToolFailureRecord`. Success is a no-op.
- **Denial / Approval**: `DenialReport` with sentinel creates a blocker `ToolFailureRecord` carrying risk class and reason. Enriched `__approval_proposal__` results create a blocker with the suggested approve command.
- **Verification plugin**: JSON results with a `failed_steps` array produce one `VerificationRecord` per failed step.

Both `record_todo_result` and `classify_tool_result` are called from `TurnEngine::execute_tool_calls` immediately after the existing tool-result emission.

## Deviations from Plan

None — plan executed exactly as written.

## Self-Check: PASSED

- [x] `crates/lattice-agent/src/tool_result.rs` — modified, exists
- [x] `crates/lattice-agent/src/turn_engine.rs` — modified, exists
- [x] Commit `05b0d60` — Task 1 verified
- [x] Commit `0fda440` — Task 2 verified
- [x] All verification tests pass (state: 28, tool_result: 20, executor: 19)
