---
phase: 06-crate-topology-and-main-path-skeleton
plan: 01
subsystem: lattice-llm
tags: [crate-topology, llm-vocabulary, stub-llm, v1.1, additive-refactor]
requires: []
provides: [lattice-llm-crate, llm-types, stub-chat, stub-stream]
affects: [workspace-topology, Cargo.toml]
tech-stack:
  added: [lattice-llm]
  patterns: [workspace-inheritance, deterministic-stub, serde-vocabulary, direct-types, tdd-red-green]
key-files:
  created:
    - crates/lattice-llm/Cargo.toml
    - crates/lattice-llm/src/lib.rs
    - crates/lattice-llm/src/types.rs
    - crates/lattice-llm/src/stub.rs
  modified:
    - Cargo.toml
    - Cargo.lock
decisions:
  - "Add lattice-llm as additive workspace crate beside legacy lattice-core (D-01 through D-05)"
  - "Define direct v1.1 vocabulary in types.rs without importing lattice-core (D-06)"
  - "Implement deterministic StubLlm with chat/stream that never reads credentials (D-18, D-19)"
  - "Use TDD RED/GREEN cycle for stub behavioral tests"
metrics:
  duration_seconds: 761
  completed_date: "2026-05-16T07:51:09Z"
  task_count: 2
  file_count: 6
---

# Phase 06 Plan 01: Add Standalone lattice-llm Crate with Vocabulary and Stub

Standalone `lattice-llm` crate introduced as an additive workspace member with direct v1.1 LLM vocabulary types (`Message`, `Role`, `ToolCall`, `ToolDefinition`, `ChatRequest`, `ChatResponse`, `LlmEvent`, `ModelTarget`, `ModelRouter`) and a deterministic `StubLlm` that provides fake `chat`/`stream` responses without provider credentials or `lattice-core` imports.

## Completed Tasks

| Task | Name | Type | Commit | Key Artifacts |
|------|------|------|--------|---------------|
| 1 | Add standalone lattice-llm crate to workspace | auto | `272db80` | Cargo.toml (workspace members + deps), crates/lattice-llm/Cargo.toml, crates/lattice-llm/src/lib.rs |
| 2 | Define direct LLM vocabulary and deterministic stub (TDD) | auto (tdd) | `ae0312f` (RED), `9a10d07` (GREEN) | crates/lattice-llm/src/types.rs, crates/lattice-llm/src/stub.rs, lib.rs updated |

## Verification Results

| Check | Result |
|-------|--------|
| `cargo metadata --format-version 1 --no-deps` | PASS |
| `cargo test -p lattice-llm` | PASS (18/18) |
| `lattice-llm` has no `lattice-*` dependencies (cargo metadata + jq) | PASS (`true`) |
| No forbidden re-exports or aliases (rg) | PASS (0 matches) |
| `cargo check --workspace` | PASS (clean) |

## TDD Gate Compliance

Task 2 followed the RED/GREEN cycle:

1. **RED** (`ae0312f`): Created `types.rs` with serde roundtrip tests (14 passed) and `stub.rs` with behavioral tests for chat "stub response" content and stream Started/Token/Done ordering (4 FAILED).
2. **GREEN** (`9a10d07`): Implemented `chat()` returning deterministic "stub response: {last_user_message}" and `stream()` yielding `[Started, Token("stub response: ..."), Done]`. All 18 tests pass.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed unused import warnings in stub.rs**
- **Found during:** Task 2 GREEN phase verification
- **Issue:** `ModelTarget` and top-level `StreamExt` imports were unused after GREEN implementation; `StreamExt` was needed only in test module
- **Fix:** Removed `ModelTarget` from top-level imports; moved `StreamExt` import into `#[cfg(test)] mod tests` scope
- **Files modified:** crates/lattice-llm/src/stub.rs
- **Commit:** `9a10d07`

## Decisions Made

- **Workspace placement:** `lattice-llm` added after `lattice-hooks` (bottom layer) and before `lattice-core`, reflecting its role as the new LLM foundation
- **Dependency selection:** `serde`, `serde_json`, `thiserror`, `futures` as runtime deps; `tokio` as dev-dep only (for `#[tokio::test]`)
- **Vocabulary design:** `FunctionCall` kept as separate struct (not flattened into `ToolCall`) matching the established pattern in `lattice-core`; `ModelRouter::resolve()` returns alias unchanged in Phase 6 (Phase 7 adds real resolution)
- **Stub output format:** `chat()` returns `"stub response: {last_user_message}"` to make the v1.1 path observable with per-request determinism

## Known Stubs

| Stub | File | Line | Reason |
|------|------|------|--------|
| `StubLlm::chat()` and `StubLlm::stream()` | crates/lattice-llm/src/stub.rs | 47, 82 | Intentional Phase 6 stub per D-18/D-19. Real provider HTTP in Phase 7. |
| `ModelRouter::resolve()` returns alias unchanged | crates/lattice-llm/src/types.rs | 192 | Intentional Phase 6 stub. Real alias resolution in Phase 7. |

## Threat Surface Scan

No new security-relevant surface beyond what is documented in the plan's threat model. The `StubLlm` does not read credentials, make HTTP calls, or access the filesystem. All public API types are pure data with serde derives.

