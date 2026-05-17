---
phase: 11
plan: 07
subsystem: testing
tags: [release-smoke, focused-tests, cli-smoke, boundary-check, ver-02, ver-04]
dependency_graph:
  requires:
    - phase: 11
      provides: "Clean 8-crate workspace with passing tests (11-06)"
  provides:
    - "Isolated CLI smoke test harness (release_smoke.rs)"
    - "Workspace boundary test for removed crate imports in focused tests"
    - "Verified focused test coverage across all VER-02 areas"
    - "All workspace gates green: check, test, LOC, boundary"
  affects: [11-VERIFICATION, release-readiness]
tech-stack:
  added: []
  patterns: [isolated-binary-smoke, temp-home-env-isolation, session-store-seeding-via-graph-api]
key-files:
  created:
    - crates/lattice-cli/tests/release_smoke.rs
  modified:
    - crates/lattice-cli/tests/cli_cutover.rs
key-decisions:
  - "Accepted .planning/codebase/ docs drift per D-05 (planning artifacts may reference old crates)"
  - "Session smoke seeds temp store via SqliteSessionGraphStore rather than calling binary subcommands"
  - "Run smoke verifies missing-profile and missing-credential error clarity without requiring live provider"
patterns-established:
  - "Isolated CLI smoke: every Command sets HOME, XDG_CONFIG_HOME, and cwd to temp dirs"
  - "Session seeding for binary tests: use SessionGraphStore trait directly in test process"
  - "Secret-safety assertion pattern: set fake sk- secret, assert output does not contain it"
requirements-completed: [VER-02, VER-04, VER-01, SIMP-03]
metrics:
  duration: ~7min
  completed: 2026-05-17
  tasks_completed: 3
  files_modified: 2
  lines_added: 490
  final_test_count: 504
  final_loc: 19922
---

# Phase 11 Plan 07: Focused Test Restoration and CLI Smoke Summary

Isolated CLI smoke harness covering all six D-16 commands plus workspace boundary test proving no removed crate imports in focused tests

## Performance

- **Duration:** 7 min
- **Started:** 2026-05-17T15:33:53Z
- **Completed:** 2026-05-17T15:40:41Z
- **Tasks:** 3
- **Files modified:** 2

## Accomplishments

- Verified focused test coverage across all six VER-02 areas (LLM routing, profile parsing, session graph, agent loop, tools/executor, API facade) plus CLI commands
- Created isolated CLI release smoke test harness (release_smoke.rs) with 11 tests covering models --auth, resolve, session status/log, doctor, and run
- All workspace gates pass: cargo check (0 errors), 504 tests, 19,922 LOC, 6/7 boundary checks

## Task Commits

Each task was committed atomically:

1. **Task 1: Repair focused tests for final crate contracts** - `b3a533d` (test)
2. **Task 2: Add isolated CLI smoke coverage** - `418f43c` (test)
3. **Task 3: Run broad workspace tests after focused fixes** - `f167d3e` (test)

## Files Created/Modified

- `crates/lattice-cli/tests/release_smoke.rs` - Isolated CLI smoke harness covering run, models --auth, resolve, session status/log, and doctor with temp HOME/XDG/cwd isolation
- `crates/lattice-cli/tests/cli_cutover.rs` - Added workspace boundary test for removed crate imports in focused tests

## Decisions Made

- Accepted .planning/codebase/ docs drift per D-05 -- planning artifacts may reference old crate names, boundary check 6/7 PASS is acceptable
- Session smoke uses SqliteSessionGraphStore to seed temp database directly from test process rather than attempting binary-level session creation
- Run smoke verifies error clarity for missing profiles and missing credentials without requiring live LLM provider

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed session ID display truncation in smoke test assertion**
- **Found during:** Task 2 (session status specific session test)
- **Issue:** CLI truncates long IDs to 12 chars with ".." suffix, test assertion used full ID
- **Fix:** Updated assertion to check for the first 12 chars of the session ID (matching CLI display behavior)
- **Files modified:** crates/lattice-cli/tests/release_smoke.rs
- **Committed in:** 418f43c (Task 2 commit)

---

**Total deviations:** 1 auto-fixed (1 bug)
**Impact on plan:** Minor fix to test assertion. No scope creep.

## Verification Results

| Check | Result |
|-------|--------|
| `cargo check --workspace` | PASS (0 errors, 0 warnings) |
| `cargo test --workspace` | PASS (504 tests) |
| `source-loc-budget.sh --max 20000` | PASS (19,922 lines) |
| `release-boundary-check.sh` | 6/7 PASS (docs drift in .planning/ only, allowed per D-05) |
| `cargo test -p lattice-llm router` | PASS (17 tests) |
| `cargo test -p lattice-profile` | PASS (55 tests) |
| `cargo test -p lattice-session` | PASS (46 tests) |
| `cargo test -p lattice-agent agent_loop` | PASS (12 tests) |
| `cargo test -p lattice-tools` | PASS (43 tests) |
| `cargo test -p lattice-api` | PASS (33 tests) |
| `cargo test -p lattice-cli --test cli_cutover` | PASS (9 tests) |
| `cargo test -p lattice-cli --test release_smoke` | PASS (11 tests) |

## Known Stubs

None. All data paths are wired; no placeholder text or empty hardcoded values remain.

## Threat Flags

None. No new security surface introduced. Smoke tests explicitly verify T-11-20 (no secret leakage) and T-11-21 (temp HOME/XDG isolation).

## Self-Check: PASSED

- SUMMARY.md: FOUND
- Commit b3a533d (Task 1): FOUND
- Commit 418f43c (Task 2): FOUND
- Commit f167d3e (Task 3): FOUND
- release_smoke.rs: FOUND
- cli_cutover.rs: FOUND

## Next Phase Readiness

- All VER-02 focused test areas covered
- All VER-04 CLI smoke commands covered with isolation
- Workspace fully green (check, test, LOC, boundary)
- Ready for plan 11-08 (final verification report and release evidence)

---
*Phase: 11-deletion-budget-and-release-verification*
*Completed: 2026-05-17*
