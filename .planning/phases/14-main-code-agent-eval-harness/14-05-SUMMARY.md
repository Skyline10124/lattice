---
phase: 14-main-code-agent-eval-harness
plan: 05
subsystem: eval
tags: [code-agent, evals, profile-isolation, gap-analysis, ci]

requires:
  - phase: 14-main-code-agent-eval-harness
    plan: 01
    provides: Runtime::build_code_agent factory path
  - phase: 14-main-code-agent-eval-harness
    plan: 02
    provides: hierarchical project instruction loading
  - phase: 14-main-code-agent-eval-harness
    plan: 03
    provides: approval proposal state
  - phase: 14-main-code-agent-eval-harness
    plan: 04
    provides: state-derived final report
provides:
  - Deterministic Rust code-agent eval harness with local scripted OpenAI-compatible responses
  - Six behavior-family eval fixtures for dirty worktree safety, project instructions, targeted reading, patch recovery, verification honesty, and profile isolation
  - Profile-isolation regression coverage proving non-coding presets do not expose code-agent editing/shell/planning tools
  - Gap-analysis regression coverage tying Phase 14 to CC_CODEX_GAP_ANALYSIS.md
affects: [phase-14, lattice-cli, lattice-tools]

tech-stack:
  added: []
  patterns:
    - Product-loop evals use Runtime::build_code_agent instead of a hand-built Agent.
    - Scripted local SSE responses keep model behavior deterministic and offline.
    - Planning research files referenced by Rust evals are tracked despite .planning ignore rules.

key-files:
  created:
    - crates/lattice-cli/tests/code_agent_eval.rs
    - .planning/phases/14-main-code-agent-eval-harness/14-05-SUMMARY.md
  modified:
    - crates/lattice-tools/tests/exposure.rs
    - .planning/research/CC_CODEX_GAP_ANALYSIS.md
    - .planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md
    - .planning/phases/14-main-code-agent-eval-harness/14-RESEARCH.md
    - .planning/phases/14-main-code-agent-eval-harness/14-05-PLAN.md

key-decisions:
  - "The eval harness registers local OpenAI-compatible models through the credentialless ollama provider path."
  - "code_agent_eval.rs documents the six fixture families and quick command at the top of the file."
  - "Gap-analysis fixtures assert planning docs reference CC_CODEX_GAP_ANALYSIS.md so EVAL-04 remains connected to its research input."

patterns-established:
  - "spawn_scripted_openai_server provides deterministic local SSE model responses for product-level loop tests."
  - "Behavior evals assert state-derived reports and tool-call order instead of trusting model prose."
  - "Profile isolation is checked at both capability resolver level and Runtime::build_code_agent exposure level."

requirements-completed: [EVAL-01, EVAL-02, EVAL-04]

duration: 24 min
completed: 2026-05-12
---

# Phase 14 Plan 05: Code-Agent Eval Harness Summary

**Phase 14 now has deterministic Rust evals for the flagship code-agent loop and profile isolation**

## Performance

- **Duration:** 24 min
- **Started:** 2026-05-12T12:16:22Z
- **Completed:** 2026-05-12T12:39:58Z
- **Tasks:** 4
- **Files modified:** 6 tracked files in this plan

## Accomplishments

- Added `crates/lattice-cli/tests/code_agent_eval.rs` with a local scripted OpenAI-compatible SSE server, temp git repositories, runtime security helpers, and a `Runtime::build_code_agent` product path.
- Added deterministic evals for dirty worktree safety, project instruction obedience, targeted file reading/searching, patch recovery, and verification honesty.
- Added profile-isolation coverage in both `crates/lattice-tools/tests/exposure.rs` and the code-agent factory eval.
- Added gap-analysis coverage that reads `.planning/research/CC_CODEX_GAP_ANALYSIS.md` and asserts Phase 14 context, research, and plan docs reference it.
- Added top-of-file harness documentation listing all six fixture families and the quick command `cargo test -p lattice-cli --test code_agent_eval`.

## Task Commits

Each task was committed atomically:

1. **Task 1: Create deterministic code-agent eval harness helpers** - `0cdf149` (test)
2. **Task 2: Add five behavior-family eval fixtures** - `2d07572` (test)
3. **Task 3: Add profile-isolation and gap-analysis coverage evals** - `ba2f3a1` (test)
4. **Task 4: Document and run the Phase 14 eval gate** - `de5d52d` (test)

## Files Created/Modified

- `crates/lattice-cli/tests/code_agent_eval.rs` - Deterministic product-loop eval harness for the code-agent factory and six behavior families.
- `crates/lattice-tools/tests/exposure.rs` - Adds non-coding profile no-leak regression coverage.
- `.planning/research/CC_CODEX_GAP_ANALYSIS.md` - Tracked canonical EVAL-04 research input used by the eval harness.
- `.planning/phases/14-main-code-agent-eval-harness/14-CONTEXT.md` - Tracked context reference required by gap-analysis eval.
- `.planning/phases/14-main-code-agent-eval-harness/14-RESEARCH.md` - Tracked research reference required by gap-analysis eval.
- `.planning/phases/14-main-code-agent-eval-harness/14-05-PLAN.md` - Tracked plan reference required by gap-analysis eval.

## Verification

Targeted Phase 14 gate:

- `cargo test -p lattice-prompt project_instruction_precedence` - PASS
- `cargo test -p lattice-prompt workspace_context_provider` - PASS
- `cargo test -p lattice-runtime code_agent_factory` - PASS
- `cargo test -p lattice-agent approval_proposal` - PASS
- `cargo test -p lattice-agent verification_status` - PASS
- `cargo test -p lattice-agent final_report` - PASS
- `cargo test -p lattice-cli approve_proposal` - PASS
- `cargo test -p lattice-cli slash_approve` - PASS
- `cargo test -p lattice-cli final_report` - PASS
- `cargo test -p lattice-tools non_coding_profiles_do_not_expose_code_agent_tools` - PASS
- `cargo test -p lattice-cli --test code_agent_eval` - PASS

Full Phase 14 gate:

- `cargo fmt --all --check` - PASS
- `cargo clippy --workspace -- -D warnings` - PASS
- `cargo test --workspace` - PASS

## Decisions Made

- `register_scripted_model` uses provider id `ollama` because the runtime already treats that provider path as credentialless when `credential_keys` is empty.
- The factory exposure eval uses a non-routable local URL because it only builds the agent and inspects tool definitions; no model request is sent.
- Planning docs referenced by Rust evals were force-added despite `.planning/` being ignored so a clean checkout can run the eval harness.

## Deviations from Plan

- The plan-listed multi-filter Cargo commands were split into one Cargo filter per command because Cargo accepts one test filter before test-binary args.
- `ToolPreset::Reader` was used as the closest non-coding preset for profile-isolation checks.

**Total deviations:** 2 syntax/profile-selection adjustments.
**Impact on plan:** No scope loss; the split commands and reader preset match the intended coverage.

## Issues Encountered

- Registering the scripted eval model under a custom provider id produced a missing-credentials error during `Runtime::build_code_agent`; using the existing credentialless `ollama` provider path fixed it.

## Known Stubs

None.

## Threat Flags

None. The eval harness uses only local loopback scripted responses and does not read environment API keys or call live providers.

## User Setup Required

None.

## Next Phase Readiness

Phase 14 is complete. v1.2 is ready for milestone audit or completion workflow.

## Self-Check: PASSED

- `crates/lattice-cli/tests/code_agent_eval.rs` exists and contains `mod fixtures`.
- All six behavior families are listed in the harness top comments.
- `Runtime::build_code_agent` is exercised by the eval harness.
- Non-coding profile isolation has resolver-level and factory-level coverage.
- Gap-analysis references are enforced by a Rust integration test.
- Targeted and full Phase 14 gates passed.

---
*Phase: 14-main-code-agent-eval-harness*
*Completed: 2026-05-12*
