---
phase: 17-frontend-adapter-migration
plan: 01
subsystem: runtime
tags: [rust, runtime-turns, cancellation, approvals, plugin-overlay, tests]
requires:
  - phase: 16-runtime-owned-agent-instance-turn-lifecycle
    provides: Runtime-owned handles, submit_turn, TurnCompletion, and Phase 16 UAT gap diagnosis
provides:
  - Deterministic runtime submit-turn tests for semantic events, same-handle busy guard, and completion fields
  - Runtime-owned cancellation API with Cancelled completion storage and partial assistant text preservation
  - Handle-owned approval and plugin overlay APIs for later frontend migration
  - Explicit missing-internal-agent recovery error semantics
affects: [frontend-adapter-migration, tui-runtime-adapter, cli-runtime-adapter]
tech-stack:
  added: []
  patterns:
    - Runtime owns terminal turn status and completion storage
    - Frontends mutate approval and plugin state through handle-scoped runtime APIs
key-files:
  created:
    - .planning/phases/17-frontend-adapter-migration/17-01-SUMMARY.md
  modified:
    - crates/lattice-runtime/src/turn.rs
    - crates/lattice-runtime/src/handle.rs
    - crates/lattice-runtime/src/lib.rs
key-decisions:
  - Runtime cancellation is represented by handle-scoped active turn state and is persisted as TurnStatus::Cancelled.
  - Missing internal agents return a typed config message containing "rebuild required" instead of leaving frontends to infer recovery behavior.
  - RuntimePluginOverlay is stored on HandleState and cleared on security policy changes.
requirements-completed: [FE-01, FE-02, FE-03, FE-04]
duration: 17min
completed: 2026-05-13
---

# Phase 17 Plan 01: Runtime Turn Gap Closure Summary

**Runtime turn lifecycle hardening with deterministic submit-turn tests, authoritative cancellation, handle-owned approval, and plugin overlay state**

## Performance

- **Duration:** 17 min
- **Started:** 2026-05-13T13:38:11Z
- **Completed:** 2026-05-13T13:55:39Z
- **Tasks:** 3
- **Files modified:** 3 source files plus this summary

## Accomplishments

- Added deterministic runtime submit-turn tests for semantic `RuntimeTurnEvent` output, same-handle busy rejection, and authoritative completion fields.
- Added `Runtime::cancel_turn` and handle active-turn cancellation state so cancelled turns store `TurnStatus::Cancelled`, emit `RuntimeTurnEvent::Cancelled`, preserve partial assistant text, and avoid success final reports.
- Added `RuntimePluginOverlay`, `Runtime::set_agent_plugin_overlay`, and `Runtime::approve_handle_pending_proposal` so frontend plans can migrate plugin and approval flows without exposing live `Agent`.
- Made missing internal agent recovery explicit with `handle has no internal agent; rebuild required` and regression coverage.

## Task Commits

1. **Task 1: Add deterministic runtime submit-turn tests and completion assertions** - `b050639` (`test`)
2. **Task 2: Make runtime cancellation reachable and completion-owned** - `bd5e6a2` (`feat`)
3. **Task 3: Add handle-owned approval, plugin overlay, and recovery semantics** - `ab3f38f` (`feat`)
4. **Formatting gate** - `4a06e08` (`style`)

## Files Created/Modified

- `crates/lattice-runtime/src/turn.rs` - Added scripted runtime turn test harness, submit-turn tests, cancellation API, cancellation-aware event/completion handling, and cancellation regression tests.
- `crates/lattice-runtime/src/handle.rs` - Added active turn id, cancellation flag, and `RuntimePluginOverlay` storage.
- `crates/lattice-runtime/src/lib.rs` - Re-exported overlay type, initialized new handle fields, added plugin overlay setter, handle approval API, approval resolution helpers, security-policy overlay clearing, and recovery tests.
- `.planning/phases/17-frontend-adapter-migration/17-01-SUMMARY.md` - Execution summary.

## Decisions Made

- Cancellation is cooperative at the runtime event/completion layer: provider streaming may finish internally, but runtime stops emitting post-cancel semantic events and stores the completion as cancelled.
- Cancelled completions intentionally omit `final_report` so frontends cannot render them as success.
- Approval mutation uses the handle's live `TaskState` and shared `RuntimeSecurity`; `TurnCompletion.task_state_snapshot` remains read-only display/history data.
- A security-policy change during handle rebuild clears plugin overlay state to avoid carrying plugin context into a new authorization context.

## Deviations from Plan

None - plan executed as written. The extra `style(17-01)` commit is a mechanical rustfmt fix after the formatting gate and does not change behavior.

## Issues Encountered

- The sandboxed environment denied loopback binds for the scripted SSE tests. This is an expected skip path in the plan; all affected tests printed the loopback denial skip message and returned `ok`.
- `cargo fmt --all --check` initially reported formatting differences. Ran `cargo fmt --all`, re-ran verification, and committed the formatting-only fix in `4a06e08`.

## Verification

| Command | Result |
|---------|--------|
| `cargo test -p lattice-runtime runtime_turn_ -- --nocapture` | PASS; 5 tests passed, scripted loopback tests skipped due bind denial |
| `cargo test -p lattice-runtime runtime_turn_cancel -- --nocapture` | PASS; 2 tests passed, scripted loopback tests skipped due bind denial |
| `cargo test -p lattice-runtime handle_approval -- --nocapture` | PASS |
| `cargo test -p lattice-runtime runtime_plugin_overlay -- --nocapture` | PASS |
| `cargo test -p lattice-runtime internal_agent_recovery -- --nocapture` | PASS |
| `cargo fmt --all --check` | PASS |

## Known Stubs

None. Stub scan only found an existing test comment containing the word "placeholder" and a format string false positive; no UI-facing placeholder or unwired runtime data was introduced.

## Threat Flags

None. The new cancellation, approval, plugin overlay, and recovery surfaces match the plan threat model and include focused regression tests.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Runtime turn gaps that blocked the frontend hard cut are closed. Later Phase 17 plans can route CLI/TUI adapters through runtime handles, call `cancel_turn`, use `approve_handle_pending_proposal`, and store plugin overlay data without exposing live `Agent`.

## Self-Check: PASSED

- Found `crates/lattice-runtime/src/turn.rs`
- Found `crates/lattice-runtime/src/handle.rs`
- Found `crates/lattice-runtime/src/lib.rs`
- Found `.planning/phases/17-frontend-adapter-migration/17-01-SUMMARY.md`
- Found commits `b050639`, `bd5e6a2`, `ab3f38f`, and `4a06e08`
- Confirmed `.planning/STATE.md` and `.planning/ROADMAP.md` remain uncommitted pre-existing parent-orchestrator changes

---
*Phase: 17-frontend-adapter-migration*
*Completed: 2026-05-13*
