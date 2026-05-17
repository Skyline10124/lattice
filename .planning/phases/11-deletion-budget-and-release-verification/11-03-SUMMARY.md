---
phase: 11-deletion-budget-and-release-verification
plan: 03
subsystem: tools-hooks
tags: [rust, lattice-tools, lattice-hooks, deletion]
requires:
  - phase: 11-deletion-budget-and-release-verification
    plan: 01
    provides: strict LOC and release-boundary gates
provides:
  - Final lattice-llm tool contracts in lattice-tools
  - Project-root tool executor without sandbox/policy compatibility stack
  - Lightweight hook lifecycle vocabulary without prompt/core dependencies
affects: [phase-11-release-verification, lattice-agent-turns, tool-execution]
tech-stack:
  added: []
  removed:
    - lattice-tools dependency on lattice-sandbox
    - lattice-hooks dependencies on lattice-core and lattice-prompt
    - Landlock crate dependency from lattice-tools
patterns:
  - Surviving tool and hook concepts live directly in final crates without removed crate identities
key-files:
  created:
    - .planning/phases/11-deletion-budget-and-release-verification/11-03-SUMMARY.md
  modified:
    - Cargo.lock
    - crates/lattice-tools/Cargo.toml
    - crates/lattice-tools/src/lib.rs
    - crates/lattice-tools/src/executor.rs
    - crates/lattice-tools/src/capabilities.rs
    - crates/lattice-tools/src/edit/apply.rs
    - crates/lattice-tools/src/edit/types.rs
    - crates/lattice-tools/src/edit/diagnostics.rs
    - crates/lattice-tools/src/tool_error.rs
    - crates/lattice-hooks/Cargo.toml
    - crates/lattice-hooks/src/lib.rs
key-decisions:
  - "ToolExecutor now uses lattice_llm::ToolCall directly and no longer exposes ToolOutcome or execute_outcome."
  - "DefaultToolExecutor retains project-root containment, symlink escape rejection, enabled-tool checks, file-size limits, and command timeout limits without importing lattice-sandbox."
  - "lattice-hooks keeps only local hook points, metadata, decisions, dispatch, and registry behavior."
patterns-established:
  - "Deletion plans treat dependency manifests, public exports, comments, tests, and diagnostic vocabulary as part of the active release boundary."
requirements-completed: [SIMP-02, SIMP-03, CRATE-05, VER-02, VER-03]
duration: 31min
completed: 2026-05-17
---

# Phase 11 Plan 03: Tools/Hooks Rehome Summary

`lattice-tools` and `lattice-hooks` now compile on final v1.1 contracts without the old core, prompt, sandbox, policy, or Landlock dependency surface.

## Accomplishments

- Rehomed tool call contracts to `lattice_llm::ToolCall` and removed the remaining `lattice-core`/old outcome compatibility seam from `lattice-tools`.
- Removed `lattice-sandbox`, approval-rule bypasses, policy denial types, `SandboxConfig`, `execute_outcome`, `ToolOutcome`, and Landlock helper code from tool execution.
- Preserved the final Phase 11 safety boundary with explicit enabled-tool checks, project-root canonicalization, parent traversal rejection, symlink escape rejection, size limits, and command timeout limits.
- Rewrote `lattice-hooks` as a small local lifecycle hook vocabulary and registry, independent of prompt compilation and core token-estimation types.
- Cleaned `Cargo.lock` after deleting the old dependency graph.

## Task Commits

1. **Task 1: Rehome tool model contracts to lattice-llm** - `47e3713`
2. **Tasks 2-3: Remove tools sandbox/policy stack and simplify hooks** - `5021f08`

## Verification

| Command | Result |
| --- | --- |
| `cargo test -p lattice-tools` | PASS - 60 unit tests, 8 exposure tests, doc tests. |
| `cargo test -p lattice-hooks` | PASS - 5 unit tests, doc tests. |
| `rg -n "lattice_core|lattice-core|lattice_sandbox|lattice-sandbox|lattice_policy|lattice-policy|lattice_prompt|lattice-prompt" crates/lattice-tools crates/lattice-hooks` | PASS - no matches. |
| `rg -n "lattice_sandbox|lattice-sandbox|SandboxConfig|DenialReport|ApprovalKind|PolicyDecision|RiskClass|ToolCatalog|landlock|execute_outcome|with_sandbox" crates/lattice-tools` | PASS - no matches. |
| `rg -n "lattice_core|lattice-core|lattice_prompt|lattice-prompt|PromptRegistry|PromptContribution|TokenEstimator|PromptHookSpec" crates/lattice-hooks` | PASS - no matches. |
| `cargo fmt --package lattice-tools -- --check` | PASS. |
| `cargo fmt --package lattice-hooks -- --check` | PASS. |
| `git diff --check` | PASS. |
| `./scripts/source-loc-budget.sh --max 100000` | PASS - current total 52,013 lines. |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Renamed patch boundary diagnostics away from sandbox vocabulary**
- **Found during:** Task 2 denylist verification
- **Issue:** Patch diagnostics still serialized `sandbox_denied` after sandbox removal.
- **Fix:** Replaced it with `boundary_denied` and updated tests.
- **Files modified:** `crates/lattice-tools/src/edit/types.rs`, `crates/lattice-tools/src/edit/apply.rs`
- **Verification:** `cargo test -p lattice-tools` and denylist grep passed.
- **Committed in:** `5021f08`

**2. [Rule 3 - Blocking] Kept resource limits while deleting Landlock**
- **Found during:** Task 2 implementation
- **Issue:** Bash execution mixed command timeout/resource limit enforcement with Landlock setup.
- **Fix:** Split the final executor boundary into `ToolLimits`, keeping timeout and size controls without Landlock imports.
- **Files modified:** `crates/lattice-tools/src/executor.rs`
- **Verification:** `bash_timeout_returns_promptly` passed.
- **Committed in:** `5021f08`

## Issues Encountered

- Both resumed subagents for this plan and the sibling Wave 2 plan returned 429 errors. The existing partial `11-03` working-tree changes were completed inline instead of discarded.
- Full `cargo fmt --all --check` still reports formatting drift in previously committed agent/CLI/LLM files outside this plan's dirty set. To avoid unrelated churn, this plan verified formatting only for touched packages.

## Known Stubs

None in the tools/hooks boundary. `lattice-agent` still references removed tool outcome APIs and is intentionally owned by Plan 11-04.

## Threat Flags

None. This plan removes dynamic approval/policy vocabulary from `lattice-tools` and retains explicit local containment checks.

## Next Phase Readiness

Wave 3 can proceed to `11-04` to collapse `lattice-agent` onto the final tool interface and remove the old agent-loop seams.
