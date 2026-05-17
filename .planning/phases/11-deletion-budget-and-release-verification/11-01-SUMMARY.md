---
phase: 11-deletion-budget-and-release-verification
plan: 01
subsystem: release-verification
tags: [loc-budget, boundary-check, docs-drift, release-gate]

requires:
  - phase: 10-cli-cutover
    provides: D-25 closure via SessionApi-scoped session command behavior
provides:
  - Strict active Rust source LOC budget script
  - Release boundary and documentation drift check script
  - Phase 11 fixed release verification evidence report
affects: [11-deletion-budget-and-release-verification, release-gates, deletion-waves]

tech-stack:
  added: []
  patterns: [repo-root-release-script, grouped-boundary-report, fixed-evidence-report]

key-files:
  created:
    - scripts/source-loc-budget.sh
    - scripts/release-boundary-check.sh
    - .planning/phases/11-deletion-budget-and-release-verification/11-VERIFICATION.md
  modified: []

key-decisions:
  - "Boundary and LOC checks are intentionally failing before deletion waves so later plans can prove blocker removal without changing script semantics."
  - "The docs drift gate is available as --docs so release-facing documentation can be verified independently after source deletion."

patterns-established:
  - "Strict LOC budget gate: count every Rust line under crates/*/src/**/*.rs and fail above --max."
  - "Grouped release boundary reports: workspace metadata, active source, old crate dirs, unsupported CLI/TUI files, API migration-home files, lockfile packages, and docs drift."

requirements-completed: [SIMP-02, SIMP-03, CRATE-05, VER-03, VER-05, VER-06]

duration: 35min
completed: 2026-05-17
---

# Phase 11 Plan 01: Release Gate Scaffolding Summary

**Strict LOC, boundary, docs drift, and release evidence gates for the Phase 11 deletion pipeline**

## Performance

- **Duration:** 35 min
- **Started:** 2026-05-17T13:00:00Z
- **Completed:** 2026-05-17T13:35:04Z
- **Tasks:** 3
- **Files modified:** 3

## Accomplishments

- Added `scripts/source-loc-budget.sh`, a deterministic strict LOC report for every Rust source file under crate `src` directories.
- Added `scripts/release-boundary-check.sh`, a grouped boundary check for removed old crate identities, unsupported CLI/TUI files, `topology.rs`, lockfile entries, and release docs drift.
- Seeded `11-VERIFICATION.md` with the full Phase 11 release gate table and D-25 handoff closure from `10-06-SUMMARY.md`.

## Task Commits

Each task was committed atomically:

1. **Task 11-01-01: Add strict active-source LOC gate** - `f1040a6` (feat)
2. **Task 11-01-02: Add release boundary and docs drift gate** - `e26dcdc` (feat)
3. **Task 11-01-03: Seed final verification evidence report** - `fee1884` (docs)

## Files Created/Modified

- `scripts/source-loc-budget.sh` - Counts strict active Rust LOC under `crates/*/src/**/*.rs`, prints per-crate totals plus `TOTAL`, and fails when the configured max is exceeded.
- `scripts/release-boundary-check.sh` - Reports Phase 11 release boundary failures by group, with `--docs` for documentation drift only.
- `.planning/phases/11-deletion-budget-and-release-verification/11-VERIFICATION.md` - Fixed release evidence report for later gate updates.

## Decisions Made

- Kept both scripts repo-root relative and read-only so they can be reused unchanged across all deletion waves.
- Made current blocker failures explicit instead of suppressing them; before deletion, `--max 20000` and boundary checks are expected to fail.
- Recorded Phase 10 D-25 as closed by `10-06-SUMMARY.md` so Phase 11 does not inherit the stale partial verifier finding.

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

- `.planning/` is ignored by default, so `11-VERIFICATION.md` had to be added with `git add -f` as an intentional phase artifact. This did not broaden tracking to other `.planning` files.

## Verification

Commands run:

- `./scripts/source-loc-budget.sh --max 100000` - passed with `TOTAL 71955`.
- `./scripts/source-loc-budget.sh --max 20000` - failed as expected before deletion waves, with `TOTAL 71955`.
- `./scripts/release-boundary-check.sh` - failed as expected, reporting six current blocker groups.
- `./scripts/release-boundary-check.sh --docs` - failed as expected, reporting release docs drift.
- `rg -n "Full Release Gate|pending" .planning/phases/11-deletion-budget-and-release-verification/11-VERIFICATION.md` - passed.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Wave 2 can now delete and rehome code against stable gates. The expected next blockers are old workspace crate identities, unsupported CLI/TUI files, `crates/lattice-api/src/topology.rs`, lockfile entries, docs drift, and the strict LOC budget.

---
*Phase: 11-deletion-budget-and-release-verification*
*Completed: 2026-05-17*
