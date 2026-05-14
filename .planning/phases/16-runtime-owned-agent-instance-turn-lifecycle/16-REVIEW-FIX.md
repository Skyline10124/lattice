---
phase: 16
date: 2026-05-13
status: all_fixed
fix_scope: all
findings_in_scope: 16
fixed: 16
skipped: 0
iteration: 1
---

# Phase 16 — Code Review Fix Report

## Summary

Applied fixes for all Critical, Warning, and Info findings from `16-REVIEW.md`.

Several fixes touch runtime orchestration semantics and should still receive human review in normal phase verification, especially schema retry behavior, session persistence semantics, and runtime/internal assembly equivalence.

## Fixed Findings

| Finding | Status | Notes |
|---|---|---|
| CR-16-01 | fixed | Handles now store full `turn::TurnCompletion` records, preserving task state, manifests, token usage, final report, model/provider, session id, and errors. |
| CR-16-02 | fixed | `execute_turn` receives resolved model/provider from handle state and records them in completion. |
| CR-16-03 | fixed | Turn task panics are caught, terminal error completion is stored, active flag is cleared, and stale handles without agents are rebuilt on the next lookup. |
| CR-16-04 | fixed | `session_id`, `persist_session`, and runtime `SessionStore` now flow into turn execution; completed turns can persist sessions and manifests. |
| CR-16-05 | fixed | Runtime security cache and handle keys now include the full security-relevant config/policy, not only sandbox mode. |
| CR-16-06 | fixed | `looks_like_error`, markdown fence stripping, raw-text parsing, and schema helpers moved to shared `lattice-orchestrator::json_output`. |
| CR-16-07 | fixed | Removed the lean handle completion type; handle history stores the rich public completion type directly. |
| CR-16-08 | fixed | Runtime internal agent construction now preserves coding capabilities, project instructions, workspace context, and task-state provider while using canonical assembly. |
| CR-16-09 | fixed | Removed unreachable `ProfileLoweringError::InvalidPreset`. |
| CR-16-10 | fixed | `rebuild_agent_handle` recomputes and updates the handle lookup key. |
| CR-16-11 | fixed | Runtime tool-start events include batch index/count; CLI verbose rendering shows multi-call counts. |
| CR-16-12 | fixed | Runtime-backed pipeline execution now returns `Err` for runtime `Error` and `Cancelled` turn completions. |
| CR-16-13 | fixed | Runtime-backed executor restores output-schema JSON parsing, validation, correction retry, and terminal error behavior. |
| CR-16-14 | fixed | Runtime handle lookup has a session-aware entrypoint and pipeline execution keys handles by `request.session_id`. |
| CR-16-15 | fixed | Runtime internal assembly now reuses canonical orchestrator assembly with PromptSpec, hooks, tool filtering, registry tools, retired-tool rejection, and sandbox options. |
| CR-16-16 | fixed | `Completed` runtime events are emitted only after final turn status is known, never for intermediate `LoopEvent::Done`. |

## Files Changed

- `crates/lattice-runtime/src/turn.rs`
- `crates/lattice-runtime/src/handle.rs`
- `crates/lattice-runtime/src/lib.rs`
- `crates/lattice-runtime/src/turn_executor.rs`
- `crates/lattice-orchestrator/src/json_output.rs`
- `crates/lattice-orchestrator/src/runner.rs`
- `crates/lattice-orchestrator/src/assembly.rs`
- `crates/lattice-orchestrator/src/agent_spec.rs`
- `crates/lattice-orchestrator/src/lib.rs`
- `crates/lattice-cli/src/coding_agent.rs`
- `crates/lattice-cli/src/frontend/tui/app.rs`
- `crates/lattice-cli/src/frontend/tui/agent_runner.rs`

## Verification

```sh
cargo fmt --all --check
cargo check -p lattice-runtime -p lattice-orchestrator -p lattice-cli
cargo test -p lattice-runtime handle_
cargo test -p lattice-orchestrator json_output
cargo test -p lattice-runtime -p lattice-orchestrator -p lattice-cli
cargo clippy -p lattice-runtime -p lattice-orchestrator -p lattice-cli -- -D warnings
```

All commands passed.

## Residual Risk

- Runtime turn session persistence now has basic `SessionStore` integration and manifest conversion. Full CLI/TUI migration should still verify durable session UX end to end.
- Runtime-owned pipeline schema retry behavior is restored at the executor layer; profiles with complex schemas should be covered by higher-level pipeline tests before Phase 17 depends on this path heavily.
