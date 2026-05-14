---
phase: 13-agent-task-recovery-state
plan: 03
subsystem: agent-state
tags: [manifest-metadata, recovery-provider, session-resume, task-state]
dependency_graph:
  requires: [13-02]
  provides: [13-04]
  affects: [lattice-agent, lattice-cli, lattice-session]
tech_stack:
  added: [Arc<Mutex<TaskState>>, ContextProvider impl]
  patterns: [shared-state-handle, provider-rendering, manifest-metadata-round-trip]
key_files:
  created:
    - crates/lattice-agent/src/prompt/task_state_provider.rs
  modified:
    - crates/lattice-agent/src/manifest.rs
    - crates/lattice-agent/src/state.rs
    - crates/lattice-agent/src/prompt/mod.rs
    - crates/lattice-agent/src/config.rs
    - crates/lattice-agent/src/turn_engine.rs
    - crates/lattice-cli/src/session.rs
    - crates/lattice-cli/src/coding_agent.rs
decisions:
  - "TaskState serialized directly as manifest metadata JSON instead of a separate snapshot type, keeping the canonical type as single source of truth"
  - "AgentState.task_state refactored to Arc<Mutex<TaskState>> for shared provider access rather than duplicating state or using stale clones"
  - "TaskStateProvider uses Layer::Memory with 15% token budget ratio, matching the pattern used by MemoryProvider"
  - "Resume metadata loading fails gracefully to messages-only when no manifests or metadata exist"
metrics:
  duration_seconds: 1459
  completed_date: "2026-05-11T18:39:22Z"
---

# Phase 13 Plan 03: Manifest Metadata Recovery Bridge Summary

**One-liner:** Task/recovery state round-trips through turn manifest metadata with a prompt provider rendering the active plan, changed files, and unverified work on resume.

## What Was Built

Turn manifests now carry a serialized snapshot of the agent's live `TaskState` as a `metadata` JSON field. The agent's task state was refactored from a plain `TaskState` struct field to `Arc<Mutex<TaskState>>` so a prompt context provider can read the current state during prompt assembly without holding a stale clone.

A new `TaskStateProvider` implements the `ContextProvider` contract and renders a structured recovery summary section when the task state is non-empty -- displaying the active plan, changed files, tool failures, verification failures, and unverified work. The section is scoped to `Layer::Memory` with a 15% token budget ratio.

The CLI session layer now preserves manifest metadata through the `agent_manifest_to_new()` conversion (previously `serde_json::Value::Null`), and the coding agent resume path loads the latest turn manifest metadata from the session store to seed the agent's task state before the first resumed turn. When no metadata exists, resume falls back to messages-only behavior.

## Tests

- **lattice-agent manifest:** 5 passed -- manifest collection and metadata round-trip
- **lattice-agent prompt:** 15 passed -- 8 new provider tests + existing memory provider tests
- **lattice-cli session:** 5 passed -- metadata preservation and null-handling tests
- **lattice-session turn_manifest:** 3 passed -- session storage tests unchanged
- **Full lattice-agent:** 153 passed (8 new provider tests)
- **Full lattice-cli:** 89 passed

## Data Flow

```
Tool call outcomes -> TaskState mutations (via Mutex)
                          |
           +--------------+--------------+
           |                             |
    Turn manifest metadata       TaskStateProvider
    (serde_json::Value)         (produce() reads Arc<Mutex>)
           |                             |
    Session persistence           Prompt assembly
    (lattice-session)            (recovery summary section)
           |
    Resume: load latest metadata -> seed TaskState -> Provider renders in next turn
```

## Deviations from Plan

None -- plan executed as written.

## Decisions Made

1. **Direct TaskState serialization over snapshot type:** The plan suggested a `TaskRecoverySnapshot` type, but `TaskState` already derives `Serialize`/`Deserialize` and contains all required fields. Direct serialization avoids maintaining two parallel types and ensures no fields are lost in translation. The `TaskRecoverySnapshot` struct was removed before commit to avoid dead code.

2. **Mutex over RwLock for shared state:** `std::sync::Mutex` was chosen over `RwLock` because the task state is frequently mutated during tool execution and infrequently read during prompt assembly. Mutex is simpler and avoids RwLock's additional constraints (TaskState is Send but Sync is not needed for the short lock durations).

3. **Layer::Memory for recovery section:** The provider uses `Layer::Memory` to match the existing pattern used by `MemoryProvider`. This keeps the recovery summary scoped as session-level operational state and ensures it is rendered in the correct prompt position without competing with system or tool layers.

## Known Stubs

None -- all data paths are wired end-to-end.

## Threat Flags

None -- no new network endpoints, auth paths, or file access patterns were introduced. Metadata persists in the existing SQLite session store and is rendered through the existing prompt provider pipeline.

## Self-Check: PASSED

- [x] crates/lattice-agent/src/prompt/task_state_provider.rs exists and committed
- [x] Commit 49b485a (Task 1) verified: `git log --oneline` confirms
- [x] Commit c742e19 (Task 2) verified: `git log --oneline` confirms
- [x] All tests pass: agent (153), CLI (89), session (3)
- [x] Full workspace builds with zero warnings
