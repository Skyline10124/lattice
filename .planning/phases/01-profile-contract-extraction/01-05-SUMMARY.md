---
phase: "01-profile-contract-extraction"
plan: "05"
tasks_completed: 3
created: "2026-05-14T12:00:00Z"
has_checkpoints: false
key-files:
  modified:
    - crates/lattice-cli/Cargo.toml
    - crates/lattice-cli/src/commands/new_agent.rs
    - crates/lattice-runtime/src/lib.rs
    - crates/lattice-runtime/src/handle.rs
    - crates/lattice-runtime/src/preset.rs
    - crates/lattice-runtime/src/turn.rs
    - crates/lattice-cli/src/frontend/tui/*.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/commands/debug.rs
    - crates/lattice-cli/tests/code_agent_eval.rs
---

## Plan 01-05 Summary: Workspace bridge

### What was built

**Task 1:** Replaced legacy `TEMPLATE` constant in `new_agent.rs` with
`lattice_profile::default_agent_template()`. Added `lattice-profile` dependency
to `lattice-cli`. Tests verify v1 sections present and legacy fields absent.

**Task 2:** Already completed during 01-04 compilation fixes — all runtime/CLI
call sites use `bundles`/`enabled` instead of `ToolPreset`/`presets`.

**Task 3:** Already completed during 01-04 cleanup — `AgentPreset` removed from
all runtime handle/turn/executor code, test fixtures updated, CLI TUI references
fixed.

### Phase 1 Gates

- `cargo test -p lattice-profile` — 55 passed
- `cargo test -p lattice-tools capabilities` — 14 passed
- `cargo test -p lattice-cli new_agent` — 4 passed
- `cargo check --workspace` — 0 errors
- `cargo fmt --all --check` — passes
- No `AgentPreset`/`ToolPreset` in production code

### Self-Check: PASSED

All Phase 1 boundaries respected: `lattice-profile` has no upper-layer
dependencies, `lattice-tools` has no preset API, orchestrator/runtime/CLI
compile against new bundles/enabled capability API.
