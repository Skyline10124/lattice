---
phase: 04-workspace-layer-removal
plan: 02
subsystem: architecture
tags: [rust, cargo, profile, forbidden-guards, dependency-cleanup]

# Dependency graph
requires:
  - phase: 04-01
    provides: "Deletion of lattice-plugin, lattice-orchestrator, and lattice-plugins crates from the workspace"
provides:
  - "CLI Cargo.toml free of dead dependency declarations for deleted crates"
  - "Profile forbidden-entry guards no longer block reintroduction of orchestrator/plugin concepts under new field names"
affects: []

# Tech tracking
tech-stack:
  added: []
  patterns: []

key-files:
  created: []
  modified:
    - "crates/lattice-profile/src/spec.rs"
    - "crates/lattice-profile/src/lowering.rs"

key-decisions: []

patterns-established: []

requirements-completed: ["REM-02", "REM-03"]

# Metrics
duration: 15min
completed: 2026-05-15
---

# Phase 4 Plan 2: Profile Guard Cleanup and Cargo.toml Dead-Dependency Removal Summary

**Removed lattice_orchestrator and lattice_plugin from profile forbidden-entry guard arrays; CLI Cargo.toml dead-dependency declarations already absent from prior wave**

## Performance

- **Duration:** 15 min
- **Started:** 2026-05-15T12:25:38Z
- **Completed:** 2026-05-15T12:40:38Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- Verified CLI Cargo.toml already has zero references to `lattice-plugin`, `lattice-orchestrator`, or `lattice-plugins` (pre-existing clean state from prior execution wave)
- Removed `"lattice_orchestrator"` and `"lattice_plugin"` from the forbidden-symbol guard array in `crates/lattice-profile/src/spec.rs` (17 remaining entries)
- Removed `"lattice_orchestrator"` and `"lattice_plugin"` from the forbidden-import guard array in `crates/lattice-profile/src/lowering.rs` (8 remaining entries)
- Confirmed all 57 profile tests pass, including `spec_source_free_of_forbidden_symbols` and `lowering_source_free_of_forbidden_imports`

## Task Commits

Each task was committed atomically:

1. **Task 1: Remove dead dependency declarations from CLI Cargo.toml** — No commit (pre-existing: verified state already clean; grep returned 0 matches, cargo check -p lattice-cli passed)
2. **Task 2: Remove forbidden-field string checks from profile spec and lowering** — `882d692` (feat)

## Files Created/Modified
- `crates/lattice-profile/src/spec.rs` — Removed two entries from forbidden array (lines 107, 110 in pre-edit version); 17 of 19 entries remain
- `crates/lattice-profile/src/lowering.rs` — Removed two entries from forbidden array (lines 264, 267 in pre-edit version); 8 of 10 entries remain

## Decisions Made
None - followed plan as specified.

## Deviations from Plan

### Pre-existing State

**1. Task 1 already completed in prior execution wave**
- **Found during:** Task 1 (initial file read)
- **Issue:** The CLI Cargo.toml `[dependencies]` section already had zero references to `lattice-plugin`, `lattice-orchestrator`, or `lattice-plugins`. The three dead dependency declarations had been removed before this plan was executed.
- **Fix:** None needed. Verified via `grep -c` (returned 0) and `cargo check -p lattice-cli` (exited 0).
- **Files modified:** None (pre-existing state)
- **Verification:** `grep -c "lattice-plugin\|lattice-orchestrator\|lattice-plugins" crates/lattice-cli/Cargo.toml` returned 0; `cargo check -p lattice-cli` exited 0

---
**Total deviations:** 1 (pre-existing clean state requiring only verification)
**Impact on plan:** None — all plan verification criteria pass. The dependency declarations were removed in a prior wave (likely Phase 3 or Plan 04-01 cleanup), and this plan's verification confirmed the expected end state.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Profile guard arrays are clean: no more transitional `lattice_orchestrator` or `lattice_plugin` entries
- CLI Cargo.toml dependency section is clean: only active runtime crates remain
- Profile tests continue to enforce remaining forbidden entries (lattice_runtime, lattice_agent, code_agent, etc.)
- Ready for Plan 04-03

---
*Phase: 04-workspace-layer-removal*
*Completed: 2026-05-15*
