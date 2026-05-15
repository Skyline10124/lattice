---
phase: 02-runtime-assembly-cutover
plan: 04
subsystem: cli
tags: [cli, runtime-session, profile-runtime, submitted-turn]
requires:
  - phase: 02-runtime-assembly-cutover
    provides: RuntimeAgentSession and Runtime::submit_session_turn
provides:
  - CLI profile-backed runtime session preparation helper
  - run/debug/code adapter submission through Runtime::submit_session_turn
  - GenericRuntimeTurnFixture for profile-backed CLI adapter tests
affects: [cli-runtime-adapter, tui-test-fixtures, phase-3-cli-ux]
tech-stack:
  added: []
  patterns: [profile-backed CLI session preparation, submitted turn event drain]
key-files:
  created:
    - crates/lattice-cli/src/profile_runtime.rs
  modified:
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/lib.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/commands/debug.rs
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/src/commands/mod.rs
    - crates/lattice-cli/src/test_support.rs
    - crates/lattice-cli/src/frontend/tui/agent_runner.rs
    - crates/lattice-cli/src/frontend/tui/app.rs
key-decisions:
  - "CLI profile discovery/lowering is centralized in profile_runtime.rs instead of reimplemented in command files."
  - "Current run/debug/code UX keeps model-based command inputs by generating a strict temporary agent.toml until Phase 3 owns final UX cleanup."
  - "Adapters drain SubmittedTurn.events and await SubmittedTurn.completion instead of querying latest_turn_completion."
patterns-established:
  - "ProfileSource -> resolve_agent_profile -> lower_profile_to_spec -> Runtime::prepare_agent_session -> Runtime::submit_session_turn."
  - "Credentials stay runtime-builder input and are excluded from source audit and manifest metadata."
requirements-completed: [RUN-01, RUN-03]
duration: 55min
completed: 2026-05-14T18:50:32Z
---

# Phase 02 Plan 04 Summary

**Migrated CLI runtime turns to profile-backed generic runtime sessions**

## Performance

- **Duration:** 55 min
- **Completed:** 2026-05-14T18:50:32Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments

- Added `crates/lattice-cli/src/profile_runtime.rs` to resolve `ProfileSource`, lower loaded profiles to `GenericAgentSpec`, compute safe source fingerprints, build runtime security, and prepare `RuntimeAgentSession`.
- Added `GeneratedProfileRuntimeSessionRequest` so current `run`, `debug`, and `code` model-based UX can use a strict temporary `agent.toml` without expanding Phase 2 into Phase 3 command cleanup.
- Converted `run`, `debug`, and `coding_agent` generic turn submission from `AgentTurnSpec`/`AgentHandleId`/`latest_turn_completion` to `Runtime::submit_session_turn`, event draining, and `SubmittedTurn.completion.await`.
- Added `GenericRuntimeTurnFixture` plus CLI adapter tests covering profile-backed submitted turn completion and source audit credential exclusion.
- Updated TUI test fixtures for the new runtime event/completion fields introduced by Wave 3.

## Task Commits

Pending in current Wave 4 commit.

## Files Created/Modified

- `crates/lattice-cli/src/profile_runtime.rs` - Profile source discovery/lowering, source fingerprinting, generated temporary profile support, and credential-exclusion tests.
- `crates/lattice-cli/src/commands/run.rs` - Generic runtime turn path now prepares a runtime session and awaits submitted turn completion.
- `crates/lattice-cli/src/commands/debug.rs` - Debug path now uses a runtime session and submitted turn completion while preserving stream rendering summary behavior.
- `crates/lattice-cli/src/coding_agent.rs` - Coding path now builds a `RuntimeAgentSession` and submits via the generic runtime session API.
- `crates/lattice-cli/src/test_support.rs` - Shared profile-backed runtime turn fixture and helper submission functions.
- `crates/lattice-cli/src/commands/mod.rs` - Added CLI adapter regression test for profile-backed submission.
- `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/lib.rs` - Wired `profile_runtime` module.
- `crates/lattice-cli/src/frontend/tui/agent_runner.rs`, `crates/lattice-cli/src/frontend/tui/app.rs` - Test fixture field updates for runtime `session_id` and profile drift warning fields.

## Decisions Made

- Kept active command UX stable for Phase 2 by generating strict temporary profile packages for model-driven command paths; Phase 3 can replace this with user-facing profile selection.
- Used `SqliteCasSessionStore` for generic test fixture persistence to exercise the session-owned path from Wave 3.
- Avoided `tempfile` in non-test helper code because `test_support` is exported under `debug_assertions`; default fixture session stores use a standard-library temp path with a UUID suffix.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Removed non-test `tempfile` dependency from debug helper code**
- **Found during:** `cargo test -p lattice-cli generic_cli_runtime_turn -- --nocapture`
- **Issue:** `test_support.rs` is compiled under `debug_assertions`, so a non-test helper could not use the dev-dependency-only `tempfile` crate.
- **Fix:** Replaced the default session store with a `std::env::temp_dir()` path using process id plus UUID and created the directory explicitly.
- **Files modified:** `crates/lattice-cli/src/test_support.rs`
- **Verification:** All Wave 4 targeted tests and `cargo check -p lattice-cli --bin lattice` passed.
- **Committed in:** pending Wave 4 commit

**Total deviations:** 1 auto-fixed (Rule 3)
**Impact on plan:** No behavioral scope expansion; fixture behavior remains isolated to test/support paths.

## Issues Encountered

- Parallel Cargo commands waited on shared package/artifact locks. All target commands completed successfully after the locks cleared.

## Verification

- `rg -n "resolve_agent_profile|lower_profile_to_spec|prepare_agent_session|source_fingerprint" crates/lattice-cli/src/profile_runtime.rs` - passed
- `rg -n "profile_runtime_source_audit_excludes_credentials" crates/lattice-cli/src/profile_runtime.rs crates/lattice-cli/src/test_support.rs` - passed
- `rg -n "GenericRuntimeTurnFixture" crates/lattice-cli/src/test_support.rs` - passed
- `rg -n "AgentTurnSpec|AgentHandleId|get_or_create_agent_handle|latest_turn_completion" crates/lattice-cli/src/profile_runtime.rs crates/lattice-cli/src/test_support.rs crates/lattice-cli/src/commands/run.rs crates/lattice-cli/src/commands/debug.rs crates/lattice-cli/src/coding_agent.rs` - passed with no matches
- `rg -n "submit_session_turn|prepare_profile_runtime_session|SubmittedTurn" crates/lattice-cli/src/commands/run.rs crates/lattice-cli/src/commands/debug.rs crates/lattice-cli/src/coding_agent.rs` - passed
- `cargo test -p lattice-cli generic_runtime_turn_fixture -- --nocapture` - passed
- `cargo test -p lattice-cli profile_runtime_source_audit_excludes_credentials -- --nocapture` - passed
- `cargo test -p lattice-cli generic_cli_runtime_turn -- --nocapture` - passed
- `cargo check -p lattice-cli --bin lattice` - passed
- `cargo fmt --all --check` - passed

## User Setup Required

None.

## Next Phase Readiness

Wave 5 can migrate TUI runtime adapter code from handle-owned turn continuity onto `RuntimeAgentSession` and `Runtime::submit_session_turn` while preserving queued-input, cancellation, and status rendering behavior.

---
*Phase: 02-runtime-assembly-cutover*
*Completed: 2026-05-14T18:50:32Z*
