---
phase: 17-frontend-adapter-migration
plan: 06
subsystem: testing
tags: [rust, cargo, runtime-turn, tui, regression-guards]

requires:
  - phase: 17-02
    provides: CLI runtime adapter for code/run/debug paths
  - phase: 17-03
    provides: TUI runtime turn adapter and completion state
  - phase: 17-04
    provides: queued prompt and cancel UX coverage
  - phase: 17-05
    provides: plugin and approval runtime migration
provides:
  - Frontend runtime source guards for CLI/TUI active paths
  - Behavior coverage replacing source-string bridge assertions
  - Workspace fmt, clippy, and test gate evidence for Phase 17
affects: [frontend-adapter-migration, lattice-cli, lattice-runtime, tui]

tech-stack:
  added: []
  patterns:
    - Source guards constrained to frontend-visible adapter files
    - Runtime behavior coverage through scripted model and ratatui render tests

key-files:
  created:
    - crates/lattice-cli/tests/frontend_runtime_guards.rs
  modified:
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/src/frontend/tui/ui.rs
    - crates/lattice-cli/tests/frontend_runtime_guards.rs

key-decisions:
  - "Source guards are limited to CLI/TUI frontend files so runtime and agent crates can continue owning Agent internally."
  - "Runtime bridge coverage now relies on behavior and render tests rather than identifier-presence assertions."

patterns-established:
  - "Frontend hard-cut guard: strip comments, then reject live Agent ownership and active LoopEvent rendering strings."
  - "TUI UI-SPEC strings are validated through rendered terminal output where practical."

requirements-completed: [FE-01, FE-02, FE-03, FE-04, EVAL-02, EVAL-04]

duration: 13min
completed: 2026-05-13
---

# Phase 17 Plan 06: Frontend Runtime Guard Hardening Summary

**Frontend runtime hard-cut guards with behavior-backed CLI/TUI coverage and clean workspace gates**

## Performance

- **Duration:** 13 min
- **Started:** 2026-05-13T16:05:12Z
- **Completed:** 2026-05-13T16:18:25Z
- **Tasks:** 3/3
- **Files modified:** 4

## Accomplishments

- Added `frontend_runtime_guards` integration tests that fail if active CLI/TUI frontend files reintroduce live `Agent` ownership, `run_streaming`, direct `.run(` calls, or active `LoopEvent` rendering.
- Removed the old `build_path_declares_runtime_turn_path` source-string assertion and replaced the missing coverage with behavior/render tests for approval usage and plugin statusline output.
- Ran the final Phase 17 workspace gates successfully: `cargo fmt --all --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace`.

## Task Commits

1. **Task 1: Add source guards against frontend live-Agent ownership** - `6765907` (`test`)
2. **Task 2: Replace source-string bridge tests with behavior coverage** - `235cd84` (`test`)
3. **Task 3: Run final workspace gates and document non-phase blockers** - summary/docs commit created after this file is committed

## Files Created/Modified

- `crates/lattice-cli/tests/frontend_runtime_guards.rs` - New regression guard scanning the six frontend files named by the plan.
- `crates/lattice-cli/src/coding_agent.rs` - Removed source-string runtime path assertion.
- `crates/lattice-cli/src/frontend/tui/ui.rs` - Added rendered approval usage and plugin statusline coverage.
- `.planning/phases/17-frontend-adapter-migration/17-06-SUMMARY.md` - Plan execution evidence.

## Verification

| Command | Outcome |
|---------|---------|
| `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` | PASS |
| `cargo test -p lattice-cli --test code_agent_eval runtime_turn -- --nocapture` | PASS |
| `cargo test -p lattice-runtime runtime_turn_busy_guard_rejects_same_handle_concurrency -- --nocapture` | PASS |
| `cargo test -p lattice-cli renders_ready_queued_prompt -- --nocapture` | PASS |
| `cargo test -p lattice-cli approval_usage -- --nocapture` | PASS |
| `cargo test -p lattice-cli plugin_prompt_running -- --nocapture` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace -- -D warnings` | PASS |
| `cargo test --workspace` | PASS |

## Decisions Made

- Kept source guards as source checks only for regression prevention; runtime behavior remains covered by scripted-model and render tests.
- Reran `code_agent_eval runtime_turn` after enabling unrestricted `cargo test` execution because the first sandboxed run skipped loopback scripted-server tests.
- Did not modify `.planning/STATE.md` or `.planning/ROADMAP.md`; the phase orchestrator is expected to handle parent-level tracking.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed rustfmt drift in the new source guard**
- **Found during:** Task 2 pre-commit formatting check
- **Issue:** `cargo fmt --all --check` reported a formatting diff in `crates/lattice-cli/tests/frontend_runtime_guards.rs`.
- **Fix:** Ran `cargo fmt --all`.
- **Files modified:** `crates/lattice-cli/tests/frontend_runtime_guards.rs`
- **Verification:** `cargo fmt --all --check` passed, and Task 1/2 focused tests were rerun.
- **Committed in:** `235cd84`

---

**Total deviations:** 1 auto-fixed (Rule 3 blocking verification issue)
**Impact on plan:** Formatting fix only; no behavior or scope change.

## Issues Encountered

- The first default-sandbox run of `cargo test -p lattice-cli --test code_agent_eval runtime_turn -- --nocapture` reported loopback bind denial and skipped the scripted-server tests. The command was rerun with approved `cargo test` execution and passed with the scripted server active.

## Known Stubs

None. Stub scan found no UI-blocking placeholder, TODO/FIXME, or hardcoded empty-data flow in files created or modified by this plan.

## Threat Flags

None. This plan added tests and documentation only; it did not introduce network endpoints, auth paths, file access APIs, or schema changes at trust boundaries.

## Self-Check: PASSED

- `crates/lattice-cli/tests/frontend_runtime_guards.rs` exists.
- Commit `6765907` exists.
- Commit `235cd84` exists.
- All final workspace gate commands passed.

## Next Phase Readiness

Phase 17 is ready for verifier review. Frontend runtime hard cut is now protected by source guards, behavior coverage, and workspace gates.

---
*Phase: 17-frontend-adapter-migration*
*Completed: 2026-05-13*
