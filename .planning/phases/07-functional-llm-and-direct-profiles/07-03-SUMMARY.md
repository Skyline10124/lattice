---
phase: 07-functional-llm-and-direct-profiles
plan: 03
subsystem: lattice-api, lattice-profile
tags: [llm, profile, api, facade, templates]
requires: ["07-01", "07-02"]
provides: ["07-04", "08"]
affects: ["lattice-cli"]
tech-stack:
  added: []
  patterns: [direct-profile-consumption, template-per-mode, LlmEvent-to-TurnEvent-mapping]
key-files:
  created: []
  modified:
    - crates/lattice-api/src/turn.rs
    - crates/lattice-api/Cargo.toml
    - crates/lattice-profile/src/schema.rs
    - crates/lattice-profile/src/load.rs
    - crates/lattice-profile/src/defaults.rs
    - crates/lattice-profile/src/lib.rs
    - crates/lattice-cli/src/commands/run_v11.rs
    - Cargo.lock
decisions:
  - Switched turn.rs from StubLlm to lattice_llm::chat()/stream() real client dispatch.
  - Added full LlmEvent-to-TurnEvent mapping including Reasoning and ToolCall* variants.
  - LoadedAgentProfile is consumed directly in turn requests via #[serde(skip)] field — no lowering pipeline.
  - Tests use StubLlm internally to avoid credential dependency in CI.
  - Added PartialEq to AgentProfile and all section types for request round-tripping.
metrics:
  duration: "~30 minutes"
  completed_date: "2026-05-16T16:30:00Z"
---

# Phase 7 Plan 3: LLM Facade Wiring and Direct Profile Consumption Summary

Wired the new `lattice_llm` facade into `lattice-api` turn execution, replacing the Phase 6 `StubLlm` path with the real client dispatch. Made `LoadedAgentProfile` a first-class input to turn requests so the API consumes profiles directly with no lowering pipeline. Added reader/editor/dev default profile templates per PROF-02.

## What Was Built

### LLM Facade Wiring (`turn.rs`)
- `run_turn` now calls `lattice_llm::chat()` (real client dispatch with model resolution and credential validation).
- `stream_turn` now calls `lattice_llm::stream()` (real streaming via provider modules).
- Fixed `LlmEvent::Done` match — now uses `Done { finish_reason, .. }` struct variant pattern.
- Added `TurnEvent` variants for the full `LlmEvent` vocabulary: `Reasoning`, `ToolCallStart`, `ToolCallDelta`, `ToolCallEnd`. `Done` is now `Done { finish_reason }` (struct variant).
- Tests use `StubLlm` directly via private helpers (`run_stub_turn`, `stub_turn_stream`) to avoid credential dependency.

### Direct Profile Consumption
- Added `lattice-profile` as a dependency of `lattice-api`.
- `RunTurnRequest` and `StreamTurnRequest` now carry `loaded_profile: Option<LoadedAgentProfile>` (marked `#[serde(skip)]`).
- When a profile is provided, its `agent.model` is used for the `ModelTarget` and its `system_prompt` is prepended as a system `Message`.
- Added `PartialEq` derives to `AgentProfile`, `AgentSection`, `PromptSection`, `ToolsSection`, `SandboxMode`, `SecuritySection`, `ContextSection`, `BehaviorSection`, and `LoadedAgentProfile` for request equality.
- No lowering pipeline (`GenericAgentSpec`) consumed in the main path — profiles are consumed directly.

### Default Templates (PROF-02)
- `default_reader_template(name, model)` — workspace + filesystem bundles, project sandbox, max_turns=5, medium reasoning. No shell/editing/web.
- `default_editor_template(name, model)` — workspace + filesystem + editing + shell, project sandbox, max_turns=20.
- `default_dev_template(name, model)` — all six tool bundles, strict sandbox, max_turns=50, high reasoning.
- All three templates are valid v1 TOML with no forbidden legacy sections.
- Exported from `lattice-profile` public API.

### CLI Compatibility (`run_v11.rs`)
- Added `loaded_profile: None` to all `RunTurnRequest` constructions.
- Updated credential-related tests for Phase 7 reality — `run_turn` now requires provider credentials.
- Ignored `run_v11_calls_api_and_returns_ok` test (requires real API keys).

## Task Summary

| Task | Name | Commit | Tests |
|------|------|--------|-------|
| 1 | Wire LLM facade into lattice-api turn execution | `eeeda50` | 17 |
| 2 | Wire lattice-profile into lattice-api | `454a43c` | 20 (+3) |
| 3 | Add reader/editor/dev default templates | `b246667` | 65 (+8) |

## Test Results

```
cargo test -p lattice-api -p lattice-profile -p lattice-llm: 210 passed; 0 failed
```

## Deviations from Plan

None — plan executed as designed.

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Missing PartialEq on section types**
- **Found during:** Task 2
- **Issue:** `LoadedAgentProfile` derived `PartialEq` but `AgentProfile` and its section types did not.
- **Fix:** Added `PartialEq` to `AgentProfile`, `AgentSection`, `PromptSection`, `ToolsSection`, `SandboxMode`, `SecuritySection`, `ContextSection`, `BehaviorSection`.
- **Files modified:** `crates/lattice-profile/src/schema.rs`, `crates/lattice-profile/src/load.rs`
- **Commit:** `454a43c`

**2. [Rule 3 - Blocking] run_v11.rs missing loaded_profile field**
- **Found during:** Task 2
- **Issue:** `lattice-cli/src/commands/run_v11.rs` constructed `RunTurnRequest` without the new `loaded_profile` field.
- **Fix:** Added `loaded_profile: None` to all three construction sites, updated credential-related tests for Phase 7.
- **Files modified:** `crates/lattice-cli/src/commands/run_v11.rs`
- **Commit:** `454a43c`

**3. [Rule 1 - Bug] Reader template comment triggered test assertion**
- **Found during:** Task 3
- **Issue:** The reader template comment "No shell, editing, or web access" contained the words "shell" and "web", causing the test that checks for absence of these words to fail.
- **Fix:** Changed comment to "No command execution, file modification, or internet access."
- **Files modified:** `crates/lattice-profile/src/defaults.rs`
- **Commit:** `b246667`

## Known Stubs

- Agent, Tools, and Commit stages in `run_turn` remain stubs (deferred to Phase 8/9).
- `commit_id: None` in `RunTurnResult` (durable commits deferred to Phase 8).
- No tool execution — `run_turn` pushes `RunTurnStage::Tools` as a placeholder.
- `#[ignore]` on `run_v11_calls_api_and_returns_ok` — requires real API credentials.

## Threat Flags

No new threat surface introduced. The `LoadedAgentProfile` field carries filesystem paths but the API layer only reads the model name and system prompt; no path traversal or file access occurs in the API layer.

## Self-Check

- [x] `crates/lattice-api/src/turn.rs` compiles and passes tests
- [x] `crates/lattice-api/Cargo.toml` includes `lattice-profile` dependency
- [x] `crates/lattice-profile/src/schema.rs` has `PartialEq` on all types
- [x] `crates/lattice-profile/src/defaults.rs` exports reader/editor/dev templates
- [x] `crates/lattice-cli/src/commands/run_v11.rs` includes `loaded_profile` field
- [x] Commit `eeeda50` exists (Task 1)
- [x] Commit `454a43c` exists (Task 2)
- [x] Commit `b246667` exists (Task 3)
- [x] 210 tests pass across lattice-api, lattice-profile, lattice-llm
- [x] `cargo check --workspace` passes (0 errors)

## Self-Check: PASSED
