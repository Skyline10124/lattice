---
phase: 03-cli-tui-single-agent-ux
plan: "03-04"
subsystem: tui
tags: [ratatui, profile-runtime, single-session]
requires:
  - phase: 03-cli-tui-single-agent-ux
    provides: profile runtime helper path
provides:
  - TUI `--agent` option and agent label
  - single selected profile runtime session path
  - snapshot label state for TUI status/welcome
affects: [tui-agent-runner, tui-statusline, tui-welcome]
tech-stack:
  added: []
  patterns: [one active RuntimeAgentSession cache]
key-files:
  modified:
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/frontend/tui/mod.rs
    - crates/lattice-cli/src/frontend/tui/app.rs
    - crates/lattice-cli/src/frontend/tui/agent_runner.rs
    - crates/lattice-cli/src/frontend/tui/view_model.rs
key-decisions:
  - "TUI uses the same `ProfileSource` parser and profile runtime preparation as `lattice run`."
  - "TUI keeps one active runtime session cache; no profile switching map was introduced."
patterns-established:
  - "TUI render model exposes `agent_label` and optional `snapshot_label`."
requirements-completed: [CLI-01, CLI-05]
duration: 25min
completed: 2026-05-15
---

# Phase 03 Plan 04 Summary

**TUI startup and turn submission now use one selected/default real agent profile**

## Accomplishments
- Added TUI `--agent` and `agent:<name|path>` label handling.
- Replaced generated `"coding"` TUI sessions with selected profile session preparation.
- Added snapshot label state from the runtime profile snapshot hash.

## Verification
- `cargo test -p lattice-cli tui_runtime_uses_selected_profile_source -- --nocapture` passed.
- `cargo test -p lattice-cli view_model_exposes_agent_label -- --nocapture` passed.
- `cargo test -p lattice-cli tui_completion_sets_snapshot_label -- --nocapture` passed.
- `cargo check -p lattice-cli --bin lattice` passed.

## Deviations from Plan

Snapshot labels are set from the active `RuntimeAgentSession.profile_snapshot.hash` at session build time; turn manifests do not carry a profile hash field in the TUI completion payload.

## Issues Encountered

The existing `lattice_agent::TurnManifestObject` type lacks profile-hash data, so completion-time snapshot-label updates use the runtime session snapshot instead.

## Next Phase Readiness

The TUI runtime path no longer needs plugin sessions or generated coding prompts, enabling plugin UX deletion.
