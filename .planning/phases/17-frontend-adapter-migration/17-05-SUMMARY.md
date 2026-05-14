---
phase: 17-frontend-adapter-migration
plan: 05
subsystem: ui
tags: [rust, tui, runtime-handles, plugin-overlay, approvals, security]
requires:
  - phase: 17-01
    provides: RuntimePluginOverlay, runtime handle approval API, and handle-scoped RuntimeSecurity
  - phase: 17-03
    provides: TUI Runtime/AgentHandleId ownership and RuntimeTurnEvent adapter
  - phase: 17-04
    provides: queued prompt and cancellation UX on current TUI runtime path
provides:
  - TUI plugin mode routes through runtime-owned plugin overlays and plugin-specific handles
  - Slash approve renders results from runtime handle-owned approval state
  - Plugin statusline labels and permission-switch security reset behavior are covered by regression tests
affects: [frontend-adapter-migration, tui, runtime, plugin-mode, approval-flow]
tech-stack:
  added: []
  patterns:
    - Runtime-owned plugin overlay application inside handle state
    - TUI statusline plugin label sourced from runtime handle snapshots
    - Approval result rendering split from runtime handle mutation
key-files:
  created:
    - .planning/phases/17-frontend-adapter-migration/17-05-SUMMARY.md
  modified:
    - crates/lattice-cli/src/frontend/tui/plugin_mode.rs
    - crates/lattice-cli/src/frontend/tui/app.rs
    - crates/lattice-cli/src/frontend/tui/view_model.rs
    - crates/lattice-cli/src/frontend/tui/ui.rs
    - crates/lattice-runtime/src/handle.rs
    - crates/lattice-runtime/src/lib.rs
key-decisions:
  - "Plugin submissions use plugin-specific runtime handles plus RuntimePluginOverlay; plugin_mode.rs no longer mutates live Agent state."
  - "AgentHandleSnapshot exposes plugin overlay name and approved-rule count so frontends can render and test runtime-owned state without accessing Agent."
  - "Permission switches clear plugin overlays before dropping local handle references and build a fresh RuntimeSecurity context."
patterns-established:
  - "Runtime::set_agent_plugin_overlay applies prompt, output contract, tool definitions, and authorized tool executor policy inside runtime-owned handle state."
  - "TUI /approve maps RuntimeError::HandleBusy to the UI-SPEC busy copy and does not replay blocked operations on success."
requirements-completed: [FE-04]
duration: 27min
completed: 2026-05-13
---

# Phase 17 Plan 05: Plugin Mode And Approval Runtime Migration Summary

**TUI plugin mode and slash approval now route through runtime-owned handle overlays, with statusline labels and security resets locked by tests**

## Performance

- **Duration:** 27 min
- **Started:** 2026-05-13T15:31:15Z
- **Completed:** 2026-05-13T15:57:57Z
- **Tasks:** 3
- **Files modified:** 6 code files plus this summary

## Accomplishments

- Removed the legacy live-Agent plugin mutation helper from `plugin_mode.rs` and replaced it with `RuntimePluginOverlay` preparation.
- Applied plugin overlays inside runtime handle state, including plugin system prompt, output contract, plugin tools, authorized tool names, and sandboxed tool executor policy.
- Routed `/approve last` and `/approve <id>` through the runtime handle approval API, with UI-SPEC copy for missing selector, busy handles, and success guidance.
- Added regression coverage for plugin statusline labels, plugin prompt transcript prefix, shared RuntimeSecurity defaults, and permission-switch plugin context reset.

## Task Commits

1. **Task 1: Replace plugin live-agent mutation with runtime overlay/handle routing**
   - `5d4eedf` test: add runtime plugin overlay expectations
   - `5df54cf` feat: route plugin mode through runtime overlay
2. **Task 2: Route approval command through runtime handle API**
   - `dd0d4ee` test: add slash approve runtime expectations
   - `f1ea33e` feat: render runtime approval results
3. **Task 3: Preserve plugin labels and security-context reset behavior**
   - `8353336` test: lock plugin labels and permission reset

## Files Created/Modified

- `crates/lattice-cli/src/frontend/tui/plugin_mode.rs` - Converts plugin context into `RuntimePluginOverlay`; removes live `Agent` setters/imports.
- `crates/lattice-cli/src/frontend/tui/app.rs` - Applies/clears runtime plugin overlays, renders runtime approval results, clears plugin context on permission changes, and adds behavior tests.
- `crates/lattice-cli/src/frontend/tui/view_model.rs` - Keeps plugin label behavior sourced from runtime handle snapshot state and adds statusline tests.
- `crates/lattice-cli/src/frontend/tui/ui.rs` - Adds render coverage for `@{PluginName}` transcript rows.
- `crates/lattice-runtime/src/handle.rs` - Extends handle snapshots with plugin overlay and approved-rule metadata.
- `crates/lattice-runtime/src/lib.rs` - Applies plugin overlays to runtime-owned agents and exposes snapshot metadata.

## Decisions Made

- Kept plugin-specific handle identity via `profile_name = plugin:{name}` and used `RuntimePluginOverlay` for the mutable plugin prompt/tool context.
- Exposed only read-only snapshot metadata to the TUI; live `Agent` and mutable task state remain owned by runtime.
- Treated task 3 as regression locking because the runtime state-source implementation landed with task 1.

## Deviations from Plan

None - plan executed within the planned plugin, approval, label, and security-reset scope.

## Issues Encountered

- Two verification commands were accidentally started in parallel during execution, causing Cargo artifact lock waiting. They completed successfully, and final verification was rerun serially.
- A parallel `git add` attempt briefly hit `.git/index.lock`; the lock was not left behind, and staging was redone serially before committing.

## Verification

| Command | Result |
|---------|--------|
| `cargo test -p lattice-cli plugin_mode -- --nocapture` | PASS |
| `cargo test -p lattice-cli runtime_plugin -- --nocapture` | PASS |
| `cargo test -p lattice-cli slash_approve -- --nocapture` | PASS |
| `cargo test -p lattice-cli plugin_statusline -- --nocapture` | PASS |
| `cargo test -p lattice-cli permissions_switch -- --nocapture` | PASS |
| `rg -n 'set_system_prompt|set_output_contract_delta|set_tools|set_tool_executor|Arc<Mutex<Agent>>|use lattice_agent::Agent' crates/lattice-cli/src/frontend/tui/plugin_mode.rs` | PASS - zero matches |
| `rg -n 'task_state_mut\(|try_lock\(' crates/lattice-cli/src/frontend/tui/app.rs` | PASS - zero matches |
| `rg -n 'Usage: /approve <id\|last>' crates/lattice-cli/src/frontend/tui/commands.rs` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace -- -D warnings` | PASS |
| `cargo test --workspace` | PASS |

## Known Stubs

None. Stub scan found only format-string placeholders and an existing test comment containing the word "placeholder"; no UI-facing placeholder or unwired data path was introduced.

## Threat Flags

None. The new plugin overlay and approval surfaces match the plan threat model: overlays are handle-scoped, authorized tool names are explicit, normal handles do not inherit plugin overlay state, and permission changes create a new authorization context.

## Auth Gates

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Plan 17-06 can rely on TUI plugin and approval flows using runtime-owned handle APIs. Remaining frontend migration work should treat `AgentHandleSnapshot` and `TurnCompletion` as the UI-facing state surfaces rather than reaching into live agent internals.

## TDD Gate Compliance

- Task 1 RED/GREEN commits present: `5d4eedf` -> `5df54cf`
- Task 2 RED/GREEN commits present: `dd0d4ee` -> `f1ea33e`
- Task 3 regression-lock commit present: `8353336`; behavior implementation was already introduced by task 1's runtime overlay migration.

## Self-Check: PASSED

- Found `.planning/phases/17-frontend-adapter-migration/17-05-SUMMARY.md`
- Found `crates/lattice-cli/src/frontend/tui/plugin_mode.rs`
- Found `crates/lattice-cli/src/frontend/tui/app.rs`
- Found `crates/lattice-runtime/src/handle.rs`
- Found `crates/lattice-runtime/src/lib.rs`
- Found commits `5d4eedf`, `5df54cf`, `dd0d4ee`, `f1ea33e`, and `8353336`
- Confirmed `.planning/STATE.md` and `.planning/ROADMAP.md` were not modified by this executor

---
*Phase: 17-frontend-adapter-migration*
*Completed: 2026-05-13*
