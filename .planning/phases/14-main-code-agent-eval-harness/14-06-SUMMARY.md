---
phase: 14-main-code-agent-eval-harness
plan: 06
subsystem: prompt
tags: [code-agent, project-instructions, precedence, regression-test]

requires:
  - phase: 14-main-code-agent-eval-harness
    plan: 02
    provides: initial project instruction precedence provider and runtime factory tests
provides:
  - Full root and child instruction file matrix regression coverage
  - File-tier-major project instruction render ordering
  - Runtime confirmation for nested code-agent instruction scope
affects: [phase-14, lattice-prompt, lattice-runtime]

tech-stack:
  added: []
  patterns:
    - Project instruction sources render by file tier, then parent-to-child directory order inside each tier
    - Root and nested instruction matrix cases are covered by provider tests

key-files:
  created:
    - .planning/phases/14-main-code-agent-eval-harness/14-06-SUMMARY.md
  modified:
    - crates/lattice-prompt/src/project_instructions_provider.rs

key-decisions:
  - "Project instruction render order is file-tier-major: CLAUDE.md tier, then AGENTS.md tier, then LATTICE.md tier."
  - "Within each instruction file tier, parent directories render before child directories so local rules override same-source parent rules."
  - "The runtime code-agent factory remains unchanged because it already constructs ProjectInstructionsProvider with both project_root and cwd."

patterns-established:
  - "Mixed root/child instruction precedence tests assert the complete CLAUDE, AGENTS, and LATTICE matrix."
  - "No runtime factory change is needed when provider ordering changes preserve the existing ProjectInstructionsProvider::new contract."

requirements-completed: [CODE-02, EVAL-01, EVAL-03]

duration: 7 min
completed: 2026-05-12
---

# Phase 14 Plan 06: Instruction Precedence Gap Closure Summary

**Root and child project instructions now compose by file-source priority and directory depth in one deterministic order**

## Performance

- **Duration:** 7 min
- **Started:** 2026-05-12T15:04:18Z
- **Completed:** 2026-05-12T15:10:49Z
- **Tasks:** 3
- **Files modified:** 2

## Accomplishments

- Added a regression test where root and nested cwd both contain `CLAUDE.md`, `AGENTS.md`, and `LATTICE.md`.
- Changed provider source ordering from directory-major to file-tier-major, with parent-before-child ordering inside each tier.
- Confirmed the runtime code-agent factory still uses `ProjectInstructionsProvider::new(project_root, cwd)` and preserves nested instruction visibility.

## Task Commits

Each task was committed atomically:

1. **Task 1: Add mixed root/child instruction precedence regression coverage** - `1cf782a` (test)
2. **Task 2: Adjust ProjectInstructionsProvider ordering to compose file and directory precedence** - `81cbbdf` (fix)
3. **Task 3: Confirm runtime code-agent factory still preserves nested instruction scope** - `1b37e93` (test, empty confirmation commit)

## Files Created/Modified

- `crates/lattice-prompt/src/project_instructions_provider.rs` - Adds the full root/child instruction matrix test and changes source enumeration to `CLAUDE.md` tier, `AGENTS.md` tier, then `LATTICE.md` tier.
- `crates/lattice-runtime/src/code_agent.rs` - Inspected and verified unchanged; the factory still constructs `ProjectInstructionsProvider::new(project_root, cwd)`.
- `.planning/phases/14-main-code-agent-eval-harness/14-06-SUMMARY.md` - Records the gap-closure execution and verification results.

## Verification

- `cargo test -p lattice-prompt project_instruction_precedence` - EXPECTED FAIL after Task 1 before the ordering fix; the new mixed matrix test failed because `child claude` rendered after `root agents`.
- `cargo test -p lattice-prompt project_instruction_precedence` - PASS after Task 2 and at plan level.
- `cargo test -p lattice-runtime code_agent_uses_root_and_cwd_project_instructions` - PASS after Task 3 and at plan level.
- `cargo fmt --all --check` - PASS.

## Decisions Made

- Use file-tier-major rendering to align the prompt order with the canonical same-directory priority statement.
- Keep the canonical precedence text unchanged: deeper directories override parent directories; within one directory `LATTICE.md > AGENTS.md > CLAUDE.md`.
- Keep runtime assembly unchanged because the provider contract remained `ProjectInstructionsProvider::new(project_root, cwd)`.

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None. The Task 1 provider test failure was expected and proved the planned regression coverage before the fix.

## Known Stubs

None.

## Threat Flags

None. The touched code does not add new network endpoints, auth paths, file access surfaces beyond the existing project instruction provider behavior, or schema changes.

## User Setup Required

None.

## Next Phase Readiness

Ready for 14-07. The Phase 14 UAT instruction precedence gap is closed and covered by targeted provider/runtime verification.

## Self-Check: PASSED

- Summary file exists at `.planning/phases/14-main-code-agent-eval-harness/14-06-SUMMARY.md`.
- Key files exist: `crates/lattice-prompt/src/project_instructions_provider.rs` and `crates/lattice-runtime/src/code_agent.rs`.
- Task commits exist in git history: `1cf782a`, `81cbbdf`, and `1b37e93`.
- Stub scan found no placeholder or TODO/FIXME stubs in the touched source files.

---
*Phase: 14-main-code-agent-eval-harness*
*Completed: 2026-05-12*
