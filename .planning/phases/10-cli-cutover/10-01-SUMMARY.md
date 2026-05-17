---
phase: 10-cli-cutover
plan: 01
subsystem: cli
tags: [clap, parser, session-api, cli-surface]

requires:
  - phase: 09-agent-loop-tools-and-api-facade
    provides: SessionApi facade with status/list/log/checkout/branch/diff/reset
provides:
  - Phase 10 CLI parser boundary: only run, models, resolve, session, doctor
  - Singular session command backed by SessionApi
  - Bare invocation prints help and exits 0
affects: [10-02, 10-03, 10-04, 10-05]

tech-stack:
  added: []
  patterns: [clap-surface-boundary, session-api-cli-wiring]

key-files:
  created:
    - crates/lattice-cli/src/commands/session.rs
  modified:
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/commands/mod.rs

key-decisions:
  - "Singular session command replaces plural sessions (D-07)"
  - "Session dispatch wired directly to SessionApi::sqlite (D-18)"
  - "Run dispatch stubbed for Plan 02 wiring (D-09)"
  - "Old internal modules kept as dead code; Phase 11 owns deletion"

patterns-established:
  - "Clap surface boundary: test allowed/forbidden parsing before implementation"
  - "Session CLI wired to SessionApi facade, not legacy session manager"

requirements-completed: [CLI-01, CLI-04]

duration: 39min
completed: 2026-05-17
---

# Phase 10 Plan 01: CLI Surface Cutover Summary

**Clap parser cut to 6 commands (run/models/resolve/session/doctor/help) with 29 surface boundary tests and session wired to SessionApi**

## Performance

- **Duration:** 39 min
- **Started:** 2026-05-17T09:05:03Z
- **Completed:** 2026-05-17T09:44:03Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments

- CLI parser accepts exactly Phase 10 commands; old commands (tui, debug, config, stats, validate, new, list, run-v11, sessions) are hard-deleted from clap surface
- Bare `lattice` invocation prints help and exits 0 instead of launching TUI
- Singular `session` command wired to `SessionApi::sqlite` for status/list/log/checkout/branch/diff/reset
- 29 cli_surface tests cover positive parsing, negative command/global rejection, and help text boundaries
- Global flags reduced to `--json` and `--verbose` only (D-05)

## Task Commits

1. **Task 10-01-01: Add CLI surface parser tests** - `7d47734` (test) - RED phase: 29 tests written, 19 expected failures
2. **Task 10-01-02: Replace clap surface** - `24b8291` (feat) - GREEN phase: parser rewritten, session wired to SessionApi
3. **Formatting fix** - `9c9d50d` (style) - cargo fmt on modified files

## Files Created/Modified

- `crates/lattice-cli/src/commands/session.rs` - New singular session command with SessionAction enum (status/list/log/checkout/branch/diff/reset) wired to SessionApi
- `crates/lattice-cli/src/main.rs` - Cli struct reduced to --json/--verbose; Commands enum: Run/Models/Resolve/Session/Doctor; bare invocation prints help; 29 cli_surface tests
- `crates/lattice-cli/src/commands/mod.rs` - Public exports: doctor, models, resolve, run, session (old modules retained as dead code)

## Decisions Made

- Run dispatch stubbed with "not yet wired" message; Plan 02 handles full run_turn wiring (D-09)
- Old internal modules (config, credentials, display, frontend, security, etc.) kept as dead code to avoid breaking lower-crate dependencies; Phase 11 owns deletion
- Session uses `default_session_dir()` returning `~/.lattice/sessions` consistent with run_v11.rs pattern
- Help text test uses `render_help()` string matching for top-level subcommand entries to avoid false positives from description text

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed test mutability for render_help**
- **Found during:** Task 10-01-02
- **Issue:** `Command::render_help()` requires `&mut self` in clap 4
- **Fix:** Changed `let cmd` to `let mut cmd` in help tests
- **Files modified:** crates/lattice-cli/src/main.rs
- **Committed in:** 24b8291

**2. [Rule 1 - Bug] Fixed session import name collision**
- **Found during:** Task 10-01-02
- **Issue:** `use commands::session` conflicted with `mod session` (internal session module)
- **Fix:** Renamed import to `session as session_cmd`
- **Files modified:** crates/lattice-cli/src/main.rs
- **Committed in:** 24b8291

**3. [Rule 1 - Bug] Fixed help test false positive for "list"**
- **Found during:** Task 10-01-02
- **Issue:** `format!("{:?}", cmd)` included subcommand descriptions containing "list"
- **Fix:** Changed to `render_help()` and check for `\n  list ` pattern (top-level entry only)
- **Files modified:** crates/lattice-cli/src/main.rs
- **Committed in:** 24b8291

---

**Total deviations:** 3 auto-fixed (3 bugs)
**Impact on plan:** All fixes were necessary for test correctness. No scope creep.

## Issues Encountered

- Pre-existing formatting issues in lattice-agent and lattice-session triggered `cargo fmt --check` failures; unrelated to plan changes, left for Phase 11

## User Setup Required

None - no external service configuration required.

## Known Stubs

- `Commands::Run` dispatch returns `anyhow::bail!("run command not yet wired to lattice-api")` - Plan 02 will wire to `lattice_api::run_turn`

## Next Phase Readiness

- CLI parser boundary is locked; Plan 02 can safely wire `run` to `lattice-api::run_turn`
- Plan 03 can extend `session` command behavior
- Plan 04 can narrow `models`/`resolve`/`doctor` to v1.1 alias router

## Self-Check: PASSED

- All created files exist (session.rs, main.rs, mod.rs, SUMMARY.md)
- All task commits exist (7d47734, 24b8291, 9c9d50d)
- 29/29 cli_surface tests pass
- No forbidden surfaces in main.rs or session.rs (boundary grep clean)

---
*Phase: 10-cli-cutover*
*Completed: 2026-05-17*
