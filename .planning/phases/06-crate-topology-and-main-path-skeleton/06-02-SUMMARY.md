---
phase: 06-crate-topology-and-main-path-skeleton
plan: 02
subsystem: lattice-api
tags: [crate-topology, api-facade, session-skeleton, turn-skeleton, migration-homes, v1.1, additive-refactor]
requires: [lattice-llm-crate, llm-types, stub-chat, stub-stream]
provides: [lattice-api-crate, session-api, turn-api, stream-api, topology-table]
affects: [workspace-topology, Cargo.toml]
tech-stack:
  added: [lattice-api]
  patterns: [workspace-inheritance, thin-facade, deterministic-stub, tdd-red-green, boundary-enforcement]
key-files:
  created:
    - crates/lattice-api/Cargo.toml
    - crates/lattice-api/src/lib.rs
    - crates/lattice-api/src/session.rs
    - crates/lattice-api/src/turn.rs
    - crates/lattice-api/src/topology.rs
  modified:
    - Cargo.toml
    - Cargo.lock
decisions:
  - "Add lattice-api as additive workspace crate beside legacy lattice-runtime (D-01 through D-03)"
  - "Define direct session/turn/topology types without importing legacy crates (D-08, D-12, D-26, D-27)"
  - "Implement run_turn with StubLlm integration and ordered stage trace (profile/session/agent/llm/tools/commit/render) (D-14, D-16)"
  - "Implement stream_turn with fake Started/Token/Done event mapping (D-17)"
  - "Record migration homes for lattice-prompt, lattice-sandbox, lattice-policy in topology.rs (CRATE-04, D-10 through D-12)"
  - "Use TDD RED/GREEN cycle for session/turn implementation and topology tests"
metrics:
  duration_seconds: 686
  completed_date: "2026-05-16T08:16:16Z"
  task_count: 3
  file_count: 7
---

# Phase 06 Plan 02: Add lattice-api Facade with Session/Turn/Stream Skeletons

Thin `lattice-api` v1.1 facade crate introduced with narrow dependencies (`lattice-llm`, `serde`, `serde_json`, `thiserror`, `futures`, `uuid`) and direct `create_session`, `open_session`, `run_turn`, and `stream_turn` API functions backed by the deterministic `StubLlm`. Migration homes for `lattice-prompt`, `lattice-sandbox`, and `lattice-policy` are recorded in `topology.rs`.

## Completed Tasks

| Task | Name | Type | Commit | Key Artifacts |
|------|------|------|--------|---------------|
| 1 | Add lattice-api manifest and facade exports | auto | `e85d489` | Cargo.toml (workspace members + deps), crates/lattice-api/Cargo.toml, crates/lattice-api/src/lib.rs |
| 2 | Implement session and turn skeleton APIs | auto (tdd) | `131ec73` (RED), `77c8d55` (GREEN) | crates/lattice-api/src/session.rs, crates/lattice-api/src/turn.rs, lib.rs updated |
| 3 | Record migration homes and enforce API boundaries | auto (tdd) | `4b97e79` (RED), `0f70cb4` (GREEN) | crates/lattice-api/src/topology.rs, lib.rs updated |

## Verification Results

| Check | Result |
|-------|--------|
| `cargo test -p lattice-api` | PASS (15/15) |
| `cargo test -p lattice-api run_turn` | PASS (4/4) |
| `cargo test -p lattice-api stream_turn` | PASS (2/2) |
| `cargo test -p lattice-api topology` | PASS (5/5) |
| `cargo check --workspace` | PASS (no errors, no warnings) |
| Forbidden dependency check (cargo metadata + jq) | PASS (`true`) |
| Forbidden re-export check (rg) | PASS (0 matches) |
| `RunTurnResult.commit_id == None` asserted | PASS |
| Stage trace ordered: profile/session/agent/llm/tools/commit/render | PASS |
| `stream_turn` yields Started, Token, Done | PASS |

## TDD Gate Compliance

Task 2 followed the RED/GREEN cycle:

1. **RED** (`131ec73`): Created `session.rs` and `turn.rs` with type definitions, `todo!()` stubs, and 10 tests. 1 passed (pure data type), 9 failed (stub panics).
2. **GREEN** (`77c8d55`): Implemented `create_session` with UUID v4, `open_session` with shaped response, `run_turn` with StubLlm integration and ordered stage trace, `stream_turn` with LlmEvent-to-TurnEvent mapping. All 10 tests pass.

Task 3 followed the RED/GREEN cycle:

1. **RED** (`4b97e79`): Created `topology.rs` with `TopologyEntry` type, `todo!()` stub, and 5 tests. All 5 failed (stub panics).
2. **GREEN** (`0f70cb4`): Implemented `migration_homes()` returning entries for lattice-prompt, lattice-sandbox, and lattice-policy. All 5 tests pass.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed unused import `self` in turn.rs**
- **Found during:** Task 2 GREEN phase verification
- **Issue:** `use futures::stream::{self, Stream, StreamExt}` had unused `self` import
- **Fix:** Removed `self` from the import
- **Files modified:** crates/lattice-api/src/turn.rs
- **Commit:** `77c8d55`

## Known Stubs

| Stub | File | Line | Reason |
|------|------|------|--------|
| `run_turn` calls `StubLlm.chat()` | crates/lattice-api/src/turn.rs | 96 | Intentional Phase 6 stub per D-18/D-19. Real provider HTTP in Phase 7. |
| `stream_turn` maps fake `StubLlm.stream()` events | crates/lattice-api/src/turn.rs | 128 | Intentional Phase 6 stub per D-17. Real streaming in Phase 7. |
| `run_turn` returns `commit_id: None` | crates/lattice-api/src/turn.rs | 111 | Intentional per D-16. Real commits in Phase 8. |
| `run_turn` Agent/Tools/Commit stage entries are stub records | crates/lattice-api/src/turn.rs | 89, 102, 105 | Intentional skeleton stages. Real agent/tools in Phase 9. |
| `create_session` and `open_session` do not touch disk | crates/lattice-api/src/session.rs | 73, 79 | Intentional per plan. Session persistence in Phase 8. |
| `migration_homes` is a static table, not migration execution | crates/lattice-api/src/topology.rs | 37 | Intentional per D-10/D-11. Actual crate migration in Phases 9/11. |

## Threat Surface Scan

No new security-relevant surface beyond what is documented in the plan's threat model. The `run_turn` and `stream_turn` functions only call `StubLlm` (no provider credentials, no HTTP, no filesystem access). The `create_session` and `open_session` functions perform no I/O. The `topology.rs` module is a static table with no runtime behavior.

## Self-Check: PASSED

All 6 created files verified on disk. All 5 commits verified in git log.

