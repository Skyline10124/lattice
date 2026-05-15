---
phase: 03-cli-tui-single-agent-ux
plan: "03-01"
subsystem: cli
tags: [profile-runtime, run-agent, session-snapshot]
requires:
  - phase: 02-runtime-assembly-cutover
    provides: runtime-owned session turn submission and immutable profile snapshots
provides:
  - profile-backed `lattice run --agent <name|path>` path
  - CLI-owned default profile creation helper
  - profile drift warning rendering for run JSON/stderr
affects: [cli-run, profile-runtime, tui-runtime]
tech-stack:
  added: []
  patterns: [ProfileSource parsing, CLI-owned default creation]
key-files:
  created: []
  modified:
    - crates/lattice-cli/src/profile_runtime.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/test_support.rs
key-decisions:
  - "`lattice run` now prepares sessions from real v1 agent profiles instead of generated temporary profiles."
  - "`--model` remains a runtime override layered on top of selected profile semantics."
patterns-established:
  - "CLI parses `--agent` into `ProfileSource` and keeps profile discovery side-effect-free."
requirements-completed: [CLI-01, CLI-05]
duration: 30min
completed: 2026-05-15
---

# Phase 03 Plan 01 Summary

**Profile-backed `lattice run --agent` with default-profile creation helpers and drift warning output**

## Accomplishments
- Added `profile_source_from_agent_arg`, `create_default_profile`, and missing-default copy helpers.
- Wired `lattice run --agent <name|path>` through `ProfileSource -> prepare_profile_runtime_session -> submit_session_turn`.
- Added JSON/stderr profile drift warning handling.

## Verification
- `cargo test -p lattice-cli profile_source -- --nocapture` passed.
- `cargo test -p lattice-cli missing_default_creation -- --nocapture` passed.
- `cargo test -p lattice-cli profile_runtime_model_override -- --nocapture` passed.
- `cargo test -p lattice-cli run_completion_json_includes_profile_warning -- --nocapture` passed.

## Deviations from Plan

None - plan executed within the intended CLI/profile scope.

## Issues Encountered

Some planned test filters had no exact historical match, so equivalent targeted tests were added and run.

## Next Phase Readiness

The run path no longer depends on generated `"run"` profiles and can support the code-command removal and TUI selected-profile path.
