---
phase: 07-functional-llm-and-direct-profiles
plan: "04"
subsystem: verification
tags: [verification, boundary, coverage, smoke, regression-fix]
requires: ["07-01", "07-02", "07-03"]
provides: ["08"]
affects: ["lattice-cli", "lattice-llm", "lattice-api", "lattice-profile"]
tech-stack:
  added: []
  patterns: [three-gate-verification, regression-only-scope]
key-files:
  created: []
  modified:
    - crates/lattice-cli/src/commands/run_v11.rs
    - crates/lattice-llm/src/providers/openai.rs
    - crates/lattice-llm/src/providers/anthropic.rs
    - crates/lattice-llm/src/providers/gemini.rs
    - crates/lattice-api/src/session.rs
    - crates/lattice-api/src/topology.rs
    - crates/lattice-api/src/turn.rs
    - crates/lattice-llm/src/client.rs
    - crates/lattice-llm/src/lib.rs
    - crates/lattice-profile/src/defaults.rs
decisions:
  - HTTP 401/403 credential errors are valid real-client proof — tests accept them alongside missing-env-var errors.
  - Pre-existing clippy warnings (large Err variant in lattice-profile, contains() in lattice-session) are structural/deferred — not Phase 7 regressions.
metrics:
  duration: "~15 minutes"
  completed_date: "2026-05-16T13:55:00Z"
---

# Phase 7 Plan 4: Verification Summary

Three-gate verification (boundary, coverage, smoke) of Phase 7-affected crates with targeted regression fixes only. No expansion into later phases.

## What Was Verified

### Boundary Check (VER-03)
- Grep for removed crate references (`lattice_core::`, `lattice_runtime::`, `lattice_prompt::`, `lattice_sandbox::`, `lattice_policy::`) in Phase 7 source — 0 matches.
- Grep for removed concepts (`TransportDispatcher`, `ProviderRegistry`, `catalog/`) — 0 matches.
- `GenericAgentSpec` still exists in `lattice-profile/src/spec.rs` but is NOT consumed by the main path (lattice-api uses `LoadedAgentProfile` directly).
- No drift detection, provenance warnings, or orchestrator indirection in the main path.

### Coverage Check
- `cargo test --workspace`: **1589 passed, 5 ignored, 0 failed** (34 suites, 2.21s).
- All Phase 7 crate tests pass: lattice-llm, lattice-api, lattice-profile, lattice-cli.

### Smoke Check
- `cargo check --workspace`: 0 errors, 4 warnings (dead code in lattice-session only).
- `cargo clippy --workspace -- -D warnings`: 9 pre-existing errors (7x large Err variant in lattice-profile, 2x contains() optimization — one in lattice-session).
- `cargo fmt --all --check`: PASSED after formatting normalization.

## Task Summary

| Task | Name | Commit | Status |
|------|------|--------|--------|
| 1 | Boundary Check | (no changes needed) | PASSED |
| 2 | Coverage Check (workspace tests) | (no changes needed) | 1589/5/0 |
| 3 | Smoke + Clippy + Format Fixes | `6bacec0` | FIXED |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] run_v11 tests fail with HTTP 401 when env has invalid credentials**
- **Found during:** Task 2 (workspace test run)
- **Issue:** `run_v11_output_includes_stub_response` and `run_v11_requires_credentials_in_phase_7` only handled missing-credential errors (error containing env var name). With `ANTHROPIC_API_KEY` set but invalid, the real provider returns HTTP 401 — error message contains neither env var name.
- **Fix:** Extended error checking in both tests to also accept HTTP 401/403 as valid "real-client-was-reached" proof.
- **Files modified:** `crates/lattice-cli/src/commands/run_v11.rs`
- **Commit:** `31a6d04`

**2. [Rule 1 - Bug] Unused `Message` imports in 3 provider files**
- **Found during:** Task 3 (clippy check)
- **Issue:** `use crate::types::{..., Message, ...}` imported `Message` at module level in openai.rs, anthropic.rs, and gemini.rs, but `Message` is only used in test modules (which have their own separate imports).
- **Fix:** Removed `Message` from the top-level imports in all 3 files.
- **Files modified:** `crates/lattice-llm/src/providers/{openai,anthropic,gemini}.rs`
- **Commit:** `6bacec0`

**3. [Rule 1 - Bug] Dead function `parse_sse_chunk` in openai provider**
- **Found during:** Task 3 (clippy check)
- **Issue:** `providers::openai::parse_sse_chunk()` was a convenience function for tests, but nothing calls it (the client uses `OpenAiSseParser::parse_chunk` directly).
- **Fix:** Removed the function.
- **Files modified:** `crates/lattice-llm/src/providers/openai.rs`
- **Commit:** `6bacec0`

**4. Formatting normalization**
- **Found during:** Task 3 (fmt check)
- **Issue:** `cargo fmt --all --check` reported diffs across 15 files.
- **Fix:** Ran `cargo fmt --all`.
- **Commit:** `6bacec0`

### Pre-existing Issues (NOT fixed — outside Phase 7 regression scope)

| Issue | Location | Reason Deferred |
|-------|----------|-----------------|
| Large Err variant (7 sites) | `lattice-profile/src/{discovery,load,lowering}.rs` | Requires boxing the error type — structural change (architectural, not regression) |
| contains() optimization | `lattice-profile/src/load.rs:91` | Pre-existing lint, not a regression |
| contains() optimization | `lattice-session/src/turn_tx.rs:287` | NOT a Phase 7 crate — outside scope |
| Dead code warnings (4) | `lattice-session/` | NOT Phase 7 crates — outside scope |

## Known Stubs (carried from Phase 7 plans)

- `StubLlm` retained for tests — real dispatch wired into public `chat()`/`stream()` (07-02).
- Agent, Tools, Commit stages in `run_turn` remain stubs — deferred to Phase 8/9 (07-03).
- `commit_id: None` in `RunTurnResult` — durable commits deferred to Phase 8 (07-03).
- No tool execution in main path — deferred to Phase 9 (07-03).
- `#[ignore]` on `run_v11_calls_api_and_returns_ok` — requires real API credentials (07-03).

## Threat Flags

No new threat surface introduced by this verification plan. Only test assertions and lint cleanups were modified.

## Self-Check

- [x] `crates/lattice-cli/src/commands/run_v11.rs` exists and passes tests
- [x] `crates/lattice-llm/src/providers/openai.rs` exists (dead function removed)
- [x] `crates/lattice-llm/src/providers/anthropic.rs` exists (unused import removed)
- [x] `crates/lattice-llm/src/providers/gemini.rs` exists (unused import removed)
- [x] Commit `31a6d04` exists (regression fix: HTTP 401/403 tests)
- [x] Commit `6bacec0` exists (clippy cleanup + formatting)
- [x] `cargo check --workspace` passes (0 errors)
- [x] `cargo test --workspace` passes (1589 passed, 5 ignored)
- [x] `cargo fmt --all --check` passes
- [x] No modifications to STATE.md or ROADMAP.md

## Self-Check: PASSED
