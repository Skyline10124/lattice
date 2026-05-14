---
phase: 14-main-code-agent-eval-harness
plan: 01
subsystem: runtime
tags: [code-agent, runtime, prompt-provider, workspace-context, cli]

requires:
  - phase: 10-capability-bundles-profile-isolation
    provides: code-agent capability preset and authorized tool exposure
  - phase: 11-structured-editing-engine
    provides: structured patch tool and multi_patch retirement
  - phase: 12-permission-approval-workflow
    provides: RuntimeSecurity and sandbox approval state
  - phase: 13-agent-task-recovery-state
    provides: TaskState provider and session metadata seed
provides:
  - Shared WorkspaceContextProvider in lattice-prompt
  - Runtime-owned CodeAgentSpec and Runtime::build_code_agent factory
  - CLI code-agent builder delegated to the runtime factory
affects: [phase-14, code-agent, lattice-cli, lattice-runtime, lattice-prompt]

tech-stack:
  added: []
  patterns:
    - Runtime-owned factory for flagship code-agent assembly
    - Provider-based workspace context instead of CLI prompt stuffing
    - Direct CodeAgentSpec construction for tests and embedders

key-files:
  created:
    - crates/lattice-prompt/src/workspace_provider.rs
    - crates/lattice-runtime/src/code_agent.rs
  modified:
    - crates/lattice-prompt/src/lib.rs
    - crates/lattice-runtime/src/lib.rs
    - crates/lattice-cli/src/coding_agent.rs

key-decisions:
  - "Runtime::build_code_agent is the authoritative built-in code-agent factory."
  - "Workspace/repo startup context is now a prompt provider rather than CLI-owned prompt text."
  - "CLI code-agent assembly delegates to runtime while retaining session, rendering, and metadata responsibilities."

patterns-established:
  - "CodeAgentSpec validates canonical project_root/cwd before agent construction."
  - "Code-agent factory attaches ProjectInstructionsProvider, WorkspaceContextProvider, task state provider, authorized executor, and the runtime-owned system prompt."
  - "CLI tests assert delegation to runtime.build_code_agent instead of private capability assembly."

requirements-completed: [CODE-01, CODE-03, CODE-05, EVAL-03]

duration: 14 min
completed: 2026-05-12
---

# Phase 14 Plan 01: Shared Code-Agent Factory Summary

**Runtime-owned code-agent factory with shared workspace context provider and thin CLI delegation**

## Performance

- **Duration:** 14 min
- **Started:** 2026-05-12T07:06:00Z
- **Completed:** 2026-05-12T07:20:12Z
- **Tasks:** 3
- **Files modified:** 5 code files

## Accomplishments

- Added `WorkspaceContextProvider` to `lattice-prompt` with bounded directory listing, git state, visible instruction-file discovery, and likely verification hints.
- Added `CodeAgentSpec`, `code_agent_system_prompt`, and `Runtime::build_code_agent` in `lattice-runtime`.
- Refactored `lattice-cli` code-agent construction to delegate to `runtime.build_code_agent`, preserving session metadata seed behavior.

## Task Commits

Each task was committed atomically:

1. **Task 1: Add shared workspace context provider** - `1435625` (feat)
2. **Task 2: Add shared runtime code-agent factory** - `90c3099` (feat)
3. **Task 3: Make CLI code-agent builder a thin factory caller** - `065abb0` (feat)

## Files Created/Modified

- `crates/lattice-prompt/src/workspace_provider.rs` - Shared provider for top-level entries, git state, instruction file visibility, and verification hints.
- `crates/lattice-prompt/src/lib.rs` - Exports `WorkspaceContextProvider`.
- `crates/lattice-runtime/src/code_agent.rs` - Defines `CodeAgentSpec`, runtime prompt text, factory validation, factory assembly, and tests.
- `crates/lattice-runtime/src/lib.rs` - Exports the new code-agent module API.
- `crates/lattice-cli/src/coding_agent.rs` - Delegates agent construction to `runtime.build_code_agent` and removes CLI-owned workspace context helpers.

## Verification

- `cargo test -p lattice-prompt workspace_context_provider` - PASS
- `cargo test -p lattice-runtime code_agent_factory` - PASS
- `cargo test -p lattice-cli build_path_declares_code_agent_capability_preset` - PASS
- `cargo test -p lattice-cli load_latest_manifest_metadata` - PASS
- `cargo fmt --all --check` - PASS

The plan-listed CLI command `cargo test -p lattice-cli build_path_declares_code_agent_capability_preset load_latest_manifest_metadata` was attempted and failed before test execution because Cargo accepts only one test filter in that position. The two equivalent targeted test commands above were run separately and passed.

## Decisions Made

- Runtime owns the default code-agent prompt and factory assembly so CLI and embedders cannot drift.
- `WorkspaceContextProvider` emits startup workspace context through the prompt provider stack; CLI no longer stuffs repo context into prompt text.
- `CodeAgentSpec.security` is required, not optional, and the factory validates root/cwd before exposing code-agent tools.

## Deviations from Plan

None - plan executed exactly as written.

**Total deviations:** 0 auto-fixed.
**Impact on plan:** No scope creep.

## Issues Encountered

- The plan's combined CLI test command used invalid Cargo test-filter syntax. It was split into two equivalent targeted commands, both green.
- Unrelated TUI files appeared in the worktree during execution. They were not created, modified, staged, or committed by this plan.

## Known Stubs

None. Stub scan found no placeholder implementation in the files changed by this plan.

## Threat Flags

None. The new runtime factory and workspace provider match the plan's T-14-01 and T-14-02 threat model mitigations.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Ready for 14-02. The shared factory/provider foundation is in place for nested instruction behavior and code-agent-level eval fixtures.

## Self-Check: PASSED

- Created files exist: `crates/lattice-prompt/src/workspace_provider.rs`, `crates/lattice-runtime/src/code_agent.rs`, and this summary.
- Task commits found in git history: `1435625`, `90c3099`, `065abb0`.
- STATE/ROADMAP/REQUIREMENTS reflect plan advancement, 1/5 Phase 14 summaries, and completed requirement IDs.

---
*Phase: 14-main-code-agent-eval-harness*
*Completed: 2026-05-12*
