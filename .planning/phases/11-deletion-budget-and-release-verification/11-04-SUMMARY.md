---
phase: 11-deletion-budget-and-release-verification
plan: 04
subsystem: lattice-agent
tags: [rust, lattice-agent, deletion, AgentLoop]
requires:
  - phase: 11-deletion-budget-and-release-verification
    plan: 03
    provides: final lattice-llm tool contracts in lattice-tools
provides:
  - AgentLoop-only lattice-agent public surface
  - Removed legacy Agent/prompt/memory/sandbox/policy/state-machine source
  - Final lattice_llm::ToolCall tool seam for AgentLoop
affects: [phase-11-release-verification, lattice-api-turns, lattice-runtime-prune]
tech-stack:
  added: []
  removed:
    - lattice-agent dependencies on lattice-core, lattice-hooks, lattice-policy, lattice-prompt, lattice-sandbox
    - lattice-agent blob-store and sqlite-memory features
    - old Agent integration tests
patterns:
  - Agent crate is a single final execution crate centered on agent_loop.rs
key-files:
  created:
    - .planning/phases/11-deletion-budget-and-release-verification/11-04-SUMMARY.md
  modified:
    - Cargo.lock
    - crates/lattice-agent/Cargo.toml
    - crates/lattice-agent/src/lib.rs
    - crates/lattice-agent/src/agent_loop.rs
    - crates/lattice-runtime/Cargo.toml
  deleted:
    - crates/lattice-agent/src/agent_tests.rs
    - crates/lattice-agent/src/audit.rs
    - crates/lattice-agent/src/behavior.rs
    - crates/lattice-agent/src/blob.rs
    - crates/lattice-agent/src/bus_event_collector.rs
    - crates/lattice-agent/src/chat.rs
    - crates/lattice-agent/src/config.rs
    - crates/lattice-agent/src/context_manager.rs
    - crates/lattice-agent/src/event.rs
    - crates/lattice-agent/src/events_provider.rs
    - crates/lattice-agent/src/final_report.rs
    - crates/lattice-agent/src/hook.rs
    - crates/lattice-agent/src/manifest.rs
    - crates/lattice-agent/src/memory/mod.rs
    - crates/lattice-agent/src/memory/sqlite.rs
    - crates/lattice-agent/src/message_source.rs
    - crates/lattice-agent/src/prompt/memory_provider.rs
    - crates/lattice-agent/src/prompt/mod.rs
    - crates/lattice-agent/src/prompt/task_state_provider.rs
    - crates/lattice-agent/src/run.rs
    - crates/lattice-agent/src/sandbox.rs
    - crates/lattice-agent/src/state.rs
    - crates/lattice-agent/src/tool_result.rs
    - crates/lattice-agent/src/turn_engine.rs
    - crates/lattice-agent/tests/agent_mock.rs
    - crates/lattice-agent/tests/context_e2e.rs
key-decisions:
  - "lattice-agent now exports only AgentLoop, AgentLoopConfig, AgentTurnRequest, AgentTurnResult, AgentTurnStatus, AgentTurnEvent, and AgentToolResultSummary."
  - "AgentLoop ToolCallExecutor now accepts lattice_llm::ToolCall directly."
  - "Old Agent, prompt, memory, sandbox, policy, manifest, context-manager, hook, state-machine, blob, and final-report paths were deleted instead of shimmed."
patterns-established:
  - "Feature flags that only existed for deleted agent modules are removed from downstream manifests when they block Cargo resolution."
requirements-completed: [SIMP-02, SIMP-03, CRATE-05, VER-02, VER-03]
duration: 24min
completed: 2026-05-17
---

# Phase 11 Plan 04: Agent Collapse Summary

`lattice-agent` is now an AgentLoop-only crate. The old `Agent` architecture and its supporting prompt, memory, sandbox, policy, manifest, state, hook, blob, and final-report modules are gone from active source.

## Accomplishments

- Converted `AgentLoop` and test fakes to use `lattice_llm::ToolCall` directly.
- Removed `ToolOutcome`/denial-report assumptions from AgentLoop status classification and aligned it with the final tools denial text.
- Rewrote `crates/lattice-agent/src/lib.rs` to export only the final AgentLoop public surface.
- Deleted 26 old agent source/test files and pruned `lattice-agent/Cargo.toml` to only final dependencies.
- Removed the stale `lattice-runtime` feature edge to `lattice-agent/blob-store`, which otherwise prevented any Cargo command from resolving after agent feature deletion.

## Task Commits

1. **Tasks 1-3: AgentLoop final tool seam and legacy Agent deletion** - `ecbe909`

## Verification

| Command | Result |
| --- | --- |
| `cargo test -p lattice-agent` | PASS - 12 AgentLoop tests passed, doc tests passed. |
| `cargo check -p lattice-api` | PASS. |
| `cargo fmt --package lattice-agent -- --check` | PASS. |
| `rg -n "pub struct Agent\\b|TODO Phase 11|lattice_core|lattice_policy|lattice_prompt|lattice_sandbox|SandboxConfig|PromptRegistry|TurnEngine|TaskState|FinalReport|execute_outcome|ToolOutcome|blob-store" crates/lattice-agent crates/lattice-agent/Cargo.toml` | PASS - no matches. |
| `./scripts/source-loc-budget.sh --max 100000` | PASS - current total 42,721 lines; `lattice-agent` is 1,301 lines. |
| `cargo check -p lattice-runtime` | BLOCKED - `lattice-runtime` still imports old `lattice_agent::Agent`, `TaskState`, manifest/final-report/hook/sandbox types and old `lattice_session` compatibility types. |
| `cargo check -p lattice-cli` | BLOCKED through `lattice-runtime` for the same old runtime imports. |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Removed stale runtime feature edge**
- **Found during:** Cargo resolution after deleting `lattice-agent/blob-store`
- **Issue:** `lattice-runtime` still declared `blob-store = ["lattice-agent/blob-store"]`, so even `cargo test -p lattice-agent` failed during dependency resolution.
- **Fix:** Removed the stale feature edge from `crates/lattice-runtime/Cargo.toml`.
- **Verification:** `cargo test -p lattice-agent` and `cargo check -p lattice-api` reached and passed compilation.
- **Committed in:** `ecbe909`

## Issues Encountered

- `lattice-runtime` is now the next major deletion target: it still contains old runtime-owned `Agent`, `TaskState`, hook, audit, manifest, and old session-store imports. This is expected to be owned by Wave 4 Plan 11-06, which deletes old crate/runtime directories and prunes manifests.
- `cargo check -p lattice-cli` is blocked indirectly because the CLI still depends on `lattice-runtime`. The final v1.1 CLI path already uses `lattice-api` + `AgentLoop`, so the remaining work is removing the old runtime dependency and support files, not adding compatibility back to `lattice-agent`.

## Known Stubs

None inside `lattice-agent`. The crate now has only:

- `crates/lattice-agent/Cargo.toml`
- `crates/lattice-agent/src/lib.rs`
- `crates/lattice-agent/src/agent_loop.rs`

## Threat Flags

None. This plan removed hidden legacy execution paths and did not add new IO, network, auth, or approval behavior.

## Next Phase Readiness

Wave 4 can proceed to `11-06` to delete old runtime/crate compatibility remnants, remove stale CLI/runtime dependencies, and move the workspace toward broad check gates.
