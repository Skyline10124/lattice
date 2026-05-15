---
phase: 02-runtime-assembly-cutover
plan: 06
subsystem: runtime
tags: [runtime-boundary, single-agent, session-turn, dependency-cutover]
requires:
  - phase: 02-runtime-assembly-cutover
    provides: RuntimeAgentSession, submit_session_turn, and CLI/TUI session-owned adapters
provides:
  - Runtime public API without pipeline, micro-agent, event bus, plugin registry/loading, plugin overlay, live handle, or orchestrator executor surfaces
  - Runtime manifest without direct lattice-orchestrator or lattice-plugin dependencies
  - Runtime boundary test rejecting removed extension concepts in active runtime source
affects: [phase-3-cli-tui-ux, phase-4-workspace-layer-removal, runtime-public-api]
tech-stack:
  added: []
  patterns: [runtime-owned session turn execution, source-level boundary guard, explicit unavailable legacy CLI stubs]
key-files:
  created:
    - crates/lattice-runtime/tests/runtime_boundary.rs
    - .planning/phases/02-runtime-assembly-cutover/02-06-SUMMARY.md
  modified:
    - crates/lattice-runtime/Cargo.toml
    - crates/lattice-runtime/src/lib.rs
    - crates/lattice-runtime/src/turn.rs
    - crates/lattice-runtime/src/preset.rs
    - crates/lattice-runtime/src/assembly.rs
    - crates/lattice-runtime/src/handle.rs
    - crates/lattice-runtime/src/turn_executor.rs
    - crates/lattice-cli/src/runtime.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/commands/validate.rs
    - crates/lattice-cli/src/frontend/tui/mod.rs
    - crates/lattice-cli/src/frontend/tui/app.rs
    - crates/lattice-cli/src/frontend/tui/agent_runner.rs
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/tests/code_agent_eval.rs
    - Cargo.lock
key-decisions:
  - "Runtime no longer owns compatibility APIs for pipeline, plugin, orchestrator, or live-handle execution."
  - "Runtime turn execution is session-only and commits completed, error, and cancelled outcomes through SessionStore::commit_turn."
  - "Phase 2 preserves workspace extension crates for Phase 4 while cutting runtime dependency edges now."
patterns-established:
  - "Runtime boundary tests strip Rust comments before checking removed architecture strings in active source."
  - "Legacy CLI/TUI extension entry points that still compile in Phase 2 report explicit unavailability instead of reintroducing runtime facade shims."
requirements-completed: [RUN-02, RUN-04]
duration: "not tracked across handoff"
completed: 2026-05-15T04:09:42Z
---

# Phase 02 Plan 06 Summary

**Runtime cut over to a single-agent session boundary with extension facade APIs and direct plugin/orchestrator dependencies removed**

## Performance

- **Duration:** Not tracked across interrupted handoff
- **Completed:** 2026-05-15T04:09:42Z
- **Tasks:** 3
- **Files modified:** 17 code/test files plus GSD metadata

## Accomplishments

- Removed runtime public facade APIs for pipeline, micro-agent, event bus, plugin registry/loading, plugin overlay, runtime-backed orchestrator executors, and live agent handles.
- Replaced runtime turn execution with the session-owned `submit_session_turn` path and status-bearing `TurnCompletion` values persisted through `SessionStore::commit_turn`.
- Removed direct `lattice-orchestrator` and `lattice-plugin` dependencies from `lattice-runtime` and the lockfile entry.
- Added `crates/lattice-runtime/tests/runtime_boundary.rs` to guard active runtime source and manifest against removed extension concepts.
- Patched remaining CLI/TUI compile fallout by making old pipeline/plugin-dir runtime entry points explicitly unavailable for Phase 2.

## Task Commits

1. **Task 1: Add runtime boundary guard for removed extension concepts** - `c32020d` (feat)
2. **Task 2: Delete runtime extension APIs and direct dependencies** - `c32020d` (feat)
3. **Task 3: Run Phase 2 closure gates** - verification completed before this summary

**Plan metadata:** committed separately with this summary.

## Files Created/Modified

- `crates/lattice-runtime/tests/runtime_boundary.rs` - Comment-stripping active source guard for removed runtime extension concepts and required session-turn concepts.
- `crates/lattice-runtime/Cargo.toml`, `Cargo.lock` - Removed direct runtime dependency edges to `lattice-orchestrator` and `lattice-plugin`; `blob-store` now forwards only to `lattice-agent`.
- `crates/lattice-runtime/src/lib.rs` - Reduced runtime public API to model routing, runtime security, invocation context, session preparation, session turns, prompt helpers, and session persistence.
- `crates/lattice-runtime/src/turn.rs` - Replaced live-handle turn execution with session/turn keyed submission, cancellation, stream events, and completion persistence.
- `crates/lattice-runtime/src/handle.rs`, `crates/lattice-runtime/src/turn_executor.rs` - Deleted obsolete live-handle and runtime-backed executor modules.
- `crates/lattice-runtime/src/preset.rs`, `crates/lattice-runtime/src/assembly.rs` - Removed old extension/preset test surfaces while keeping generic assembly behavior.
- `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/commands/validate.rs` - Removed runtime pipeline construction and validation calls; old pipeline execution now fails with an explicit Phase 2 cutover message.
- `crates/lattice-cli/src/frontend/tui/mod.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`, `crates/lattice-cli/src/frontend/tui/agent_runner.rs`, `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/tests/code_agent_eval.rs` - Adjusted remaining CLI/TUI callers and fixtures after runtime handle/status API removal.

## Decisions Made

- Runtime is no longer a compatibility shell for extension layers. The extension crates still exist for Phase 4 removal, but runtime cannot expose or depend on their APIs.
- Plugin directory loading from TUI is reported as unavailable during the single-agent cutover rather than routed through runtime plugin builder methods.
- Runtime boundary verification is source-level and manifest-level. Historical planning docs are intentionally out of scope for the guard.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] CLI/TUI callers still referenced removed runtime facade APIs**
- **Found during:** Task 2 (Delete runtime extension APIs and direct dependencies)
- **Issue:** Removing runtime extension APIs broke old CLI pipeline validation/execution and TUI plugin directory loading call sites.
- **Fix:** Replaced runtime-backed pipeline execution with an explicit unavailable error, removed runtime dry-run pipeline validation, built the TUI plugin registry locally, and reported plugin directory loading as unavailable for the cutover.
- **Files modified:** `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/commands/validate.rs`, `crates/lattice-cli/src/frontend/tui/mod.rs`
- **Verification:** `cargo check -p lattice-cli --bin lattice`, `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture`, `cargo check --workspace`, and `cargo check --workspace --all-targets` passed.
- **Committed in:** `c32020d`

---

**Total deviations:** 1 auto-fixed blocking compile fallout.
**Impact on plan:** Required to complete the runtime API cutover without reintroducing compatibility shims.

## Issues Encountered

- `cargo fmt --all --check` initially reported formatting-only diffs in touched files. `cargo fmt --all` corrected them, and the check passed on rerun.
- Parallel targeted Cargo tests briefly waited on package cache locks. All commands completed successfully.
- `cargo check --workspace --all-targets` surfaced the remaining `code_agent_eval` test file still importing removed `AgentSpec` / `BuiltAgent` APIs. The test file was rewritten to the runtime-backed generic path and the obsolete gap-analysis assertion was removed.
- Optional full `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` were not run for this plan. The required targeted tests, runtime boundary guard, workspace check, rustfmt, and diff whitespace gates passed.

## Verification

- `cargo test -p lattice-runtime --test runtime_boundary -- --nocapture` - passed.
- `cargo test -p lattice-runtime generic_agent_assembly -- --nocapture` - passed via the runtime test suite after the cutover.
- `cargo test -p lattice-runtime turn_completion -- --nocapture` - passed via the runtime test suite after the cutover.
- `cargo check -p lattice-runtime` - passed before closure; runtime also passed as part of workspace check.
- `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` - passed.
- `cargo test -p lattice-session` - passed.
- `cargo test -p lattice-profile` - passed.
- `cargo test -p lattice-runtime` - passed.
- `cargo check --workspace` - passed.
- `cargo check --workspace --all-targets` - passed after rewriting the stale code-agent eval harness.
- `cargo test -p lattice-cli --test code_agent_eval -- --nocapture` - passed.
- `cargo fmt --all --check` - passed after formatting.
- `git diff --check` - passed.
- `rg -n "lattice_orchestrator|lattice_plugin|Pipeline|MicroAgent|EventBus|PluginRegistry|RuntimePluginOverlay|RuntimeAgentTurnExecutor|load_plugin_dir|watch_plugin_dir|AgentHandleId|get_or_create_agent_handle|latest_turn_completion" crates/lattice-runtime/src crates/lattice-runtime/Cargo.toml` - passed with no matches.

## User Setup Required

None.

## Next Phase Readiness

Phase 2 is complete. Runtime is now cleanly centered on generic session-owned agent execution, so Phase 3 can remove or reshape the remaining CLI/TUI user-facing plugin, pipeline, and code-agent UX surfaces without fighting runtime facade dependencies.

---
*Phase: 02-runtime-assembly-cutover*
*Completed: 2026-05-15T04:09:42Z*
