---
phase: 03-cli-tui-single-agent-ux
plan: "03-02"
subsystem: cli
tags: [runtime-turn, profile-runtime, code-command-removal]
requires:
  - phase: 03-cli-tui-single-agent-ux
    provides: profile-backed run path
provides:
  - neutral runtime turn/session seed/model inventory helpers
  - removal of specialized `lattice code` command and `coding_agent.rs`
affects: [cli-run, tui-agent-runner, runtime-completion]
tech-stack:
  added: []
  patterns: [neutral CLI runtime helper modules]
key-files:
  created:
    - crates/lattice-cli/src/runtime_turn.rs
    - crates/lattice-cli/src/model_inventory.rs
    - crates/lattice-cli/src/session_seed.rs
    - crates/lattice-cli/tests/profile_runtime_eval.rs
  modified:
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/lib.rs
    - crates/lattice-cli/src/test_support.rs
  deleted:
    - crates/lattice-cli/src/coding_agent.rs
    - crates/lattice-cli/tests/code_agent_eval.rs
key-decisions:
  - "Reusable completion, model inventory, and manifest seed behavior now lives under generic module names."
patterns-established:
  - "Profile runtime evals use an ordinary `code` profile fixture, not a built-in code preset."
requirements-completed: [CLI-03]
duration: 25min
completed: 2026-05-15
---

# Phase 03 Plan 02 Summary

**Specialized code-agent surface removed while preserving generic runtime turn helpers**

## Accomplishments
- Removed the `Code` subcommand and deleted `coding_agent.rs`.
- Created neutral `runtime_turn`, `model_inventory`, and `session_seed` modules.
- Renamed the integration eval to `profile_runtime_eval` and kept ordinary profile capability coverage.

## Verification
- `cargo test -p lattice-cli runtime_completion -- --nocapture` passed.
- `cargo test -p lattice-cli load_latest_manifest_metadata -- --nocapture` passed.
- `cargo test -p lattice-cli --test profile_runtime_eval -- --nocapture` passed.
- `cargo check -p lattice-cli --bin lattice` passed.

## Deviations from Plan

None - code-specific behavior was migrated or removed as planned.

## Issues Encountered

`git mv` required approval because it writes the Git index; after approval, the test file was renamed normally.

## Next Phase Readiness

Active CLI code has no `Commands::Code` or `coding_agent` dependency, so command-surface cleanup can proceed without preserving legacy aliases.
