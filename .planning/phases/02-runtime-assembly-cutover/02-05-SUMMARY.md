---
phase: 02-runtime-assembly-cutover
plan: 05
subsystem: tui
tags: [tui, runtime-session, submitted-turn, frontend-guard]
requires:
  - phase: 02-runtime-assembly-cutover
    provides: RuntimeAgentSession and Runtime::submit_session_turn
provides:
  - TUI runtime adapter state keyed by RuntimeAgentSession and session_id
  - TUI runtime event filtering by session_id plus runtime turn id
  - Frontend guard tests rejecting handle-owned runtime adapter regressions
affects: [tui-runtime-adapter, runtime-boundary-cleanup, phase-3-plugin-ux]
tech-stack:
  added: []
  patterns: [session-owned TUI runtime adapter, submitted turn completion propagation]
key-files:
  created:
    - .planning/phases/02-runtime-assembly-cutover/02-05-SUMMARY.md
  modified:
    - crates/lattice-cli/src/frontend/tui/agent_runner.rs
    - crates/lattice-cli/src/frontend/tui/app.rs
    - crates/lattice-cli/src/frontend/tui/action.rs
    - crates/lattice-cli/src/frontend/tui/event.rs
    - crates/lattice-cli/src/frontend/tui/update.rs
    - crates/lattice-cli/src/frontend/tui/view_model.rs
    - crates/lattice-cli/src/frontend/tui/plugin_mode.rs
    - crates/lattice-cli/src/frontend/tui/ui.rs
    - crates/lattice-cli/src/frontend/tui/commands.rs
    - crates/lattice-cli/src/frontend/tui/interaction.rs
    - crates/lattice-cli/src/profile_runtime.rs
    - crates/lattice-cli/tests/frontend_runtime_guards.rs
key-decisions:
  - "TUI stores reusable RuntimeAgentSession values instead of runtime AgentHandleId values."
  - "TUI terminal events carry TurnCompletion from SubmittedTurn.completion instead of querying latest_turn_completion."
  - "Plugin-mode UI state remains local in Phase 2; runtime plugin overlay behavior is not preserved."
patterns-established:
  - "TUI submit path builds generated profile runtime sessions and submits through Runtime::submit_session_turn."
  - "Frontend guard tests strip comments and reject handle-owned runtime adapter APIs in active TUI source."
requirements-completed: [RUN-01, RUN-03]
duration: 43min
completed: 2026-05-14T19:33:38Z
---

# Phase 02 Plan 05 Summary

**TUI runtime adapter migrated from live handles to session-owned submitted turns**

## Performance

- **Duration:** 43 min
- **Completed:** 2026-05-14T19:33:38Z
- **Tasks:** 2
- **Files modified:** 15

## Accomplishments

- Replaced TUI `AgentHandleId` state with `RuntimeAgentSession`, `active_session_id`, and session-keyed plugin runtime state.
- Converted `agent_runner` to prepare generated profile runtime sessions and submit through `Runtime::submit_session_turn`.
- Runtime stream dispatch now filters events by `session_id` and `TurnId`, then sends terminal events with the awaited `TurnCompletion`.
- Removed TUI dependency on `RuntimePluginOverlay`; plugin status remains local UI state for Phase 3 cleanup.
- Updated frontend runtime guards to reject `AgentHandleId`, runtime handle creation, `latest_turn_completion`, and `RuntimePluginOverlay` in active TUI runtime files.

## Task Commits

Pending in current Wave 5 commit.

## Files Created/Modified

- `crates/lattice-cli/src/frontend/tui/agent_runner.rs` - Session-backed build/reuse path, submitted turn stream drain, and session/turn event filtering.
- `crates/lattice-cli/src/frontend/tui/app.rs` - Runtime session state, cancellation, completion, approval task-state propagation, and plugin session isolation.
- `crates/lattice-cli/src/frontend/tui/action.rs`, `crates/lattice-cli/src/frontend/tui/event.rs`, `crates/lattice-cli/src/frontend/tui/update.rs` - TUI runtime lifecycle events now carry session ids and completion payloads.
- `crates/lattice-cli/src/frontend/tui/view_model.rs`, `crates/lattice-cli/src/frontend/tui/ui.rs` - Plugin statusline tests now use runtime session fixtures instead of handle overlays.
- `crates/lattice-cli/src/frontend/tui/commands.rs`, `crates/lattice-cli/src/frontend/tui/interaction.rs` - Model/provider/thinking changes clear runtime sessions instead of handles.
- `crates/lattice-cli/src/frontend/tui/plugin_mode.rs` - Removed runtime overlay construction from plugin-mode support.
- `crates/lattice-cli/src/profile_runtime.rs` - Generated runtime profiles preserve optional reasoning effort for the TUI adapter.
- `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/commands/debug.rs` - Updated generated profile request call sites for the reasoning-effort field.
- `crates/lattice-cli/tests/frontend_runtime_guards.rs` - Guard contract updated for session-owned runtime execution.

## Decisions Made

- Reused `RuntimeAgentSession` directly in TUI state rather than introducing another local id wrapper; the runtime session already carries profile, invocation, security, and optional durable session identity.
- Dropped the old `RuntimeBusy` event path because `submit_session_turn` reports session-turn conflicts through normal runtime errors keyed by session/turn identity.
- Added `thinking_effort` to generated profile session requests so TUI `/thinking` still affects the profile-derived generic runtime path.

## Deviations from Plan

None - plan executed within the adapter migration boundary.

## Issues Encountered

- Existing plugin statusline tests still used runtime handle overlays after the core migration. They were rewritten to use `RuntimeAgentSession` fixtures and local `plugin_sessions` state.
- Parallel Cargo commands briefly waited on shared package/artifact locks. All target commands completed successfully after locks cleared.

## Verification

- `rg -n "AgentHandleId|get_or_create_agent_handle|latest_turn_completion|RuntimePluginOverlay|submit_turn\\(|normal_handle|plugin_handles|active_handle|RuntimeBusy|queue_runtime_busy_prompt" crates/lattice-cli/src/frontend/tui/agent_runner.rs crates/lattice-cli/src/frontend/tui/app.rs crates/lattice-cli/src/frontend/tui/action.rs crates/lattice-cli/src/frontend/tui/event.rs crates/lattice-cli/src/frontend/tui/update.rs crates/lattice-cli/src/frontend/tui/view_model.rs crates/lattice-cli/src/frontend/tui/plugin_mode.rs crates/lattice-cli/src/frontend/tui/ui.rs crates/lattice-cli/tests/frontend_runtime_guards.rs` - passed; matches only in guard denylist.
- `rg -n "RuntimeAgentSession|submit_session_turn|event_session_id|runtime_event_turn_id" crates/lattice-cli/src/frontend/tui/agent_runner.rs` - passed.
- `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` - passed.
- `cargo test -p lattice-cli tui_runtime_session_identity -- --nocapture` - passed.
- `cargo check -p lattice-cli --bin lattice` - passed.
- `cargo test -p lattice-cli frontend::tui::view_model::tests::plugin_statusline_plugin_prompt_running_shows_plugin_statusline -- --nocapture` - passed.
- `cargo test -p lattice-cli frontend::tui::ui::tests::plugin_statusline::plugin_prompt_running_shows_plugin_statusline -- --nocapture` - passed.
- `cargo fmt --all --check` - passed.
- `git diff --check` - passed.

## User Setup Required

None.

## Next Phase Readiness

Wave 6 can remove runtime extension facade APIs, live-handle surfaces, and direct orchestrator/plugin dependency edges now that CLI and TUI adapters use session-owned submitted turns.

---
*Phase: 02-runtime-assembly-cutover*
*Completed: 2026-05-14T19:33:38Z*
