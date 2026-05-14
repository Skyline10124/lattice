---
phase: "01-profile-contract-extraction"
plan: "04"
tasks_completed: 2
created: "2026-05-14T11:45:00Z"
has_checkpoints: false
key-files:
  modified:
    - crates/lattice-orchestrator/src/agent_spec.rs
    - crates/lattice-orchestrator/src/lib.rs
    - crates/lattice-orchestrator/src/assembly.rs
    - crates/lattice-runtime/src/lib.rs
    - crates/lattice-runtime/src/preset.rs
    - crates/lattice-runtime/src/handle.rs
    - crates/lattice-runtime/src/turn.rs
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/src/frontend/tui/*.rs
    - crates/lattice-cli/src/commands/debug.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/tests/code_agent_eval.rs
---

## Plan 01-04 Summary: Remove AgentPreset from orchestrator

### What was built

Removed `AgentPreset` enum from `lattice-orchestrator::agent_spec` and
`lattice-runtime::preset`. Deleted `preset` field from `AgentTurnSpec`,
`AgentSpec`, and `AgentHandleKey`. Updated `lower_profile_to_turn_spec` to
no longer inspect `capabilities.presets` for "coding" or "code-agent" strings.

Orchestrator's assembly.rs maps legacy preset strings to bundles where needed
(reader → Filesystem, coding/code-agent → all 6 bundles). Runtime's 
`build_agent` now detects coding-capable agents through bundle count rather
than typed preset. All TUI, CLI, and eval test references updated.

### Task Completion

| Task | Status |
|------|--------|
| Task 1: Remove AgentPreset from agent_spec | Complete |
| Task 2: Keep executor compile-safe | Complete |

### Tests

- **95 tests pass** in `lattice-orchestrator`
- `cargo check` — 0 errors, full workspace compiles
- `rg` verification: no AgentPreset/ToolPreset in production orchestrator code

### Self-Check: PASSED

- `cargo test -p lattice-orchestrator` — 95 passed
- `cargo check` — 0 errors
- No preset symbols in production code
- Orchestrator compiles without importing `lattice-profile`
