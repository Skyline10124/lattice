---
phase: 18-cli-live-rendering-and-alpha-release-gate
plan: 02
subsystem: cli
tags: [streaming, sequence-tests, jsonl, human-renderer, exit-code, reasoning-visibility]
dependency_graph:
  requires: [lattice-api::stream_turn, lattice-api::TurnEvent, lattice-cli::RunRenderState, lattice-cli::render_human_event, lattice-cli::render_json_event]
  provides: [fake_event_loop, fake_jsonl_loop, s test helpers, 12 multi-event sequence tests]
  affects: [lattice-cli]
tech_stack:
  added: []
  patterns: [vec-writer-test-driver, state-accumulated-assertion, jsonl-roundtrip-verification, mid-token-newline-trick-assertion]
key_files:
  created: []
  modified:
    - crates/lattice-cli/src/commands/run.rs
decisions:
  - "Vec<u8> writer parametrization for test isolation (D-A7): fake_event_loop and fake_jsonl_loop drive render_human_event/render_json_event against in-memory buffers"
  - "Plain #[test] (not #[tokio::test]) for all sequence tests: render functions are synchronous"
  - "s() helper for lossless Vec<u8>-to-String conversion in assertions"
  - "Protocol-violation test added as fix commit after context-compaction loss"
metrics:
  duration: 2400s
  completed: 2026-05-22T13:10:00Z
  tasks: 4
  files: 1
---

# Phase 18 Plan 02: Sequence Tests Summary

Multi-event sequence tests for the CLI streaming renderer covering happy-path flows, failure/cancellation/protocol-violation scenarios, JSONL round-trip verification across all 12 TurnEvent variants, and reasoning visibility rules (hidden default, visible verbose).

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add fake_event_loop / fake_jsonl_loop / s test helpers | e55b53c | run.rs |
| 2 | Add happy-path + interleave sequence tests (5 tests) | 1b1dfee | run.rs |
| 3 | Add failure / cancellation / protocol-violation sequence tests (4 tests) | ea0433e | run.rs |
| 4 | Add JSONL + reasoning-visibility sequence tests (3 tests) | fb639c0 | run.rs |
| fix | Add missing seq_stream_ends_without_terminal protocol violation test | cb8c28a | run.rs |

## Key Decisions

1. **Vec<u8> writer parametrization (D-A7)**: Both `fake_event_loop` and `fake_jsonl_loop` drive `render_human_event`/`render_json_event` against `Vec<u8>` buffers, returning the raw byte vectors plus accumulated `RunRenderState`. This gives exact byte-level assertions without needing real stdout/stderr capture.

2. **Plain `#[test]` (not `#[tokio::test]`)**: All sequence tests use synchronous `#[test]` because `render_human_event` and `render_json_event` are synchronous functions. No async runtime needed.

3. **`s()` helper**: A thin `String::from_utf8` wrapper that panics on invalid UTF-8, ensuring test buffers are always valid for string-based assertions.

4. **Protocol-violation test recovery**: The `seq_stream_ends_without_terminal_is_protocol_violation` test was lost during context compaction and was recovered in a separate fix commit.

## Verification Results

- `cargo fmt --all --check`: PASSED
- `cargo clippy -p lattice-cli --all-targets -- -D warnings`: PASSED (0 issues)
- `cargo test -p lattice-cli`: PASSED (136 tests)
- `crates/lattice-cli/src/lib.rs` does NOT exist (bin-only crate preserved)
- No `[lib]` stanza added to Cargo.toml
- No new `pub` modifiers on renderer functions

## Test Coverage

### New sequence tests (12 total)

| Test | Requirement | Scenario |
|------|-------------|----------|
| seq_happy_path_human | D-B1, D-B2, D-C2, D-A4 row 1 | Full happy-path: stdout=TokenDelta concat, stderr=status+commit, exit=0 |
| seq_happy_path_verbose_with_epilogue | D-C3, D-A3 | Verbose mode: epilogue shows session, commit, status, usage breakdown |
| seq_trailing_token_newline_emitted | D-B4 | TokenDelta without trailing \n gets final newline appended |
| seq_token_then_status_inserts_stderr_newline | D-B3 | Mid-token newline trick: stderr gets \n before status line |
| seq_multiround_usage_sums_into_state | D-A3, D-C3 | Two ModelFinished events: usage sums in state, epilogue reads from state |
| seq_tool_failure_renders_summary | D-B5, D-A4 row 1 | ToolCallFinished{ok:false} renders summary, exit still 0 (TurnCommitted.Completed) |
| seq_turn_failed_then_commit_drains_stream | D-A6, D-A4 row 2 | TurnFailed then TurnCommitted{Error}: stream drains past terminal, exit=1 |
| seq_commit_failed_renders_reason | D-A4 row 4 | CommitFailed renders reason, exit=1 |
| seq_stream_ends_without_terminal_is_protocol_violation | D-A4 row 7 | Stream ends without terminal: state has no terminal/commit, exit=1 |
| seq_jsonl_round_trips_every_variant | D-D1, D-D3, D-B6 | All 12 TurnEvent variants round-trip through JSONL; zero ANSI; zero human chrome |
| seq_reasoning_hidden_default_in_human | D-C2 | ReasoningDelta hidden from stderr in non-verbose mode |
| seq_reasoning_visible_in_verbose | D-C3 | ReasoningDelta visible in stderr in verbose mode |

### Preserved tests (124 total)

All 124 pre-existing tests remain green: 15 per-variant unit tests from Plan 01, 9 original tests, plus integration tests.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Missing protocol-violation test recovered**
- **Found during:** Post-commit verification (context compaction loss)
- **Issue:** `seq_stream_ends_without_terminal_is_protocol_violation` was specified in the plan but was not present in the committed code after context compaction
- **Fix:** Re-implemented the test from the plan specification and committed as a separate fix commit
- **Files modified:** crates/lattice-cli/src/commands/run.rs
- **Commit:** cb8c28a

### Cosmetic Issues

**1. Wrong commit message on Task 3**
- **Issue:** Commit `ea0433e` has message "add fake_event_loop, fake_jsonl_loop, and s helpers" instead of describing the failure/cancellation/protocol-violation tests. Content is correct; message is wrong.
- **Impact:** Cosmetic only; no functional impact

## Known Stubs

None.

## Threat Flags

None. No production code modified; only test additions under `#[cfg(test)]`.

## Self-Check: PASSED

- run.rs: FOUND
- 18-02-SUMMARY.md: FOUND
- All 6 commits verified: e55b53c, 1b1dfee, ea0433e, fb639c0, cb8c28a, b7fdbc5
- 136 tests pass, clippy clean, fmt clean
