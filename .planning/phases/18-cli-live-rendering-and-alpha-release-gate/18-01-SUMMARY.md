---
phase: 18-cli-live-rendering-and-alpha-release-gate
plan: 01
subsystem: cli
tags: [streaming, renderer, jsonl, exit-code, human-mode]
dependency_graph:
  requires: [lattice-api::stream_turn, lattice-api::TurnEvent]
  provides: [commands::run::run (stream-based), RunRenderState, render_human_event, render_json_event, render_verbose_epilogue, exit_code_from_state]
  affects: [lattice-cli]
tech_stack:
  added: []
  patterns: [stream-consumption-loop, writer-parametrized-renderer, state-accumulated-exit-code, mid-token-stderr-newline-trick]
key_files:
  created: []
  modified:
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/main.rs
decisions:
  - "Option A layout: keep all renderer code in commands/run.rs (file stays readable at ~580 lines production code)"
  - "Per-delta reasoning lines instead of prefix-once + dimmed continuation (acceptable fallback per D-C3)"
  - "status_to_exit_code kept with #[allow(dead_code)] for preserved test compatibility"
  - "Removed RunTurnResult import after render_result deletion"
metrics:
  duration: 1166s
  completed: 2026-05-22T12:25:07Z
  tasks: 6
  files: 2
---

# Phase 18 Plan 01: CLI Streaming Renderer Summary

Stream-based `lattice run` command consuming `lattice_api::stream_turn` with per-event rendering, JSONL output mode, accumulated state for exit-code derivation, and full D-A4 matrix test coverage.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add RunRenderState + signal enums + state-update + exit-code helpers | 3330747 | run.rs |
| 2 | Implement render_json_event (JSONL) and update --json help + module rustdoc | 381f017 | run.rs, main.rs |
| 3 | Implement render_human_event with status lines and mid-token newline trick | b270722 | run.rs |
| 4 | Implement render_verbose_epilogue from RunRenderState | efd9956 | run.rs |
| 5 | Rewrite run_inner to drive stream_turn; delete render_result | ccc6118 | run.rs, main.rs |
| 6 | Add D-A4 exit-code-matrix table-driven unit test | 36ab011 | run.rs |

## Key Decisions

1. **Single-file layout (Option A)**: All renderer code stays in `commands/run.rs`. The file remains readable at ~580 lines of production code. No sibling module extraction needed.
2. **Per-delta reasoning lines**: Used the acceptable fallback from D-C3 (each `ReasoningDelta` gets a `reasoning: {content}` line on stderr) rather than prefix-once + dimmed continuation, which would have required additional state tracking.
3. **status_to_exit_code preserved**: The old `status_to_exit_code` function is kept with `#[allow(dead_code)]` because the three preserved tests (`run_status_to_exit_code_completed_is_success`, `_error_is_1`, `_cancelled_is_1`) still reference it. It is no longer called from production code.
4. **RunTurnResult import removed**: After `render_result` deletion, `RunTurnResult` was no longer referenced in production code. The integration tests that use `lattice_api::run_turn` reference `RunTurnResult` through the `lattice_api::` path.

## Verification Results

- `cargo fmt --all --check`: PASSED
- `cargo clippy -p lattice-cli --all-targets -- -D warnings`: PASSED (0 issues)
- `cargo test -p lattice-cli`: PASSED (124 tests)
- `render_result` fully deleted (0 matches in run.rs)
- `lattice_api::stream_turn` called exactly once in production body
- `lattice_api::run_turn` NOT called in `run_inner` (still called in 2 preserved integration tests)
- `lattice_api::run_turn` still exported from `lattice-api` alongside `stream_turn`
- `--json` help text mentions "JSONL" and alpha behavior change

## Test Coverage

### New tests (15 total)

| Test | Requirement | Rule |
|------|-------------|------|
| render_json_event_emits_single_line_with_trailing_newline | D-D1 | JSONL line format |
| render_json_event_roundtrips_every_variant | D-D1 | All 12 TurnEvent variants roundtrip |
| render_json_event_emits_no_ansi_escapes | D-B6/D-D3 | No ANSI in JSON mode |
| render_human_token_delta_writes_only_to_stdout | D-B1 | stdout/stderr separation |
| render_human_status_line_after_partial_token_inserts_stderr_newline | D-B3 | Mid-token newline trick |
| render_human_tool_finished_ok_renders_byte_size | D-B5 | Byte size with literal B suffix |
| render_human_tool_finished_err_renders_summary | D-B5 | Error summary with byte count |
| render_human_reasoning_hidden_default | D-C2 | Hidden in non-verbose |
| render_human_reasoning_visible_verbose | D-C3 | Visible in verbose |
| render_human_turn_started_silent_default | D-C2 | Silent in non-verbose human mode |
| render_human_committed_lowercases_status | D-B2 | Lowercase status in human rendering |
| verbose_epilogue_emits_all_fields_when_populated | D-C3 | All fields when state populated |
| verbose_epilogue_skips_unset_fields | D-C3 | Empty state = no output |
| verbose_epilogue_usage_from_state_not_committed | D-A3 | Usage from usage_total, not TurnCommitted |
| exit_code_from_state_covers_all_da4_rows | D-A4 | All 7 in-stream exit-code rows |

### Preserved tests (9 total)

All 9 tests from the original `#[cfg(test)] mod tests` block remain green:
- `run_status_to_exit_code_completed_is_success`, `_error_is_1`, `_cancelled_is_1`
- `run_profile_source_none_is_default`, `_name`, `_path`
- `run_missing_default_message_references_profile_path`
- `run_default_session_dir_under_lattice_sessions`
- `run_integration_creates_session_and_returns_commit`
- `run_integration_missing_session_returns_not_found`
- `tool_executor_uses_workspace_root_not_profile_package_dir`
- `run_source_has_no_old_crate_imports`

## Deviations from Plan

None - plan executed exactly as written.

## Known Stubs

None.

## Threat Flags

None. No new network endpoints, auth paths, file access patterns, or schema changes. All changes are CLI rendering layer consuming existing `stream_turn` API.

## Self-Check

PASSED - all files exist, all commit hashes verified, 124 tests pass, clippy clean, fmt clean.