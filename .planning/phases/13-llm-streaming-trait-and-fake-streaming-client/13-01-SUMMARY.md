---
phase: 13-llm-streaming-trait-and-fake-streaming-client
plan: 01
subsystem: llm
tags: [llm-streaming, test-utils, fake-client, futures]
requires:
  - phase: 12-event-and-status-vocabulary-contract
    provides: event and status vocabulary cleanup that Phase 13 leaves unchanged
provides:
  - LlmClient::stream_chat default fallback over chat()
  - lattice_agent::testing::FakeStreamingLlmClient behind feature test-utils
  - streaming_seam integration tests covering D-13.2 through D-13.11b
affects: [phase-14-agent-streaming, phase-16-api-live-stream, phase-17-timeout-and-cancellation]
tech-stack:
  added: [futures]
  patterns:
    - feature-gated lattice_agent::testing module
    - two-channel streaming error model
    - FIFO fake streaming response queue
key-files:
  created:
    - crates/lattice-agent/src/testing.rs
    - crates/lattice-agent/tests/streaming_seam.rs
  modified:
    - Cargo.lock
    - crates/lattice-agent/Cargo.toml
    - crates/lattice-agent/src/agent_loop/traits.rs
    - crates/lattice-agent/src/lib.rs
key-decisions:
  - "Keep AgentLoop, RealLlmClient, lattice-llm sources, and lattice-api usage tests unchanged in Phase 13."
  - "Expose FakeStreamingLlmClient only through feature = \"test-utils\"."
  - "Keep startup failures as Result::Err and mid-stream failures as LlmEvent::Error."
patterns-established:
  - "Default stream_chat synthesizes Reasoning, Token, tool call events, and Done without Started."
  - "FakeStreamingLlmClient::chat aggregates queued events into ChatResponse for compatibility tests."
requirements-completed: [LLM-06, LLM-07, LLM-08, LLM-09]
duration: 11 min
completed: 2026-05-21
---

# Phase 13 Plan 01: LLM Streaming Seam Summary

**LlmClient streaming seam with a default buffered fallback and feature-gated fake streaming client**

## Performance

- **Duration:** 11 min
- **Started:** 2026-05-21T10:21:10Z
- **Completed:** 2026-05-21T10:32:25Z
- **Tasks:** 4
- **Files modified:** 6

## Accomplishments

- Added `LlmClient::stream_chat` with a default fallback that converts `ChatResponse` into a deterministic `LlmEvent` stream.
- Added `lattice_agent::testing::FakeStreamingLlmClient` behind `feature = "test-utils"`, with FIFO `push_events` / `push_error` behavior and a `call_count()` helper.
- Added 11 integration tests in `tests/streaming_seam.rs` covering the Phase 13 validation matrix for default fallback ordering, usage propagation, startup errors, mid-stream errors, aggregation, and metadata ignoring.
- Preserved the Phase 13 boundary: AgentLoop still calls `chat()`, `RealLlmClient` is unchanged, `lattice-llm` source is unchanged, and `lattice-api/tests/usage_turn.rs` is unchanged.

## Task Commits

Each task was committed atomically:

1. **Task 1: Add futures dependency and test-utils feature** - `bf500ab` (`chore(13-01): add lattice-agent streaming test feature`)
2. **Task 2: Add default LLM streaming fallback** - `f4ba34c` (`feat(13-01): add default LLM streaming fallback`)
3. **Task 3: Add FakeStreamingLlmClient** - `4556177` (`feat(13-01): add fake streaming LLM client`)
4. **Task 4: Cover streaming seam** - `e893bbb` (`test(13-01): cover LLM streaming seam`)
5. **Formatting follow-up for Task 3 file** - `b7ad466` (`refactor(13-01): format fake streaming client`)

## Files Created/Modified

- `Cargo.lock` - Records `futures` as a direct dependency of `lattice-agent`.
- `crates/lattice-agent/Cargo.toml` - Adds `futures = "0.3"` and `[features] test-utils = []`.
- `crates/lattice-agent/src/agent_loop/traits.rs` - Adds the default `stream_chat` fallback on `LlmClient`.
- `crates/lattice-agent/src/lib.rs` - Exposes `pub mod testing` only under `feature = "test-utils"`.
- `crates/lattice-agent/src/testing.rs` - Implements `FakeStreamingLlmClient`, its builder, and event aggregation into `ChatResponse`.
- `crates/lattice-agent/tests/streaming_seam.rs` - Adds the 11 required integration tests for D-13.2 through D-13.11b.

## Decisions Made

- Followed the plan's direct dependency requirement for `futures = "0.3"` in `lattice-agent`.
- Used feature-only module gating for `lattice_agent::testing`, not `cfg(test)`, so integration tests and downstream crates opt in consistently.
- Kept the exact two-channel error split: queued fake startup errors return `Err`, while `LlmEvent::Error` inside `push_events` remains in the stream.
- Did not format or modify `crates/lattice-llm/src/client.rs`, because that pre-existing drift is already deferred to Phase 14 and Phase 13 freezes `lattice-llm` source.

## Deviations from Plan

None - product scope and phase boundaries were executed as planned. The only extra commit was formatting cleanup for a Task 3 file after `rustfmt` narrowed the local diff.

**Total deviations:** 0 auto-fixed.
**Impact on plan:** No scope expansion; all Phase 13 boundaries remain intact.

## Issues Encountered

- `cargo test -p lattice-cli` and sandboxed `cargo test --workspace` failed when the CLI test subprocess tried to use the default session database under `/home/astrin/.config/lattice/sessions/session_v11.db`. Re-running with filesystem approval passed: `cargo test -p lattice-cli` reported 109 passed, and `cargo test --workspace` reported 447 passed.
- `cargo fmt --all --check` still fails only on the known pre-existing formatting drift in `crates/lattice-llm/src/client.rs:289`, already recorded in `.planning/STATE.md` as deferred to Phase 14. Phase 13 did not touch `lattice-llm`.

## Verification

- `cargo build -p lattice-agent` - passed
- `cargo build -p lattice-cli` - passed
- `cargo build -p lattice-api` - passed
- `cargo build --workspace` - passed
- `cargo build -p lattice-agent --features test-utils` - passed
- `cargo test -p lattice-agent --features test-utils --test streaming_seam` - passed, 11 tests
- `cargo test -p lattice-agent --features test-utils` - passed, 23 tests
- `cargo test -p lattice-agent` - passed, 12 tests
- `cargo test -p lattice-cli` - passed with approved filesystem access, 109 tests
- `cargo test -p lattice-api` - passed, 34 tests
- `cargo test --workspace` - passed with approved filesystem access, 447 tests
- `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` - passed
- `cargo clippy --workspace -- -D warnings` - passed
- `cargo fmt --all --check` - known pre-existing failure in `crates/lattice-llm/src/client.rs:289`

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Phase 14 can switch AgentLoop's model path from `chat()` to `stream_chat()` using the new seam and the deterministic fake streaming client. No runtime/API/CLI consumer behavior was changed in this phase.

---
*Phase: 13-llm-streaming-trait-and-fake-streaming-client*
*Completed: 2026-05-21*
