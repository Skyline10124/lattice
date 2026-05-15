---
phase: 02-runtime-assembly-cutover
plan: 03
subsystem: runtime
tags: [runtime, session-turn, profile-drift, turn-status, manifest]
requires:
  - phase: 02-runtime-assembly-cutover
    provides: GenericAgentSpec runtime assembly and session transaction contracts
provides:
  - RuntimeAgentSession and AgentSessionRequest setup API
  - Runtime::submit_session_turn and SubmittedTurn completion receiver
  - Session-owned Completed/Error/Cancelled turn commits with profile snapshot metadata
affects: [cli-runtime-adapter, tui-runtime-adapter, runtime-boundary]
tech-stack:
  added: []
  patterns: [prepared runtime session, submitted turn receiver, locked profile snapshot assembly]
key-files:
  created:
    - crates/lattice-runtime/src/session.rs
  modified:
    - crates/lattice-runtime/src/lib.rs
    - crates/lattice-runtime/src/turn.rs
key-decisions:
  - "The generic runtime path opens a session turn before assembly and deserializes the locked profile snapshot for execution."
  - "SubmittedTurn owns the terminal completion receiver, avoiding latest-turn lookup semantics for generic sessions."
patterns-established:
  - "Runtime session turns are correlated by session_id plus turn_id across events and completions."
  - "Profile drift is surfaced as a warning and persisted as safe manifest metadata while keeping the locked snapshot active."
requirements-completed: [RUN-01, RUN-03]
duration: 19min
completed: 2026-05-14T17:55:39Z
---

# Phase 02 Plan 03 Summary

**Prepared generic runtime sessions with session-owned turn commits and drift warnings**

## Performance

- **Duration:** 19 min
- **Started:** 2026-05-14T17:36:19Z
- **Completed:** 2026-05-14T17:55:39Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments

- Added `AgentSessionRequest`, `RuntimeAgentSession`, `ProfileSourceFingerprint`, and `Runtime::prepare_agent_session`.
- Added `Runtime::submit_session_turn`, `SubmittedTurn`, and `Runtime::cancel_session_turn` for session/turn-owned generic execution.
- Updated runtime turn events and completions with `session_id`, while keeping `handle_id` on legacy event shapes for compatibility until adapter migration.
- Committed completed, error, and cancelled generic turns through `SessionStore::commit_turn` with profile snapshot identity and status metadata.
- Propagated session profile drift warnings into `TurnCompletion.profile_drift_warning` and manifest `profile_drift` metadata.

## Task Commits

Pending in current Wave 3 commit.

## Files Created/Modified

- `crates/lattice-runtime/src/session.rs` - Prepared runtime session request/response types and profile snapshot creation from safe source audit data.
- `crates/lattice-runtime/src/turn.rs` - Session-owned submitted turn path, completion receiver, cancellation, status commits, drift metadata, and tests.
- `crates/lattice-runtime/src/lib.rs` - Exported session setup types and added active session-turn tracking.

## Decisions Made

- `submit_session_turn` commits through `open_turn`/`commit_turn` regardless of legacy `persist_session`; the generic path is explicitly session-owned.
- The old handle-based persistence helper remains only for legacy `submit_turn` callers and was renamed to `persist_legacy_handle_turn_session` so it is not the primary design.
- Generic session execution uses a transient agent assembled from the locked snapshot returned by `open_turn`, not from a changed caller-side spec.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Preserved legacy handle compatibility while satisfying the new persistence boundary**
- **Found during:** Task 2 acceptance grep
- **Issue:** The plan required `fn persist_turn_session` to disappear. Fully deleting the legacy helper before CLI/TUI migration would break existing `submit_turn` callers outside this plan's scope.
- **Fix:** Renamed the helper to `persist_legacy_handle_turn_session` and kept it scoped to the old handle path. The new generic path uses `SessionStore::open_turn` and `SessionStore::commit_turn`.
- **Files modified:** `crates/lattice-runtime/src/turn.rs`
- **Verification:** `rg -n "fn persist_turn_session" crates/lattice-runtime/src/turn.rs` returned no matches; runtime target tests and check passed.
- **Committed in:** pending Wave 3 commit

**Total deviations:** 1 auto-fixed (Rule 3)
**Impact on plan:** No behavioral scope expansion; this keeps old adapters compiling until Waves 4-6 remove the handle-owned path.

## Issues Encountered

- Parallel Cargo verification commands waited on the shared artifact lock. All target commands completed successfully after the lock cleared.

## Verification

- `rg -n "pub struct RuntimeAgentSession|pub struct AgentSessionRequest|pub struct SubmittedTurn" crates/lattice-runtime/src` - passed
- `rg -n "session_id:.*String|turn_id: TurnId" crates/lattice-runtime/src/turn.rs` - passed
- `rg -n "open_turn|commit_turn|TurnOutcomeStatus" crates/lattice-runtime/src/turn.rs crates/lattice-runtime/src/session.rs` - passed
- `rg -n "profile_drift_warning|profile changed on disk; current session remains locked to snapshot|profile_drift" crates/lattice-runtime/src/turn.rs crates/lattice-runtime/src/session.rs` - passed
- `rg -n "fn persist_turn_session" crates/lattice-runtime/src/turn.rs` - passed with no matches
- `cargo test -p lattice-runtime runtime_turn_event -- --nocapture` - passed
- `cargo test -p lattice-runtime turn_completion -- --nocapture` - passed
- `cargo test -p lattice-runtime profile_drift -- --nocapture` - passed
- `cargo test -p lattice-session commit_turn -- --nocapture` - passed
- `cargo check -p lattice-runtime` - passed
- `cargo fmt --all --check` - passed

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Wave 4 can convert CLI adapters to `prepare_profile_runtime_session` plus `Runtime::submit_session_turn`, then consume `SubmittedTurn.events` and `SubmittedTurn.completion` without querying `latest_turn_completion`.

---
*Phase: 02-runtime-assembly-cutover*
*Completed: 2026-05-14T17:55:39Z*
