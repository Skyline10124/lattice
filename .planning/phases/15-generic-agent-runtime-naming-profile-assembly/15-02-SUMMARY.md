---
phase: 15-generic-agent-runtime-naming-profile-assembly
plan: 02
subsystem: runtime
tags: [rust, lattice-runtime, agent-spec, presets, coding]

requires:
  - phase: 15-generic-agent-runtime-naming-profile-assembly
    provides: ToolPreset::Coding from Plan 15-01
provides:
  - AgentPreset::Coding as the typed runtime coding preset
  - AgentSpec preset, cwd, and task-state seed builder fields
  - Runtime::build_agent preset dispatch through preset::apply_coding_preset
  - coding_system_prompt exported from lattice-runtime
  - Removal of CodeAgentSpec, Runtime::build_code_agent, and code_agent.rs
affects: [phase-15-cli-coding-agent-migration, phase-16-runtime-owned-agent-lifecycle]

tech-stack:
  added: []
  patterns:
    - Typed runtime preset selection
    - Product-neutral AgentSpec assembly
    - Coding capability wiring isolated behind AgentPreset::Coding

key-files:
  created:
    - crates/lattice-runtime/src/preset.rs
    - .planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-02-SUMMARY.md
  modified:
    - crates/lattice-runtime/src/lib.rs
    - crates/lattice-runtime/src/code_agent.rs (deleted)

key-decisions:
  - "AgentPreset::Coding is the explicit opt-in runtime preset for coding agents."
  - "Runtime::build_agent is the authoritative runtime construction path for the coding preset."
  - "Non-preset AgentSpec builds must still declare explicit capabilities."

patterns-established:
  - "Preset-specific providers, tools, task-state provider, and security executor wiring live in preset.rs."
  - "Generic agents without presets do not inherit coding-only tools."

requirements-completed: [GEN-01, GEN-03]

duration: 8 min
completed: 2026-05-13
---

# Phase 15 Plan 02: Runtime Preset Assembly Summary

**Runtime-owned `AgentPreset::Coding` assembly through generic `AgentSpec` with the legacy code-agent factory removed**

## Performance

- **Duration:** 8 min
- **Started:** 2026-05-13T03:33:58Z
- **Completed:** 2026-05-13T03:41:51Z
- **Tasks:** 3
- **Files modified:** 4

## Accomplishments

- Added `crates/lattice-runtime/src/preset.rs` with `AgentPreset::Coding`, `coding_system_prompt`, canonical workspace validation, coding tool wiring, project/workspace providers, task-state provider, and migrated tests.
- Extended `AgentSpec` with `cwd`, `task_state_seed`, `preset`, and `coding_preset()` while routing `Runtime::build_agent` through `preset::apply_coding_preset`.
- Removed the old `code_agent.rs` module and eliminated `CodeAgentSpec` / `Runtime::build_code_agent` references from `lattice-runtime`.
- Added regression coverage proving non-preset generic agents do not receive `patch`, `bash`, `todo_write`, or `web_search`.

## Task Commits

Each task was committed atomically:

1. **Task 1: Create runtime coding preset module** - `931f690` (feat)
2. **Task 2: Dispatch AgentSpec through presets** - `6057fe4` (feat)
3. **Task 3: Delete legacy code agent module** - `526cbb3` (refactor)

## Files Created/Modified

- `crates/lattice-runtime/src/preset.rs` - Defines `AgentPreset::Coding`, coding preset application, coding system prompt, directory validation, and migrated preset tests.
- `crates/lattice-runtime/src/lib.rs` - Exports the preset module, extends `AgentSpec`, dispatches presets in `build_agent`, and adds runtime preset isolation tests.
- `crates/lattice-runtime/src/code_agent.rs` - Deleted after its functionality moved into `preset.rs` and `AgentSpec`.
- `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-02-SUMMARY.md` - Records this plan outcome.

## Decisions Made

- Kept coding assembly typed with `AgentPreset::Coding` instead of string-based runtime preset dispatch.
- Let `Runtime::build_agent` apply presets before generic capability validation, while keeping system prompt, message seeding, and thinking effort as common post-preset overrides.
- Returned coding tool definitions from `build_agent` for preset builds so existing callers can inspect exposed tools without depending on the deleted `CodeAgentSpec`.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Pulled minimal lib.rs preset integration into Task 1**
- **Found during:** Task 1 (Create preset.rs with migrated tests)
- **Issue:** A new Rust source file is not compiled unless it is declared from `lib.rs`, and the migrated tests depended on `AgentSpec` preset fields plus `Runtime::build_agent` dispatch. The plan listed those changes under Task 2 but required Task 1 tests to run.
- **Fix:** Added the minimum `lib.rs` preset export, `AgentSpec` fields/builders, and `build_agent` preset dispatch in the Task 1 commit so `cargo test -p lattice-runtime preset::tests` executed the four migrated tests.
- **Files modified:** `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/preset.rs`
- **Verification:** `cargo test -p lattice-runtime preset::tests`
- **Committed in:** `931f690`

---

**Total deviations:** 1 auto-fixed (1 blocking plan sequencing issue)
**Impact on plan:** The final file ownership and behavior match the plan; only the commit boundary for required `lib.rs` integration moved earlier so Task 1 verification was real.

## Issues Encountered

- The Task 1/Task 2 split was not independently compilable as written. This was handled as the documented Rule 3 deviation above.

## Verification

- `cargo test -p lattice-runtime preset::tests` - passed, 4 tests
- `cargo test -p lattice-runtime --lib` - passed, 24 tests
- `cargo test -p lattice-runtime` - passed, 24 tests plus doctests
- `cargo clippy -p lattice-runtime -- -D warnings` - passed
- `cargo fmt --all --check` - passed
- `rg -n 'CodeAgentSpec|build_code_agent' crates/lattice-runtime/` - zero results
- `rg -n 'pub mod code_agent|code_agent|CodeAgent' crates/lattice-runtime/src` - zero results

## Known Stubs

None.

## Authentication Gates

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Plan 15-03 can update orchestrator naming and fixtures against the canonical `coding` preset terminology. Plan 15-04 can migrate CLI coding entrypoints from `CodeAgentSpec` to `AgentSpec::coding_preset()`.

## Self-Check: PASSED

- Found created file: `crates/lattice-runtime/src/preset.rs`
- Found modified file: `crates/lattice-runtime/src/lib.rs`
- Confirmed deleted file: `crates/lattice-runtime/src/code_agent.rs`
- Found summary file: `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-02-SUMMARY.md`
- Found task commits: `931f690`, `6057fe4`, `526cbb3`

---
*Phase: 15-generic-agent-runtime-naming-profile-assembly*
*Completed: 2026-05-13*
