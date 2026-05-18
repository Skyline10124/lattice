---
phase: 10-cli-cutover
plan: 05
subsystem: cli
tags: [clap, integration-tests, boundary-verification, source-audit]

requires:
  - phase: 10-cli-cutover
    provides: CLI surface cutover (10-01), run command (10-02), session command (10-03), diagnostics (10-04)
provides:
  - "23 boundary smoke integration tests verifying Phase 10 CLI cutover completeness"
  - "Source-boundary audit confirming old parser/dispatch names absent from active CLI code"
  - "Focused and workspace test suite verification"
affects: [phase-11-deletion, cli-release]

tech-stack:
  added: []
  patterns: [binary-integration-tests-using-CARGO_BIN_EXE, source-boundary-grep-audit]

key-files:
  created:
    - crates/lattice-cli/tests/cli_cutover.rs
  modified: []

key-decisions:
  - "Used env!(CARGO_BIN_EXE_lattice) for binary integration tests instead of adding assert_cmd dependency"
  - "Source-boundary grep matches in comments/imports documented as false positives rather than weakening patterns"
  - "Pre-existing clippy errors in lattice-profile and fmt issues across workspace are out of scope per deviation rules"

requirements-completed: [CLI-01, CLI-02, CLI-03, CLI-04]

duration: 5min
completed: 2026-05-17
---

# Phase 10 Plan 05: CLI Boundary Verification Summary

**23 binary-level integration tests proving help, parser, diagnostics, and failure behavior from the user perspective without live credentials**

## Performance

- **Duration:** 5 min
- **Started:** 2026-05-17T10:24:27Z
- **Completed:** 2026-05-17T10:29:40Z
- **Tasks:** 2
- **Files modified:** 1

## Accomplishments
- 23 integration tests verify the complete Phase 10 CLI surface: help text, old command rejection, secret-safe diagnostics, and failure behavior
- Source-boundary audit confirms old parser/dispatch names (`RunV11`, `Debug`, `Validate`, `Tui`, `Stats`, `Sessions`, `Stash`, `Merge`, `Rebase`, `CherryPick`, `Reflog`, `Manifests`, `Artifacts`, `run_v11::run`, `frontend::tui::run`) are absent from active CLI dispatch code
- All focused suites pass: lattice-cli (593), lattice-api (42), lattice-session (91), lattice-llm (128)
- Workspace-wide tests pass: 1866 passed, 3 ignored

## Task Commits

Each task was committed atomically:

1. **Task 10-05-01: Add boundary smoke integration tests** - `67510fc` (test)
2. **Task 10-05-02: Run source-boundary and focused suite verification** - (verification-only, no code changes)

## Files Created/Modified
- `crates/lattice-cli/tests/cli_cutover.rs` - 23 binary-level integration tests covering help text boundary, old command/global rejection, secret-safe resolve/models output, session failure behavior, and run input validation

## Verification Results

### Test Suites
| Suite | Result |
|-------|--------|
| lattice-cli | 593 passed |
| lattice-api | 42 passed |
| lattice-session | 91 passed |
| lattice-llm | 128 passed |
| workspace | 1866 passed, 3 ignored |
| cli_cutover (new) | 23 passed |

### Source-Boundary Audit
| Check | Files | Result |
|-------|-------|--------|
| `RunV11\|Debug\|Validate\|Tui\|Stats\|Config\|Sessions\|...` | main.rs, session.rs | Clean (Config false positive: module import, not command variant) |
| `StubLlm\|NoopToolExecutor\|TurnCompletion\|lattice_runtime` | run.rs | Clean (all in `#[cfg(test)]` or legacy helper with deprecation comment) |
| `Catalog\|list_models\|inspect_model\|provider_override` | models.rs, resolve.rs, doctor.rs | Clean (all matches are comments about ABSENCE of old code) |
| `dry_run\|system_prompt\|max_turns\|stream_output\|--agent` | run.rs | Clean (zero matches) |
| `frontend::tui::run` | main.rs, session.rs | Clean (zero matches) |

### Pre-existing Issues (Out of Scope)
- **clippy:** 8 errors in `lattice-profile` crate (large `Err`-variant, `manual_contains`) -- not from Phase 10 changes
- **fmt:** Formatting inconsistencies across workspace -- not from Phase 10 changes

## Decisions Made
- Used `env!(CARGO_BIN_EXE_lattice)` for binary integration tests instead of adding `assert_cmd` as a dev dependency -- zero new dependencies required
- Source-boundary grep `Config` match in main.rs is a module/struct import (`use config::Config`), not the old `Commands::Config` subcommand variant -- documented as false positive
- `StubLlmClient`/`NoopToolExecutor` in run.rs are test-only code inside `#[cfg(test)] mod run_cutover` -- not production surface
- `run_completion_value` is a legacy helper with explicit deprecation comment, used only by `test_support.rs` -- not user-facing

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None - all tests and boundary checks passed on first run, confirming plans 10-01 through 10-04 implemented the CLI surface cutover correctly.

## Known Stubs

None - no stubs found in Phase 10 CLI surface code.

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| T-10-01 mitigated | cli_cutover.rs | resolve --json test verifies env var names appear but secret values never leak |
| T-10-02 mitigated | cli_cutover.rs | Old command/global rejection tests verify clap error status, not compatibility messages |
| T-10-03 mitigated | cli_cutover.rs | session log missing-id test verifies clear failure with id in error message |

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Phase 10 CLI cutover is fully verified with automated boundary tests
- Phase 11 (broad deletion) can safely remove old command modules knowing boundary tests catch regressions
- Pre-existing clippy/fmt issues in lattice-profile should be addressed before release but are not Phase 10 scope

## Self-Check: PASSED

- [x] `crates/lattice-cli/tests/cli_cutover.rs` exists
- [x] Commit `67510fc` exists in git log
- [x] SUMMARY.md created and committed

---
*Phase: 10-cli-cutover*
*Completed: 2026-05-17*
