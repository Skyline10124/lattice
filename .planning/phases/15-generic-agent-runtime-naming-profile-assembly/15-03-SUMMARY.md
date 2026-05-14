---
phase: 15-generic-agent-runtime-naming-profile-assembly
plan: 03
subsystem: orchestrator
tags: [rust, lattice-orchestrator, capabilities, presets, coding]

requires:
  - phase: 15-generic-agent-runtime-naming-profile-assembly
    provides: ToolPreset::Coding canonical preset from Plan 15-01
  - phase: 15-generic-agent-runtime-naming-profile-assembly
    provides: Runtime coding preset migration context from Plan 15-02
provides:
  - Orchestrator assembly test fixtures use the canonical "coding" preset string
  - Assembly test names use coding terminology instead of code-agent terminology
  - Grep audit proving ToolPreset::CodeAgent is absent from crate source
affects: [phase-15-cli-coding-agent-migration, phase-16-runtime-owned-agent-lifecycle]

tech-stack:
  added: []
  patterns:
    - Canonical preset strings in orchestrator profile fixtures
    - Repo-wide grep audits recorded as task commits

key-files:
  created:
    - .planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-03-SUMMARY.md
  modified:
    - crates/lattice-orchestrator/src/assembly.rs

key-decisions:
  - "Orchestrator assembly fixtures should use the canonical \"coding\" profile preset spelling."
  - "Remaining CLI CodeAgentSpec/build_code_agent references are owned by Plan 15-04 and were not modified under this plan's ownership boundary."

patterns-established:
  - "Profile assembly tests verify coding tool exposure through CapabilitiesConfig presets vec![\"coding\"]."
  - "Audit-only tasks can be represented by an empty commit when no file change is required."

requirements-completed: [GEN-03, GEN-04]

duration: 3 min
completed: 2026-05-13
---

# Phase 15 Plan 03: Orchestrator Assembly Coding Fixture Summary

**Orchestrator assembly tests now use the canonical `coding` preset string, with a grep audit confirming `ToolPreset::CodeAgent` is absent from crate source**

## Performance

- **Duration:** 3 min
- **Started:** 2026-05-13T03:45:53Z
- **Completed:** 2026-05-13T03:48:51Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments

- Updated `make_profile()` in orchestrator assembly tests to use `presets: vec!["coding".into()]`.
- Renamed assembly tests from `code_agent` terminology to `coding` terminology.
- Confirmed `ToolPreset::CodeAgent` and `pub mod code_agent` are absent from crate source.
- Recorded remaining `CodeAgentSpec` / `build_code_agent` CLI references as Plan 15-04-owned migration work.

## Task Commits

Each task was committed atomically:

1. **Task 1: Update orchestrator assembly test fixtures and test names** - `e0fdc9d` (test)
2. **Task 2: Final repo-wide grep audit for legacy references** - `b03c5fc` (chore, empty audit commit)

## Files Created/Modified

- `crates/lattice-orchestrator/src/assembly.rs` - Uses the canonical `coding` preset string in test profile fixtures and coding terminology in assembly test names.
- `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-03-SUMMARY.md` - Records execution, verification, audit findings, and self-check.

## Decisions Made

- Renamed the additional `registry_multi_patch_cannot_reenter_code_agent_profile` test to `registry_multi_patch_cannot_reenter_coding_profile` so the plan's `rg -n 'code.agent|CodeAgent' crates/lattice-orchestrator/src/assembly.rs` check returns zero results.
- Left `lattice-cli` `CodeAgentSpec` / `build_code_agent` references untouched because they are outside this plan's ownership and explicitly covered by Plan 15-04.

## Grep Audit

- `rg -n 'ToolPreset::CodeAgent' crates/ --glob '!target/**'` - zero crate source results.
- `rg -n 'pub mod code_agent' crates/` - zero crate source results.
- `rg -n 'CodeAgentSpec' crates/ --glob '!target/**'` - remaining results only in `crates/lattice-cli/src/coding_agent.rs` and `crates/lattice-cli/tests/code_agent_eval.rs`, owned by Plan 15-04.
- `rg -n 'build_code_agent' crates/ --glob '!target/**'` - remaining results only in `crates/lattice-cli/src/coding_agent.rs` and `crates/lattice-cli/tests/code_agent_eval.rs`, owned by Plan 15-04.
- Active `.planning/` documents still contain historical and future-plan references to the old names; those are informational and expected until Plan 15-04 documentation cleanup.

## Verification

- `cargo test -p lattice-orchestrator assembly::tests::coding_profile_exposes_editing_shell_planning_and_web` - passed.
- `cargo test -p lattice-orchestrator assembly::tests` - passed, 16 tests.
- `cargo test -p lattice-orchestrator` - passed, 85 tests plus doctests.
- `cargo clippy -p lattice-orchestrator -- -D warnings` - passed.
- `cargo fmt --all --check` - passed.
- `rg -n 'code-agent|code_agent|CodeAgent' crates/lattice-orchestrator/src/assembly.rs` - zero results.

## Deviations from Plan

None - plan executed exactly as written within the requested file ownership boundary.

## Issues Encountered

- The audit found remaining `CodeAgentSpec` and `build_code_agent` references in `lattice-cli`. These are not in this plan's ownership and are explicitly described by Plan 15-04 as the next migration target.

## Known Stubs

None.

## Threat Flags

None.

## Authentication Gates

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Plan 15-04 can migrate `lattice-cli` from `CodeAgentSpec` / `Runtime::build_code_agent` to `AgentSpec::coding_preset()` / `Runtime::build_agent`. No shared tracking files were modified by this executor.

## Self-Check: PASSED

- Found modified file: `crates/lattice-orchestrator/src/assembly.rs`
- Found summary file: `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-03-SUMMARY.md`
- Found task commits: `e0fdc9d`, `b03c5fc`
- Confirmed `STATE.md` and `ROADMAP.md` were not modified.

---
*Phase: 15-generic-agent-runtime-naming-profile-assembly*
*Completed: 2026-05-13*
