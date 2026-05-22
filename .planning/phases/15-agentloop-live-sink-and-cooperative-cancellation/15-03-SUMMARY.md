---
phase: 15-agentloop-live-sink-and-cooperative-cancellation
plan: 03
subsystem: agent
tags: [agent-loop, tests, live-sink, cancellation, mpsc, streaming]

# Dependency graph
requires:
  - phase: 15-01
    provides: AgentTurnEvent PartialEq derive (D-15b), apply_optional_timeout, flush_partial_assistant_message
  - phase: 15-02
    provides: run_turn_live entrypoint, emit_agent_event, StreamOutcome::Cancelled, five new cancel checkpoints
provides:
  - "crates/lattice-agent/tests/live_sink_and_cancel.rs — 7 D-18 scenarios + optional D-21 timeout regression"
  - "drive_run_turn_live concurrent-drain helper — tokio::join!(run, drain) per RESEARCH §5"
  - "CancelOnDoneLlm — flips cancel between last assembly event and Done (checkpoint #3 fixture)"
  - "CancelOnFirstTool — flips cancel inside first execute (checkpoint #5 fixture)"
  - "CancelBetweenTokensLlm — flips cancel between Token('p') and Token('ar') (checkpoint #2 fixture, no new dev-dep)"
  - "PendingStreamLlm — never-yields LlmClient for the timeout regression"
affects: [15-04]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Concurrent drain via tokio::join!(run_turn_live, recv-loop) so bounded mpsc never deadlocks"
    - "futures::stream::unfold for state-machine-driven LlmClient fakes (no tokio-stream dev-dep)"
    - "PartialEq vector equality (assert_eq! on Vec<AgentTurnEvent>) to lock live-sink fidelity"
    - "ToolCallExecutor wrapper composing existing FakeToolExecutor for cancel-during-execute fixtures"

key-files:
  created:
    - crates/lattice-agent/tests/live_sink_and_cancel.rs
  modified: []

key-decisions:
  - "Per-task atomic commits — Plan 15-02 atomicity constraint does not apply here (tests don't share a compile-coupling map). Each scenario lands as its own commit so any future regression bisect is one-file-per-task."
  - "futures::stream::unfold over tokio-stream::ReceiverStream — RESEARCH §8 no-new-dep recommendation honored. Cargo.toml unchanged."
  - "Final fmt drift fixed in a separate style(15-03) commit so the seven scenario commits stay scope-pure (test logic only)."
  - "Channel capacity 32 (D-20) chosen via constant arg to drive_run_turn_live; concurrent drain makes capacity choice non-load-bearing."

requirements-completed: [AGENT-08, AGENT-09, VER-11]

# Metrics
duration: ~9min
completed: 2026-05-22
---

# Phase 15 Plan 03: Live Sink and Cooperative Cancellation Behavioral Suite Summary

**A new `crates/lattice-agent/tests/live_sink_and_cancel.rs` lands the 7 required D-18 scenarios + the optional D-21 timeout regression; every Phase 15 invariant is now defended at runtime via `cargo test -p lattice-agent --features test-utils`.**

## Performance

- **Duration:** ~9 min
- **Started:** 2026-05-21T18:37:58Z
- **Completed:** 2026-05-21T18:46:53Z
- **Tasks:** 9 (8 scenario tasks + 1 fmt-fix commit)
- **Files created:** 1
- **Files modified:** 0
- **Commits:** 9 task-scoped commits

## Accomplishments

- **`tests/live_sink_and_cancel.rs`** — 964-line behavioral suite gated `#![cfg(feature = "test-utils")]` (matches `streaming_delta.rs`). Eight `#[tokio::test]` functions exercise every cancel checkpoint in `run_turn_inner` + `consume_model_stream`:
  - **scenario_01_cancel_before_tool_execution** — checkpoint #3 (post-Done, pre-tool-loop). Custom `CancelOnDoneLlm` (futures::stream::unfold) flips cancel between the last assembly event and Done; consumer returns `StreamOutcome::Done`; `run_turn_inner` finalizes the assistant tool-call message + emits `ModelFinished`; checkpoint #3 returns Cancelled before any tool starts. Asserts: `Cancelled` status, `"cancelled by caller"` reason, ToolCallStarted=0, ModelFinished=1, assistant tool-call message in transcript, no tool result message, empty tool_results.
  - **scenario_02_cancel_during_tool_execution** — checkpoint #5. Reuses `CancelOnFirstTool` wrapping `FakeToolExecutor` queued with `Ok("real-tool-output")`. Wrapper flips cancel before forwarding to inner; inner returns the real result; `ToolCallFinished { ok: true }` is recorded; checkpoint #5 returns Cancelled before opening a second round. Asserts: ToolCallStarted=1, ToolCallFinished{ok=true}=1, ModelStarted=1, real tool message in transcript.
  - **scenario_03_live_sender_mirrors_result_events** — band B3 sample 1. Two-round successful turn (tokens, reasoning, one tool call, then final tokens); `assert_eq!(received_via_mpsc, result.events)` — element-wise vector equality via PartialEq from Plan 15-01.
  - **scenario_04_receiver_drop_does_not_cancel_turn** — band B3 sample 2 (D-12 / API-10). Drops `rx` immediately, then awaits `run_turn_live`; `emit_agent_event` swallows every `SendError`; turn returns `Completed` with full `result.events` populated.
  - **scenario_05_cancel_mid_stream_preserves_partial_content** — checkpoint #2 / band B2. Custom `CancelBetweenTokensLlm` (futures::stream::unfold) flips cancel between `Token("p")` and `Token("ar")`; `consume_model_stream` returns `StreamOutcome::Cancelled` after processing "ar"; "tial" and Done are never reached. Asserts: `Cancelled` status, partial content "par", reasoning_content `None`, no `ModelFinished`, exactly two `TokenDelta` events.
  - **scenario_06_cancel_after_first_tool_in_multi_tool_batch** — checkpoint #5 in a multi-tool batch. Two valid tool calls; `CancelOnFirstTool` flips cancel during c1; checkpoint #5 skips c2. Asserts: ToolCallStarted=1 (c1), ToolCallFinished=1 (c1), no events for c2, c1 GraphMessage in transcript, c2 GraphMessage not in transcript.
  - **scenario_07_run_turn_buffered_matches_run_turn_live** — band B4. Same fixture run via `run_turn` (None) and via `run_turn_live` + concurrent drain (Some); asserts `result_a.events == result_b.events`. Locks the backward-compat invariant.
  - **scenario_08_run_turn_live_honors_timeout** — D-21 optional regression. `PendingStreamLlm` returns `futures::stream::pending`; 50ms timeout elapses; `apply_optional_timeout` returns the buffered timeout shape (Error / "timeout" / empty messages / empty events). Confirms the helper is wired into both entrypoints.

- **`drive_run_turn_live(loop, request, capacity) -> (AgentTurnResult, Vec<AgentTurnEvent>)`** — the canonical concurrent-drain helper from RESEARCH §5. Uses `tokio::join!` (not `tokio::spawn`) so the `&AgentLoop` borrow stays usable; bounded channel capacity 32 (D-20).

- **Helper structs** — `FakeToolExecutor` (mirrors `streaming_delta.rs:40-69`), `CancelOnFirstTool` (wraps `FakeToolExecutor` + `Arc<AtomicBool>`), `CancelOnDoneLlm` and `CancelBetweenTokensLlm` (custom streaming `LlmClient` fakes via `futures::stream::unfold`), `PendingStreamLlm` (never-yields `LlmClient`). All lives in the test file; nothing leaks into `lattice_agent::testing`.

- **No new dependencies.** `Cargo.toml` byte-identical to pre-plan state — `futures::stream::unfold` (already in dev-deps via the `futures` crate) replaced the proposed `tokio-stream::ReceiverStream` per RESEARCH §8.

- **Full lattice-agent suite green:** `cargo test -p lattice-agent --features test-utils` reports 51/51 passing across 6 suites (lib, agent_loop, streaming_delta, streaming_seam, live_sink_and_cancel, doc-tests). `cargo clippy -p lattice-agent --all-targets --features test-utils -- -D warnings` clean. `cargo fmt --all --check` clean.

## Task Commits

| # | Task | Commit |
|---|------|--------|
| 1 | Scaffold `tests/live_sink_and_cancel.rs` (helpers + drain recipe) | `030fcaa` |
| 2 | Scenarios #1 + #6 (checkpoints #3 / #5) | `c327f01` |
| 3 | Scenario #2 (checkpoint #5, real-result preservation) | `40a85a0` |
| 4 | Scenario #3 (live mirror equality) | `e28f1a6` |
| 5 | Scenario #4 (receiver-drop tolerance) | `4839360` |
| 6 | Scenario #5 (checkpoint #2, partial content) | `ddb4e9b` |
| 7 | Scenario #7 (run_turn / run_turn_live B4) | `04416c3` |
| 8 | Scenario #8 (D-21 timeout regression) | `605938c` |
| 9 | rustfmt drift fix | `e8c5121` |

## Files Created/Modified

- **Created:** `crates/lattice-agent/tests/live_sink_and_cancel.rs` (964 lines, 8 `#[tokio::test]` scenarios + 5 helper structs + 1 free function).
- **Modified:** None. `Cargo.toml` unchanged.

## Decisions Made

- **Per-task atomic commits** — each scenario lands as one commit. Plan 15-02's atomic-commit directive applied only to the type-level coupling between `StreamOutcome::Cancelled`, the match arm, and the widened signatures. Plan 15-03 has no such coupling: each scenario is self-contained, so per-task commits maximize bisectability of any future regression.
- **futures::stream::unfold over tokio-stream::ReceiverStream** — RESEARCH §8 explicitly recommended the no-new-dep path. `unfold` lets `CancelOnDoneLlm` and `CancelBetweenTokensLlm` express their state machines (yield event, transition state, optionally side-effect) without adding `tokio-stream` to dev-dependencies.
- **CancelOnDoneLlm flips cancel only between the last assembly event and Done** — chosen so checkpoint #2 (post-event, pre-next-stream-pull) fires with `cancel = false` for every assembly event, then the cancel store happens, then Done is yielded; per D-03 Done is never a cancel point inside `consume_model_stream`, so the function returns `StreamOutcome::Done` and `run_turn_inner` finalizes the assistant message before checkpoint #3 observes the cancel. This is exactly the timing scenario #1 requires.
- **`Cargo.toml` left untouched intentionally** — verifying via `git diff 244018b..HEAD -- crates/lattice-agent/Cargo.toml` (empty diff) is a Plan 15-03 acceptance criterion. Honored.
- **rustfmt drift fix landed as a separate style(15-03) commit** — keeps the seven scenario commits scope-pure (test logic only). The drift was a single multi-line `let (result_b, ..) = drive_run_turn_live(...)` call site that fmt prefers to collapse onto one line; cosmetic only.

## Deviations from Plan

None. Plan executed exactly as written; every acceptance criterion verified.

- `crates/lattice-agent/tests/live_sink_and_cancel.rs` exists with `#![cfg(feature = "test-utils")]` as the first non-blank line.
- `drive_run_turn_live` exists, uses `tokio::join!`, channel capacity 32.
- `make_request` and `make_request_with_cancel` helpers present.
- `CancelOnFirstTool` struct present (Task 2).
- `CancelBetweenTokensLlm` struct present (Task 6); uses `futures::stream::unfold`; no `tokio_stream` reference anywhere in the file.
- All 8 scenario `fn ...` declarations marked `#[tokio::test]`; all assertions per plan acceptance criteria.
- Scenario #3 contains literal `assert_eq!(received, result.events);` — the load-bearing PartialEq lock.
- Scenario #4 contains `drop(rx);` before the `run_turn_live` await.
- Scenario #5 asserts `Cancelled` + partial content `"par"` + no `ModelFinished` event.
- Scenario #8 contains `Duration::from_millis(50)` and asserts `Error` + `Some("timeout")`.
- `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` exits 0 (8/8 passing in ~50ms).
- `cargo test -p lattice-agent --features test-utils` exits 0 (51/51 across 6 suites).
- `git diff 244018b..HEAD -- crates/lattice-agent/Cargo.toml` empty.

## Issues Encountered

One non-blocking surprise: `cargo test ... --test live_sink_and_cancel scenario_01_... scenario_06_...` (two TESTNAME positionals as the plan's verify command suggests) is rejected by `cargo test` — only one TESTNAME positional is supported. Worked around by running `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` (no filter — all scenarios in this file run together) which is equally fast (<100ms). No deviation from plan; the verify commands in `<acceptance_criteria>` filter via the binary's name-substring matcher and would still pass if executed individually.

## User Setup Required

None — no external service configuration, no new dependencies, no config changes.

## Verification

- `cargo build -p lattice-agent --features test-utils --tests` — clean (no errors, no new warnings beyond pre-existing baseline).
- `cargo test -p lattice-agent --features test-utils --test live_sink_and_cancel` — 8/8 passing.
- `cargo test -p lattice-agent --features test-utils` — 51/51 passing across 6 suites.
- `cargo clippy -p lattice-agent --all-targets --features test-utils -- -D warnings` — clean.
- `cargo fmt --all --check` — clean.
- `git diff 244018b..HEAD -- crates/lattice-agent/Cargo.toml` — empty (no dev-dep changes).

Coverage map (Nyquist sampling per RESEARCH §2):

| Band | Description | Scenarios | Status |
|------|-------------|-----------|--------|
| B1 | Cancellation timing across checkpoints | #1 (#3) / #2 (#5) / #5 (#2) / #6 (#5 multi-tool) | ≥ 4 samples ✓ |
| B2 | Partial-content preservation on cancel | #1 (assistant msg) / #5 (partial "par") + #2/#6 indirect | ≥ 2 samples ✓ |
| B3 | Live-sink fidelity + lifecycle | #3 (full mirror) / #4 (drop) + bounded-channel safety implicit | ≥ 2 samples ✓ |
| B4 | Backward-compat invariant | #7 | ≥ 1 sample ✓ |

Checkpoint #1b (post-`ModelStarted`, pre-`stream_chat`) is exercised indirectly by every scenario with a successful round (#3, #4, #6, #7); RESEARCH §8 documented this as an accepted sampling gap.

## Threat Surface Scan

No new network endpoints, auth paths, file access patterns, or schema changes at trust boundaries. The threat model in PLAN.md (T-15-03-FLAKE, T-15-03-DEADLOCK, T-15-03-SC) accurately enumerates the surface; nothing additional was introduced.

- **T-15-03-FLAKE (mitigate)** — All scenarios deterministic. `FakeStreamingLlmClient` emits synchronously; `CancelBetweenTokensLlm` and `CancelOnDoneLlm` use `futures::stream::unfold` with explicit ordering; no `sleep`, no real I/O. The only timing-dependent test is scenario #8 (timeout regression), which intentionally races the helper against a 50ms wallclock — completes in ~50ms.
- **T-15-03-DEADLOCK (mitigate)** — Every live-sender scenario drives via `drive_run_turn_live` which `tokio::join!`s run + drain. Sequential drain is forbidden by D-19. Capacity 32 amply exceeds the highest scenario event count (#3 emits ~12-14 events).
- **T-15-03-SC (accept)** — No new dependencies. Cargo.toml byte-identical.

## Known Stubs

None. Every test path is wired end-to-end:

- All helpers (`drive_run_turn_live`, `make_request`, `make_request_with_cancel`, `FakeToolExecutor`, `CancelOnFirstTool`, `CancelOnDoneLlm`, `CancelBetweenTokensLlm`, `PendingStreamLlm`) are used by at least one scenario.
- All scenario assertions execute against real `AgentLoop::run_turn_live` / `run_turn` calls — no mocking of the system under test.
- No `unimplemented!()`, `todo!()`, or `#[ignore]` markers anywhere in the file.

## Next Phase Readiness

- **Plan 15-04 (closeout):** the workspace gate (clippy + fmt + workspace test) plus STATE.md / ROADMAP.md / REQUIREMENTS.md updates is the final wave-2 task. Plan 15-03 leaves the workspace in a fmt-clean / clippy-clean / test-clean state; Plan 15-04 should run cleanly with no preexisting drift.
- **Phase 16 (public live stream):** the agent-side primitives (run_turn_live + AgentTurnEvent live mirror + StreamOutcome::Cancelled + five new cancel checkpoints) now have a behavioral lock at runtime; any regression in cancel timing, partial preservation, live-mirror fidelity, or buffered backward-compat will fail at least one Plan 15-03 scenario before the public API stream is even reachable.
- No blockers.

## Self-Check: PASSED

- `crates/lattice-agent/tests/live_sink_and_cancel.rs` — FOUND
- `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-03-SUMMARY.md` — FOUND (this file)
- commits `030fcaa`, `c327f01`, `40a85a0`, `e28f1a6`, `4839360`, `ddb4e9b`, `04416c3`, `605938c`, `e8c5121` — all present on branch `worktree-agent-a0b0290e96fb66e2d`
- `cargo test -p lattice-agent --features test-utils` — 51/51 passing post-final-commit

---
*Phase: 15-agentloop-live-sink-and-cooperative-cancellation*
*Completed: 2026-05-22*
