---
phase: 02-runtime-assembly-cutover
plan: 02
subsystem: runtime
tags: [runtime, generic-agent-spec, assembly, sandbox, tools]
requires:
  - phase: 02-runtime-assembly-cutover
    provides: Session ProfileSnapshot and turn transaction contracts
provides:
  - RuntimeInvocationContext for invocation-only runtime inputs
  - Runtime::assemble_generic_agent from GenericAgentSpec
  - GenericAgentAssembly with transient Agent, resolved model, context length, and tool definitions
affects: [runtime-turns, cli-runtime-adapter, tui-runtime-adapter, runtime-boundary]
tech-stack:
  added: [lattice-profile dependency in lattice-runtime]
  patterns: [generic profile spec assembly, sandbox-bound tool executor]
key-files:
  created:
    - crates/lattice-runtime/src/assembly.rs
  modified:
    - crates/lattice-profile/src/spec.rs
    - crates/lattice-runtime/Cargo.toml
    - crates/lattice-runtime/src/lib.rs
    - Cargo.lock
key-decisions:
  - "Runtime consumes lattice_profile::GenericAgentSpec directly; no RuntimeAgentSpec type was introduced."
  - "Generic runtime assembly uses one resolved capability selection for both model-visible tools and executor authorization."
patterns-established:
  - "RuntimeInvocationContext carries cwd/project_root/provider/security/task-state outside the pure profile spec."
  - "Runtime generic assembly rejects empty capabilities and unknown bundles before constructing the agent."
requirements-completed: [RUN-01, RUN-02]
duration: 9min
completed: 2026-05-14T17:36:19Z
---

# Phase 02 Plan 02 Summary

**GenericAgentSpec-driven runtime assembly with sandbox-bound tool authorization**

## Performance

- **Duration:** 9 min
- **Started:** 2026-05-14T17:27:38Z
- **Completed:** 2026-05-14T17:36:19Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments

- Added serde round-trip support and tests for `GenericAgentSpec` and nested spec structs.
- Added `RuntimeInvocationContext`, `GenericAgentAssembly`, and `Runtime::assemble_generic_agent`.
- Wired project instructions/workspace context providers, system prompt, history, optional task-state seed, reasoning effort, resolved tool definitions, and `DefaultToolExecutor` with sandbox plus authorized tools.

## Task Commits

Pending in current Wave 2 commit.

## Files Created/Modified

- `crates/lattice-runtime/src/assembly.rs` - Generic runtime assembly implementation and tests.
- `crates/lattice-profile/src/spec.rs` - Serde derives and round-trip test for the pure profile spec.
- `crates/lattice-runtime/Cargo.toml` / `Cargo.lock` - Added runtime dependency on `lattice-profile`.
- `crates/lattice-runtime/src/lib.rs` - Exported generic assembly types.

## Decisions Made

- The assembly layer uses canonical `ToolBundle::from_str` instead of accepting unknown bundle names.
- The test source guard splits denylist tokens so acceptance grep does not self-match test strings.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Adjusted runtime test fixture to current model catalog shape**
- **Found during:** Wave 2 compile
- **Issue:** The initial test fixture used an older `ModelCatalogEntry` field shape and `Message::user`, which do not exist in the current code.
- **Fix:** Reused the current `canonical_id/providers` catalog shape and `Message::new(Role::User, ...)`.
- **Files modified:** `crates/lattice-runtime/src/assembly.rs`
- **Verification:** `cargo test -p lattice-runtime generic_agent_assembly -- --nocapture` passed.
- **Committed in:** pending Wave 2 commit

**Total deviations:** 1 auto-fixed (Rule 3)
**Impact on plan:** No semantic scope change; tests now match current codebase APIs.

## Issues Encountered

None beyond compile-time fixture correction.

## Verification

- `cargo test -p lattice-profile generic_agent_spec -- --nocapture` - passed
- `cargo test -p lattice-runtime generic_agent_assembly -- --nocapture` - passed
- `cargo test -p lattice-runtime sandbox -- --nocapture` - passed
- `cargo check -p lattice-runtime` - passed
- Acceptance grep for `RuntimeAgentSpec`, `AgentTurnSpec`, and `lattice_orchestrator::assembly` in runtime assembly - passed

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Wave 3 can now prepare a runtime session from `GenericAgentSpec`, open a session turn, deserialize the locked profile snapshot, assemble a transient agent through `assemble_generic_agent`, and commit terminal outcomes through `SessionStore::commit_turn`.

---
*Phase: 02-runtime-assembly-cutover*
*Completed: 2026-05-14T17:36:19Z*
