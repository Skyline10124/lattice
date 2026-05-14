---
phase: 14-main-code-agent-eval-harness
plan: 02
subsystem: prompt
tags: [code-agent, project-instructions, precedence, runtime]

requires:
  - phase: 14-main-code-agent-eval-harness
    plan: 01
    provides: shared runtime code-agent factory and workspace provider
provides:
  - Explicit project instruction precedence contract
  - Provider tests for nested directory and same-directory precedence
  - Runtime test covering project_root plus cwd instruction scope
affects: [phase-14, lattice-prompt, lattice-runtime]

tech-stack:
  added: []
  patterns:
    - Provider renders lower-priority instruction blocks first
    - Runtime code-agent factory uses project_root and cwd for project instructions

key-files:
  modified:
    - crates/lattice-prompt/src/project_instructions_provider.rs
    - crates/lattice-runtime/src/code_agent.rs

key-decisions:
  - "Instruction precedence is stated in the prompt output and verified in provider tests."
  - "Same-directory render order is CLAUDE.md, then AGENTS.md, then LATTICE.md so later text carries higher precedence."
  - "Phase 14 still treats @file references as literal text and does not expand include syntax."

patterns-established:
  - "Nested child directory instructions render after parent directory instructions."
  - "Runtime tests exercise the factory-owned project instruction provider helper with both project root and cwd."

requirements-completed: [CODE-02, EVAL-03]

duration: 28 min
completed: 2026-05-12
---

# Phase 14 Plan 02: Project Instruction Precedence Summary

**Deterministic hierarchical project instruction loading for the code agent**

## Performance

- **Duration:** 28 min
- **Completed:** 2026-05-12T08:25:03Z
- **Tasks:** 2
- **Files modified:** 2 code files

## Accomplishments

- Updated `ProjectInstructionsProvider` to state the canonical precedence contract in generated context.
- Rendered parent directories before child directories, and same-directory files as `CLAUDE.md`, `AGENTS.md`, then `LATTICE.md`.
- Added provider tests for nested precedence, same-directory precedence, and literal `@file` handling.
- Added a runtime code-agent test proving the factory path preserves root plus nested cwd instruction scope.

## Task Commits

1. **Task 1: Make project instruction precedence explicit** - `7698e04` (feat)
2. **Task 2: Wire root and cwd into code-agent instruction provider** - `da132e6` (test)

## Files Created/Modified

- `crates/lattice-prompt/src/project_instructions_provider.rs` - Adds precedence header, deterministic render order, priority labels, and provider tests.
- `crates/lattice-runtime/src/code_agent.rs` - Uses a factory-owned project instruction provider helper and tests nested cwd instruction scope.

## Verification

- `cargo test -p lattice-prompt project_instruction_precedence` - PASS
- `cargo test -p lattice-runtime code_agent_uses_root_and_cwd_project_instructions` - PASS
- `cargo fmt --all --check` - PASS

## Decisions Made

- Keep `@nested.md` and other include-looking text literal for Phase 14.
- Use explicit `priority=parent/general`, `priority=local/native`, and related labels so tests and prompt consumers can inspect ordering.
- Add a focused runtime helper test instead of exposing prompt internals from `Agent`.

## Deviations from Plan

- The first executor stalled after writing Task 1 but before committing. The orchestrator interrupted it, reviewed the partial work, and completed the plan manually.
- The stalled executor also touched unrelated TUI tracked files; those changes were reverted before any 14-02 commit.

**Total deviations:** 2 operational deviations, both resolved.
**Impact on plan:** No scope change and no unrelated TUI files were committed.

## Issues Encountered

- Unrelated TUI worktree changes were already present and remain intentionally untouched.
- Runtime prompt internals are private, so the runtime test uses a focused factory helper path plus a successful `Runtime::build_code_agent` construction.

## Known Stubs

None.

## Threat Flags

None. The plan's parent/local precedence and include-expansion risks are covered by deterministic tests.

## User Setup Required

None.

## Next Phase Readiness

Ready for 14-03. The code-agent factory and project instruction provider now have deterministic nested instruction behavior.

## Self-Check: PASSED

- Summary file exists and records both task commits.
- Targeted provider/runtime tests pass.
- No unrelated TUI tracked files remain in the 14-02 diff.

---
*Phase: 14-main-code-agent-eval-harness*
*Completed: 2026-05-12*
