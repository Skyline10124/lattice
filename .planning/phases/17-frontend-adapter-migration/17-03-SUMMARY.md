---
phase: 17-frontend-adapter-migration
plan: 03
subsystem: ui
tags: [rust, tui, runtime-handles, runtime-turn-events, turn-completion]
requires:
  - phase: 17-frontend-adapter-migration
    provides: 17-01 runtime handle, cancellation, approval, plugin overlay, and completion APIs
  - phase: 17-frontend-adapter-migration
    provides: 17-02 CLI runtime adapter path and runtime persistence test support
provides:
  - TUI App state owns Runtime and AgentHandleId values instead of live Agent instances
  - TUI stream adapter consumes RuntimeTurnEvent and maps into existing Event/TuiAction surfaces
  - TUI completion lifecycle reads TurnCompletion and no longer persists completed runtime turns manually
affects: [phase-17, tui, plugin-handle-migration, queued-input-ux, approval-runtime-migration]
tech-stack:
  added: []
  patterns:
    - Runtime-owned handle state in frontend coordinators
    - RuntimeTurnEvent-to-TuiAction adapter tests
    - TurnCompletion-driven TUI terminal lifecycle
key-files:
  created:
    - .planning/phases/17-frontend-adapter-migration/17-03-SUMMARY.md
  modified:
    - crates/lattice-cli/src/frontend/tui/app.rs
    - crates/lattice-cli/src/frontend/tui/agent_runner.rs
    - crates/lattice-cli/src/frontend/tui/action.rs
    - crates/lattice-cli/src/frontend/tui/event.rs
    - crates/lattice-cli/src/frontend/tui/update.rs
    - crates/lattice-cli/src/frontend/tui/turn.rs
    - crates/lattice-cli/src/frontend/tui/session_state.rs
    - crates/lattice-cli/src/frontend/tui/commands.rs
    - crates/lattice-cli/src/frontend/tui/interaction.rs
    - crates/lattice-cli/src/frontend/tui/mod.rs
    - crates/lattice-cli/src/frontend/tui/plugin_mode.rs
    - crates/lattice-cli/src/coding_agent.rs
key-decisions:
  - "TUI local numeric turn IDs remain for widget compatibility, while runtime handle and TurnId are tracked separately for stale-event filtering."
  - "Runtime terminal events are split from visible stream tokens so update.rs can query latest_turn_completion after the runtime stream has drained."
  - "Frontend-side save_completed_turn is removed from active App/update paths; runtime persistence is the authoritative save path."
patterns-established:
  - "Runtime stream adapter filters RuntimeTurnEvent by AgentHandleId and TurnId before emitting visible TUI events."
  - "Completion metadata updates model/provider/token state from TurnCompletion rather than live Agent state."
requirements-completed: [FE-01, FE-02, FE-04]
duration: 26m
completed: 2026-05-13
---

# Phase 17 Plan 03: TUI Runtime Adapter Migration Summary

**TUI runtime-handle streaming with RuntimeTurnEvent mapping and TurnCompletion-owned terminal lifecycle**

## Performance

- **Duration:** 26m
- **Started:** 2026-05-13T14:32:19Z
- **Completed:** 2026-05-13T14:58:37Z
- **Tasks:** 3
- **Files modified:** 12

## Accomplishments

- Replaced TUI live `Agent` / `Arc<Mutex<Agent>>` ownership with `Runtime`, normal/plugin `AgentHandleId` state, active handle tracking, and active runtime `TurnId`.
- Reworked `agent_runner` into a runtime submit-turn adapter that maps `RuntimeTurnEvent` into the existing `Event` / `TuiAction` model with stale handle/turn filtering.
- Moved active TUI terminal handling to `TurnCompletion`, removed frontend runtime-turn persistence from `app.rs` / `update.rs`, and reset runtime handles/transient state on session load.

## Task Commits

1. **Task 1: Replace TUI build/stream structs with runtime handle structs** - `bd44f9a` (feat)
2. **Task 2: Map RuntimeTurnEvent into existing TUI action model** - `8b93e48` (test)
3. **Task 3: Move TUI completion and persistence ownership to runtime** - `33b94a0` (test)
4. **Clippy follow-up** - `5f88122` (fix)

## Files Created/Modified

- `crates/lattice-cli/src/frontend/tui/app.rs` - Runtime/handle-owned TUI coordinator state, runtime completion application, session-load reset behavior, and completion tests.
- `crates/lattice-cli/src/frontend/tui/agent_runner.rs` - Runtime handle build helper, `Runtime::submit_turn` stream adapter, event filtering, and runtime event mapping tests.
- `crates/lattice-cli/src/frontend/tui/action.rs` - Runtime lifecycle actions added to the normalized TUI action model.
- `crates/lattice-cli/src/frontend/tui/event.rs` - Runtime lifecycle event variants added for background stream tasks.
- `crates/lattice-cli/src/frontend/tui/update.rs` - Runtime terminal action queries `latest_turn_completion` and no longer auto-submits queued prompts.
- `crates/lattice-cli/src/frontend/tui/turn.rs` - TUI turn tests converted away from field reassignment after default initialization.
- `crates/lattice-cli/src/frontend/tui/session_state.rs` - Legacy frontend save helper marked inactive while runtime owns persistence.
- `crates/lattice-cli/src/frontend/tui/commands.rs` - Model/provider/effort changes clear runtime handles; approve usage copy updated.
- `crates/lattice-cli/src/frontend/tui/interaction.rs` - Menu model/provider selection clears runtime handles.
- `crates/lattice-cli/src/frontend/tui/mod.rs` - TUI runtime initialized with CLI credentials.
- `crates/lattice-cli/src/frontend/tui/plugin_mode.rs` - Legacy live-agent plugin mutation helper marked inactive until plugin handle migration.
- `crates/lattice-cli/src/coding_agent.rs` - Legacy manifest/system-prompt helpers marked inactive after runtime hard cut.

## Decisions Made

- Runtime terminal completion is represented as a non-rendering lifecycle event after stream drain, avoiding a race where `Completed` is observed before runtime stores `TurnCompletion`.
- Plugin-specific handle slots are tracked in App now, but plugin overlay mutation remains deferred to the dedicated plugin migration plan.
- The queued prompt helper remains available for the follow-up queued-input UI plan, but completion/update no longer auto-submits queued prompts.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Kept intermediate migration code clippy-clean**
- **Found during:** Task 1 and final verification
- **Issue:** Removing live `Agent` usage made legacy plugin/session helper paths unused before their dedicated follow-up plans, which would fail `clippy -D warnings`.
- **Fix:** Marked those legacy helpers as inactive with narrow `#[allow(dead_code)]` annotations and kept runtime paths authoritative.
- **Files modified:** `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/src/frontend/tui/plugin_mode.rs`, `crates/lattice-cli/src/frontend/tui/session_state.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`
- **Verification:** `cargo clippy -p lattice-cli --all-targets -- -D warnings`
- **Committed in:** `bd44f9a`

**2. [Rule 1 - Bug] Fixed TUI test clippy warning carried from 17-02 deferred item**
- **Found during:** Task 1 verification readiness
- **Issue:** `crates/lattice-cli/src/frontend/tui/turn.rs` tests used field reassignment after `Default`, matching the 17-02 deferred `field_reassign_with_default` warning.
- **Fix:** Converted test setup to struct literals with `..TurnState::default()`.
- **Files modified:** `crates/lattice-cli/src/frontend/tui/turn.rs`
- **Verification:** `cargo clippy -p lattice-cli --all-targets -- -D warnings`
- **Committed in:** `bd44f9a`

**3. [Rule 1 - Bug] Fixed runtime adapter needless borrow**
- **Found during:** Final clippy verification
- **Issue:** Runtime handle reuse check borrowed an already borrowed `AgentHandleId`, triggering `clippy::needless_borrow`.
- **Fix:** Passed the handle reference directly.
- **Files modified:** `crates/lattice-cli/src/frontend/tui/agent_runner.rs`
- **Verification:** `cargo clippy -p lattice-cli --all-targets -- -D warnings`
- **Committed in:** `5f88122`

---

**Total deviations:** 3 auto-fixed (2 bug/blocking clippy issues, 1 migration-intermediate cleanup)
**Impact on plan:** All fixes were limited to TUI/runtime adapter correctness and verification readiness. No architecture checkpoint was required.

## Issues Encountered

- Some task 2/3 scaffolding landed in the first feature commit because App, Event, Action, Update, and Runner type changes needed to compile together. Later task commits added focused behavior coverage for the task-specific acceptance criteria.
- `cargo test -p lattice-cli runtime_turn_event -- --nocapture` reported loopback-bind skips for an existing debug runtime eval, but all TUI runtime event tests ran and passed.
- `cargo test -p lattice-cli runtime_persistence -- --nocapture` reported loopback-bind skips for an existing run-command persistence eval, while the TUI runtime persistence test ran and passed.

## Verification

- `cargo test -p lattice-cli frontend::tui::agent_runner -- --nocapture` - PASSED
- `cargo test -p lattice-cli runtime_turn_event -- --nocapture` - PASSED
- `cargo test -p lattice-cli tui_completion -- --nocapture` - PASSED
- `cargo test -p lattice-cli runtime_persistence -- --nocapture` - PASSED
- `cargo fmt --all --check` - PASSED
- `cargo clippy -p lattice-cli --all-targets -- -D warnings` - PASSED
- `rg -n 'Arc<Mutex<Agent>>|use lattice_agent::Agent|agent: Option' crates/lattice-cli/src/frontend/tui/app.rs crates/lattice-cli/src/frontend/tui/agent_runner.rs` - zero matches
- `rg -n 'LoopEvent::' crates/lattice-cli/src/frontend/tui/agent_runner.rs` - zero matches
- `rg -n 'save_completed_turn\(' crates/lattice-cli/src/frontend/tui/app.rs crates/lattice-cli/src/frontend/tui/update.rs` - zero matches

## Known Stubs

None. Stub-pattern scan found only format-string placeholders in CLI/TUI status copy.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- TUI queued-input UX can now build on runtime busy events without auto-submission from completion.
- Plugin/approval migration can consume the runtime handle slots and remove the inactive live-agent plugin helper.
- Runtime persistence is now the active TUI contract; future session graph work should treat `TurnCompletion` as authoritative.

## Self-Check: PASSED

- Summary file exists at `.planning/phases/17-frontend-adapter-migration/17-03-SUMMARY.md`.
- Commits `bd44f9a`, `8b93e48`, `33b94a0`, and `5f88122` exist in git history.
- Worktree was clean before writing this SUMMARY; only the SUMMARY file is pending commit.

---
*Phase: 17-frontend-adapter-migration*
*Completed: 2026-05-13*
