---
phase: 17-frontend-adapter-migration
plan: 02
subsystem: cli
tags: [rust, cli, runtime-turns, tdd, session-persistence]
requires:
  - phase: 17-frontend-adapter-migration
    provides: Plan 17-01 runtime submit_turn, TurnCompletion, cancellation, handle approval, and plugin overlay APIs
provides:
  - CLI code command hard cut to Runtime::submit_turn and TurnCompletion rendering
  - Generic run command hard cut to minimal runtime handles and runtime-owned persistence
  - Debug chat phase hard cut to RuntimeTurnEvent collection and TurnCompletion summaries
  - Behavior tests for runtime-turn output and persistence paths
affects: [cli-runtime-adapter, tui-runtime-adapter, frontend-adapter-migration]
tech-stack:
  added: []
  patterns:
    - Frontend commands submit TurnRequest to runtime handles and render from TurnCompletion
    - Test support builds scripted runtime handles without live providers
key-files:
  created:
    - crates/lattice-cli/src/lib.rs
    - crates/lattice-cli/src/test_support.rs
    - .planning/phases/17-frontend-adapter-migration/deferred-items.md
    - .planning/phases/17-frontend-adapter-migration/17-02-SUMMARY.md
  modified:
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/commands/debug.rs
    - crates/lattice-cli/src/commands/mod.rs
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/session.rs
    - crates/lattice-cli/src/frontend/tui/agent_runner.rs
    - crates/lattice-cli/tests/code_agent_eval.rs
key-decisions:
  - CLI code, run, and debug now treat TurnCompletion as the authoritative output and persistence source.
  - BuiltCodingAgent no longer exposes live Agent; TUI legacy construction is temporarily localized in agent_runner until the TUI migration plan.
  - Scripted runtime-turn behavior tests use a small lattice-cli library/test-support target instead of source-string assertions.
patterns-established:
  - Runtime handle + TurnRequest + RuntimeTurnEvent drain + latest_turn_completion is the CLI adapter pattern.
  - CLI JSON/text rendering is derived from TurnCompletion rather than LoopEvent or live Agent state.
requirements-completed: [FE-03, FE-04]
duration: 26min
completed: 2026-05-13
---

# Phase 17 Plan 02: CLI Runtime Adapter Hard Cut Summary

**CLI code, generic run, and debug chat now execute through runtime-owned handles and render authoritative TurnCompletion output**

## Performance

- **Duration:** 26 min
- **Started:** 2026-05-13T14:01:43Z
- **Completed:** 2026-05-13T14:27:59Z
- **Tasks:** 3
- **Files modified:** 10 code/test files plus 2 planning docs

## Accomplishments

- Converted `lattice code` to build a runtime handle, submit `TurnRequest`, render streaming `RuntimeTurnEvent`, and render text/JSON/session messages from `TurnCompletion`.
- Converted generic non-pipeline `run` to minimal runtime handles with runtime-owned session persistence; pipeline execution remains unchanged.
- Converted `debug` chat execution to runtime events/completion while preserving resolve-only output.
- Added behavior tests for code/run/debug runtime-turn paths and persistence/final-report ownership.

## Task Commits

1. **Task 1 RED: Coding runtime-turn evals** - `94f8138` (`test`)
2. **Task 1 GREEN: lattice code runtime adapter** - `b9df81d` (`feat`)
3. **Task 2 RED: run runtime-turn tests** - `3c70c40` (`test`)
4. **Task 2 GREEN: generic run runtime adapter** - `39e896d` (`feat`)
5. **Task 3 RED: debug runtime-turn test** - `157d699` (`test`)
6. **Task 3 GREEN: debug runtime adapter** - `1db6cff` (`feat`)
7. **Adapter clippy cleanup** - `c4c026d` (`style`)

## Files Created/Modified

- `crates/lattice-cli/src/coding_agent.rs` - Removed `BuiltCodingAgent.agent`; code command now submits runtime turns and renders from `TurnCompletion`.
- `crates/lattice-cli/src/commands/run.rs` - Replaced non-pipeline live-agent execution with minimal runtime handles, runtime event rendering, and completion-derived output.
- `crates/lattice-cli/src/commands/debug.rs` - Replaced debug chat `agent.run` with runtime handle execution and completion-derived summaries.
- `crates/lattice-cli/src/test_support.rs` - Added scripted runtime-turn helpers for code/run/debug behavior tests.
- `crates/lattice-cli/src/lib.rs` and `crates/lattice-cli/src/main.rs` - Exposed test support for integration/unit coverage.
- `crates/lattice-cli/src/commands/mod.rs` and `crates/lattice-cli/tests/code_agent_eval.rs` - Added runtime-turn behavior tests.
- `crates/lattice-cli/src/frontend/tui/agent_runner.rs` - Localized temporary TUI legacy build path after `BuiltCodingAgent` stopped exposing `Agent`.
- `crates/lattice-cli/src/session.rs` - Marked legacy manifest conversion as retained compatibility code.

## Decisions Made

- Runtime completion is authoritative for CLI output: `assistant_text`, token usage, final report, model/provider, and saved session id are read from `TurnCompletion`.
- Generic `run` keeps the pipeline path unchanged; only the non-pipeline prompt path was migrated in this plan.
- TUI live-agent migration remains a later plan, so the compile-preserving legacy path is isolated in `frontend/tui/agent_runner.rs`.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added CLI library/test-support target for behavior tests**
- **Found during:** Task 1
- **Issue:** `lattice-cli` was binary-only, so `code_agent_eval` could not call runtime adapter seams for behavior tests.
- **Fix:** Added `src/lib.rs` plus `src/test_support.rs` with scripted runtime-turn helpers.
- **Files modified:** `crates/lattice-cli/src/lib.rs`, `crates/lattice-cli/src/test_support.rs`, `crates/lattice-cli/src/main.rs`
- **Verification:** `cargo test -p lattice-cli --test code_agent_eval runtime_turn -- --nocapture`
- **Committed in:** `b9df81d`, `3c70c40`, `39e896d`, `1db6cff`

**2. [Rule 3 - Blocking] Preserved TUI compilation after BuiltCodingAgent hard cut**
- **Found during:** Task 1
- **Issue:** Removing `BuiltCodingAgent.agent` broke `frontend/tui/agent_runner.rs`, which still depends on live `Agent` until Plan 17-03.
- **Fix:** Moved temporary legacy live-agent construction into `agent_runner.rs` instead of re-exposing `Agent` from `BuiltCodingAgent`.
- **Files modified:** `crates/lattice-cli/src/frontend/tui/agent_runner.rs`
- **Verification:** All Plan 17-02 verification commands compile and pass.
- **Committed in:** `b9df81d`

**3. [Rule 3 - Blocking] Cleaned plan-introduced clippy findings**
- **Found during:** Post-task quality check
- **Issue:** New runtime adapter/test-support code triggered clippy warnings for collapsible conditionals and loop style.
- **Fix:** Collapsed the verbose save-session condition and converted scripted read loops to `while let`.
- **Files modified:** `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/src/test_support.rs`, `crates/lattice-cli/tests/code_agent_eval.rs`
- **Verification:** Plan verification still passes; clippy now only reports out-of-scope TUI test warnings.
- **Committed in:** `c4c026d`

---

**Total deviations:** 3 auto-fixed blocking/style issues.
**Impact on plan:** All fixes supported the hard cut and behavior-test requirements without reintroducing live-Agent fallback in CLI code/run/debug.

## Issues Encountered

- The sandboxed environment denies loopback binds for scripted OpenAI-compatible tests. Existing convention is to print a skip message and return `ok`; all affected tests followed that path.
- `cargo clippy -p lattice-cli --all-targets -- -D warnings` still fails on pre-existing `frontend/tui/turn.rs` test setup warnings (`field_reassign_with_default`). This is logged in `deferred-items.md` and left for the TUI plans.

## Verification

| Command | Result |
|---------|--------|
| `cargo test -p lattice-cli --test code_agent_eval runtime_turn -- --nocapture` | PASS; 2 tests ok, scripted loopback tests skipped due bind denial |
| `cargo test -p lattice-cli commands::run_runtime -- --nocapture` | PASS; lib/bin unit tests ok, scripted loopback tests skipped due bind denial |
| `cargo test -p lattice-cli debug_runtime -- --nocapture` | PASS; lib/bin unit tests ok, scripted loopback tests skipped due bind denial |
| `cargo fmt --all --check` | PASS |
| `cargo clippy -p lattice-cli --all-targets -- -D warnings` | FAIL; out-of-scope pre-existing TUI test warnings in `frontend/tui/turn.rs` |

## Known Stubs

None. Stub scan found only format-string false positives in CLI output formatting.

## Threat Flags

None. The only new network listener is test-only scripted loopback support; production CLI paths use existing runtime model routing and session persistence.

## User Setup Required

None - no external service configuration required.

## Deferred Issues

- `crates/lattice-cli/src/frontend/tui/turn.rs` has pre-existing clippy `field_reassign_with_default` warnings in unit tests. Logged in `.planning/phases/17-frontend-adapter-migration/deferred-items.md`.

## Next Phase Readiness

CLI one-shot surfaces now follow the runtime-turn adapter pattern. Plan 17-03 can migrate TUI streaming/state with the `BuiltCodingAgent` hard cut already enforced and a temporary legacy TUI construction path clearly isolated.

## Self-Check: PASSED

- Found `crates/lattice-cli/src/coding_agent.rs`
- Found `crates/lattice-cli/src/commands/run.rs`
- Found `crates/lattice-cli/src/commands/debug.rs`
- Found `crates/lattice-cli/src/test_support.rs`
- Found `crates/lattice-cli/src/lib.rs`
- Found `.planning/phases/17-frontend-adapter-migration/17-02-SUMMARY.md`
- Found `.planning/phases/17-frontend-adapter-migration/deferred-items.md`
- Found commits `94f8138`, `b9df81d`, `3c70c40`, `39e896d`, `157d699`, `1db6cff`, and `c4c026d`
- Confirmed `.planning/STATE.md`, `.planning/ROADMAP.md`, and `.planning/REQUIREMENTS.md` were not modified

---
*Phase: 17-frontend-adapter-migration*
*Completed: 2026-05-13*
