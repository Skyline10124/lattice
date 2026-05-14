---
phase: 15-generic-agent-runtime-naming-profile-assembly
plan: 04
subsystem: cli
tags: [rust, lattice-cli, lattice-runtime, agent-spec, coding-preset]

requires:
  - phase: 15-generic-agent-runtime-naming-profile-assembly
    provides: ToolPreset::Coding and AgentPreset::Coding from Plans 15-01 and 15-02
  - phase: 15-generic-agent-runtime-naming-profile-assembly
    provides: Orchestrator coding preset cleanup from Plan 15-03
provides:
  - CLI `lattice code` construction through `AgentSpec` plus `AgentPreset::Coding`
  - Deterministic CLI eval harness using `Runtime::build_agent`
  - Workspace verification proving legacy code-agent runtime API names are absent from crate source
affects: [phase-16-runtime-owned-agent-lifecycle, phase-19-regression-compatibility-hardening]

tech-stack:
  added: []
  patterns:
    - CLI product entrypoints construct generic `AgentSpec` values and opt into coding behavior with `AgentPreset::Coding`.
    - Deterministic eval fixtures route through `Runtime::build_agent` while preserving behavior-focused assertions.
    - Verification-only tasks are recorded with empty commits when no file change is required.

key-files:
  created:
    - .planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-04-SUMMARY.md
  modified:
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/tests/code_agent_eval.rs

key-decisions:
  - "The CLI coding entrypoint now builds through `Runtime::build_agent(AgentSpec::new(model).preset(AgentPreset::Coding))`."
  - "The eval harness keeps `make_code_agent` helper names as product-test vocabulary while changing their implementation to the generic runtime preset path."
  - "Shared tracking files were intentionally left unchanged; the phase orchestrator owns STATE.md and ROADMAP.md updates after wave completion."

patterns-established:
  - "Coding preset usage is explicit at CLI and test fixture boundaries."
  - "The CLI `coding_system_prompt` wrapper delegates to `lattice_runtime::coding_system_prompt`."

requirements-completed: [GEN-01, GEN-02, GEN-04]

duration: 6 min
completed: 2026-05-13
---

# Phase 15 Plan 04: CLI Coding Preset Migration Summary

**`lattice code` and its deterministic eval harness now use the generic `AgentSpec` plus `AgentPreset::Coding` runtime assembly path**

## Performance

- **Duration:** 6 min
- **Started:** 2026-05-13T03:53:40Z
- **Completed:** 2026-05-13T03:58:52Z
- **Tasks:** 3
- **Files modified:** 3

## Accomplishments

- Migrated `crates/lattice-cli/src/coding_agent.rs` from `CodeAgentSpec` and `Runtime::build_code_agent` to `AgentSpec` with `AgentPreset::Coding`.
- Updated the CLI `coding_system_prompt` wrapper to call `lattice_runtime::coding_system_prompt`.
- Migrated `code_agent_eval.rs` fixtures to `Runtime::build_agent`, renamed tests and scripted model IDs around coding preset terminology, and preserved all v1.2 behavior anchors.
- Ran full workspace build, test, clippy, fmt, and legacy API grep verification.

## Task Commits

Each task was committed atomically:

1. **Task 1: Migrate CLI coding_agent.rs to use AgentPreset::Coding + AgentSpec** - `9aceca9` (feat)
2. **Task 2: Migrate code_agent_eval.rs fixtures, imports, and test calls to AgentSpec + coding preset** - `6ec0d79` (test)
3. **Task 3: Full workspace build, test, clippy, fmt verification** - `181661a` (test, empty verification commit)

**Plan metadata:** pending final docs commit.

## Files Created/Modified

- `crates/lattice-cli/src/coding_agent.rs` - Builds coding agents through `AgentSpec::new(...).preset(AgentPreset::Coding)` and `runtime.build_agent(...)`.
- `crates/lattice-cli/tests/code_agent_eval.rs` - Uses the generic runtime preset path for deterministic eval fixtures and renamed coding-preset eval identifiers.
- `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-04-SUMMARY.md` - Records execution, verification, deviations, and self-check.

## Decisions Made

- Used `.preset(AgentPreset::Coding)` rather than `.coding_preset()` in the CLI to match the plan's explicit must-have grep and make the product preset visible at the call site.
- Kept helper function names `make_code_agent` / `make_code_agent_at` in the eval harness because they describe the `lattice code` product behavior under test; their implementation now routes through the generic preset API.
- Did not update `.planning/STATE.md` or `.planning/ROADMAP.md` because the user explicitly reserved shared tracking writes for the orchestrator.

## Verification

- `cargo test -p lattice-cli --bin lattice coding_agent::tests::build_path_declares_coding_preset` - passed.
- `cargo test -p lattice-cli --bin lattice coding_agent::tests` - passed, 11 tests.
- `cargo build -p lattice-cli` - passed.
- `cargo test -p lattice-cli --test code_agent_eval` - passed, 8 tests.
- `cargo build --workspace` - passed.
- `cargo test --workspace` - passed.
- `cargo clippy --workspace -- -D warnings` - passed.
- `cargo fmt --all --check` - passed.
- `rg -n 'CodeAgentSpec|build_code_agent|ToolPreset::CodeAgent' crates/ --glob '!target/**'` - zero results.
- `timeout 20s cargo run -p lattice-cli -- code "echo test" -m sonnet` - built and exited at credential resolution with missing API key configuration; no runtime panic.

## Acceptance Criteria

- `use lattice_runtime::{AgentPreset, AgentSpec, Runtime}` is the CLI `lattice_runtime` import in `coding_agent.rs`.
- `CodeAgentSpec`, `build_code_agent`, and `ToolPreset::CodeAgent` have zero crate source references.
- `AgentPreset::Coding` and `runtime.build_agent` are present in both the CLI path and eval fixture path.
- Deterministic evals for dirty worktree safety, project instruction loading, targeted reading, patch recovery, verification honesty, profile isolation, and gap-analysis linkage pass.
- Full workspace build, test, clippy, and fmt checks pass.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Adjusted invalid Task 1 unit-test command for a binary-only crate**
- **Found during:** Task 1 (CLI coding_agent.rs migration)
- **Issue:** The plan specified `cargo test -p lattice-cli --lib ...`, but `lattice-cli` has no library target. Running the command fails before reaching the intended test.
- **Fix:** Verified the same inline test suite through the binary target with `cargo test -p lattice-cli --bin lattice coding_agent::tests::build_path_declares_coding_preset` and `cargo test -p lattice-cli --bin lattice coding_agent::tests`.
- **Files modified:** None.
- **Verification:** Binary-target unit tests passed.
- **Committed in:** `9aceca9` for the Task 1 implementation; verification adjustment did not require a code change.

---

**Total deviations:** 1 auto-fixed (Rule 3: blocking verification command).
**Impact on plan:** No behavior scope changed. The alternate command verifies the same inline tests in the actual package target layout.

## Issues Encountered

- `cargo test -p lattice-cli coding_agent::tests::build_path_declares_coding_preset` compiles integration tests too and failed before Task 2 because `code_agent_eval.rs` still referenced the old API. This was expected sequencing; Task 2 migrated the eval harness and the full suite then passed.

## Known Stubs

None. Stub scan found only a legitimate runtime format string containing `{}` placeholders.

## Threat Flags

None. The plan changed CLI/runtime assembly calls and deterministic test identifiers; it did not introduce new network endpoints, auth paths, file access patterns, or schema trust boundaries beyond those already covered by the plan threat model.

## Authentication Gates

- `timeout 20s cargo run -p lattice-cli -- code "echo test" -m sonnet` reached model credential resolution and exited with missing provider API keys (`NOUS_API_KEY`, `GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, `OPENCODE_ZEN_API_KEY`, `KILO_API_KEY`, or `AI_GATEWAY_API_KEY`). This is an expected environment gate for live model execution and not a code failure.

## User Setup Required

None for build/test verification. Live `lattice code -m sonnet` execution requires one of the supported provider credentials listed by the CLI error.

## Next Phase Readiness

Phase 16 can rely on CLI coding agent construction and deterministic evals flowing through the generic runtime preset path. No shared tracking files were modified by this executor.

## Self-Check: PASSED

- Found modified file: `crates/lattice-cli/src/coding_agent.rs`
- Found modified file: `crates/lattice-cli/tests/code_agent_eval.rs`
- Found summary file: `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-04-SUMMARY.md`
- Found task commits: `9aceca9`, `6ec0d79`, `181661a`
- Confirmed `STATE.md` and `ROADMAP.md` were not modified.

---
*Phase: 15-generic-agent-runtime-naming-profile-assembly*
*Completed: 2026-05-13*
