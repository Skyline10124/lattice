---
phase: 15-agentloop-live-sink-and-cooperative-cancellation
plan: 02
subsystem: agent
tags: [agent-loop, live-sink, cancellation, mpsc, streaming, atomic-commit]

# Dependency graph
requires:
  - phase: 14-agentloop-streaming-delta-engine
    provides: AgentLoop streaming-delta engine, all-or-nothing tool validation, StreamOutcome { Done, Error, Exhausted }
  - phase: 15-01
    provides: flush_partial_assistant_message helper (D-05), apply_optional_timeout helper (D-10), AgentTurnEvent: PartialEq derive (D-15b)
provides:
  - "pub async fn AgentLoop::run_turn_live(request, sender) -> AgentTurnResult (D-09)"
  - "emit_agent_event free fn — universal AgentTurnEvent emission path with optional live mpsc mirror (D-12 / D-13)"
  - "StreamOutcome::Cancelled variant with accumulated content preservation (D-01)"
  - "consume_model_stream cancel-aware via cancel_flag: Option<&Arc<AtomicBool>> + post-event check (D-02 / D-14)"
  - "run_turn_inner widened with live_events: Option<&Sender> threading + Cancelled match arm (D-04 / D-10)"
  - "Five new cancel checkpoints (1b, 2, 3, 4, 5) in run_turn_inner / consume_model_stream (D-06)"
affects: [15-03]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Atomic compile-coupled commit: variant + match arm + signature widening land together so cargo check stays exhaustive"
    - "Best-effort live mirror via let _ = sender.send(event).await — receiver-drop tolerated, backpressure awaited (no try_send)"
    - "Cancel checkpoint placed after each emission so the event causing the cancel decision is always observed by both sinks before exit"
    - "Dual entrypoint shape — run_turn (None) + run_turn_live (Some(&sender)) sharing one apply_optional_timeout(...) wrapper around run_turn_inner"

key-files:
  created: []
  modified:
    - crates/lattice-agent/src/agent_loop/mod.rs

key-decisions:
  - "Live sender is moved into run_turn_live (not cloned) — RAII drop on return closes the rx for tests / consumers"
  - "Checkpoint #5 clones stream_result.assistant_content because subsequent loop iterations can still consume it; the Cancelled stream-arm at #2 moves it (return is the only consumer)"
  - "AtomicBool was added to use std::sync::atomic alongside Ordering — consume_model_stream signature names the type directly rather than re-aliasing through a local"
  - "Did not split into multiple commits per task — RESEARCH §4 / §7 + PATTERNS Compile-time Coupling Map require atomicity (StreamOutcome variant + match arm + signature widening at the type level). One commit, six tasks executed in-place."
  - "Did not add the optional dedicated test for checkpoint #1b — RESEARCH §8 documented sampling gap accepted; the path runs on every successful round and Plan 15-03 will exercise it indirectly via every other live-mirror scenario"

requirements-completed: [AGENT-08, AGENT-09]

# Metrics
duration: ~22min
completed: 2026-05-22
---

# Phase 15 Plan 02: AgentLoop live sink + cooperative cancellation Summary

**Atomic Wave 1 commit — AgentLoop now exposes `run_turn_live` mirroring events onto an `mpsc::Sender`, observes caller cancellation at five new checkpoints across `run_turn_inner` + `consume_model_stream`, preserves partial content via the new `StreamOutcome::Cancelled` arm, and routes every emission through a single `emit_agent_event` helper.**

## Performance

- **Duration:** ~22 min
- **Started:** 2026-05-21T18:31:45Z
- **Completed:** 2026-05-21T18:54:00Z
- **Tasks:** 6 (executed atomically per plan directive — single commit)
- **Files modified:** 1

## Accomplishments

- **`pub async fn run_turn_live(&self, request, events: Sender<AgentTurnEvent>)`** added next to `run_turn` (D-09). Both methods share `apply_optional_timeout(&request, run_turn_inner(.., live_events))`; the only difference is `None` vs `Some(&events)` for the live channel. The unknown-tools error path uses the same shape as `run_turn` so the early-error branch returns identically across both entrypoints.
- **`emit_agent_event(buffered, live, event)`** is now the universal emission path. Body: `buffered.push(event.clone()); if let Some(sender) = live { let _ = sender.send(event).await; }`. The trailing `let _ = ... .await` swallows `SendError` (receiver drop) silently and awaits backpressure on a full bounded channel — no `try_send`. Every former `all_events.push(...)` site (5 in `run_turn_inner`: ModelStarted, ToolCallStarted, ModelFinished×2, ToolCallFinished) and every former `events.push(...)` site (2 in `consume_model_stream`: TokenDelta, ReasoningDelta) routes through it.
- **`StreamOutcome::Cancelled`** variant added (D-01). The match in `run_turn_inner` now covers all four variants exhaustively (Done | Error(_) | Cancelled | Exhausted). The Cancelled arm flushes partial assistant + reasoning content via the Plan 15-01 `flush_partial_assistant_message` helper and returns `status: AgentTurnStatus::Cancelled, terminal_reason: Some("cancelled by caller")` — no synthesized `ModelFinished` event (D-04 / D-08).
- **`consume_model_stream` widened** to four parameters (D-02 / D-14): `mut stream`, `events: &mut Vec<AgentTurnEvent>`, `live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>`, `cancel_flag: Option<&Arc<AtomicBool>>`. The post-event cancel checkpoint (#2) runs after each non-`Done` `LlmEvent` match arm body (after the `emit_agent_event` and after pending-call mutations) and before the next `stream.next().await`. Done remains a non-cancel point (D-03). Malformed-toolcall `Error(...)` early-returns above the cancel check are unchanged.
- **Five new cancel checkpoints** land at exactly the sites D-06 / PATTERNS specify, all loading the flag via `Ordering::Relaxed` and returning the same `"cancelled by caller"` terminal reason consistent with the pre-existing round-top check (#1):
  - **#1b** — after the awaited `emit_agent_event(.., ModelStarted)`, before `stream_chat`. Returns `Cancelled` with empty assistant_output (no partial — stream never opened).
  - **#2** — inside `consume_model_stream`, after each non-`Done` LlmEvent (post-emit, pre-next-`stream.next()`).
  - **#3** — after the assistant tool-call message is committed to `messages`/`all_messages`, before the `for call in &validated_calls` loop. Transcript keeps the assistant tool-call message; no tool result messages (D-08).
  - **#4** — at the top of the per-tool loop body, before `emit_agent_event(.., ToolCallStarted)`. Skips remaining tools; prior tools' events / messages / summaries preserved.
  - **#5** — after the tool message is pushed to `messages`/`all_messages`, before the next iteration. Already-running tool's real result is recorded; in-flight tool is never aborted (D-07).
- **Compile invariants maintained** in a single commit. `cargo check`, `cargo build`, `cargo clippy --features test-utils -- -D warnings`, and `cargo test --features test-utils` all pass with the entire change set landed atomically.

## Task Commits

Tasks 1–6 land atomically in a single commit per plan directive (RESEARCH §4 / §7 + PATTERNS Compile-time Coupling Map require simultaneous landing of the variant + match arm + signature widening to keep `cargo check` green):

1. **Task 1: emit_agent_event helper + StreamOutcome::Cancelled variant** — `ababdb8` (feat)
2. **Task 2: Widen consume_model_stream with live + cancel_flag + checkpoint #2** — `ababdb8` (feat)
3. **Task 3: Widen run_turn_inner + Cancelled arm + push→emit migration** — `ababdb8` (feat)
4. **Task 4: Add cancel checkpoints 1b / 3 / 4 / 5** — `ababdb8` (feat)
5. **Task 5: Add pub async fn run_turn_live entrypoint** — `ababdb8` (feat)
6. **Task 6: Verify + commit** — `ababdb8` (feat)

Plan-level commit message:
`feat(phase-15): live event sink + cooperative cancellation in AgentLoop`

## Files Created/Modified

- `crates/lattice-agent/src/agent_loop/mod.rs` — +305 / −30 (335-line diff). Added: `pub async fn run_turn_live` next to `run_turn`; widened `run_turn_inner` signature with `live_events: Option<&Sender>`; widened `consume_model_stream` signature with `live` + `cancel_flag`; new `StreamOutcome::Cancelled` variant + Cancelled match arm; `emit_agent_event` async free fn at module scope (next to the Plan 15-01 helpers); five new cancel checkpoints at the sites D-06 specifies; replaced 5 `all_events.push(..)` + 2 `events.push(..)` sites with awaited `emit_agent_event(..)` calls; threaded `request.cancel_flag.as_ref()` into the `consume_model_stream` call. Added `AtomicBool` to the `std::sync::atomic` import group (was previously importing `Ordering` only) so the new signature can name the type directly.

## Decisions Made

- **Atomic single-commit** — Tasks 1–6 land as one commit per plan directive. Splitting them would break `cargo check` mid-sequence: introducing the new variant without the match arm makes the match non-exhaustive; widening `consume_model_stream` without updating the lone caller is a type error; widening `run_turn_inner` without a `Some(&sender)` caller leaves dead code that clippy would flag. RESEARCH §4 / §7 + PATTERNS "Compile-time Coupling Map" justify the atomicity.
- **`AtomicBool` re-import** — Added `AtomicBool` to the existing `use std::sync::atomic::Ordering;` import (now `use std::sync::atomic::{AtomicBool, Ordering};`) so `consume_model_stream`'s `cancel_flag: Option<&Arc<AtomicBool>>` parameter can name the type directly. The `types.rs` `AgentTurnRequest::cancel_flag` field already imports `AtomicBool` from the same path, so the dependency exists at workspace scope.
- **Checkpoint #5 clones stream_result.assistant_content** — unlike the Cancelled stream-arm (#2 path) which is the only consumer of `stream_result.assistant_content` and can move it, checkpoint #5 sits inside the per-tool loop where the surrounding round may continue if the cancel flag is not set; subsequent iterations also reference `stream_result.assistant_content`. The `.clone()` is unavoidable here. (Same reasoning applies to checkpoints #3 and #4.)
- **Sender ownership in `run_turn_live`** — the Sender is moved in (not borrowed) so its drop on function return closes the receiver. Tests rely on this RAII close to terminate `rx.recv()` loops in concurrent-drain patterns. `apply_optional_timeout` borrows `Some(&events)` so the Sender is *not* moved into the inner future — it stays alive for the timeout-elapse path too.
- **Did not add the optional checkpoint-#1b regression test** — RESEARCH §8 documented this as an accepted sampling gap. The same code path runs on every successful round in Plan 15-03's scenarios #3, #4, #6, #7, so a regression at #1b breaks every other live-mirror test indirectly.
- **No `cargo fmt`** — explicitly excluded by Task 6 directive ("Plan 15-04 owns the workspace fmt gate").

## Deviations from Plan

None. Plan executed exactly as written. Each acceptance criterion verified:

- `pub async fn run_turn_live` — present at mod.rs:126 with the exact `request: AgentTurnRequest, events: tokio::sync::mpsc::Sender<AgentTurnEvent>` signature.
- `emit_agent_event` — present at mod.rs:931 with `buffered: &mut Vec<AgentTurnEvent>, live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>` and a 4-line body containing `let _ = sender.send(event).await;`.
- `StreamOutcome::Cancelled` — variant declared at mod.rs:649; produced at mod.rs:861 (consume_model_stream cancel return); consumed at mod.rs:355 (run_turn_inner Cancelled arm).
- `flag.load(Ordering::Relaxed)` — 6 occurrences (round-top + 1b + 2 + 3 + 4 + 5).
- `status: AgentTurnStatus::Cancelled` — 6 occurrences (round-top + 1b + 3 + 4 + 5 + Cancelled stream-arm).
- `all_events.push(` — 0 occurrences (every site rewritten to `emit_agent_event(...)`).
- `events.push(AgentTurnEvent::TokenDelta` / `events.push(AgentTurnEvent::ReasoningDelta` — 0 occurrences each (rewritten to `emit_agent_event(events, live, ...).await`).
- `cargo build -p lattice-agent` — clean (172 crates, no errors, no new warnings).
- `cargo clippy -p lattice-agent --all-targets --features test-utils -- -D warnings` — clean.
- `cargo test -p lattice-agent --features test-utils` — 43 / 43 passing across 5 suites (lib, agent_loop, streaming_delta, streaming_seam, doc-tests). No Phase 13/14 regressions.

## Issues Encountered

One transient build error during execution: the new `consume_model_stream` signature named `AtomicBool` directly but the file previously only imported `Ordering` from `std::sync::atomic`. Fixed by widening the import to `{AtomicBool, Ordering}`. Caught by `cargo build` on first run; resolved with one edit; total turnaround under 30 seconds. No deviation from plan — same fix is implicit in any reading of `consume_model_stream`'s post-Phase-15 signature.

## User Setup Required

None — no external service configuration, no new dependencies, no config changes.

## Verification

- `cargo build -p lattice-agent` — clean (no errors, no new warnings beyond pre-Phase-15 baseline).
- `cargo check -p lattice-agent` — clean.
- `cargo clippy -p lattice-agent --all-targets --features test-utils -- -D warnings` — clean.
- `cargo test -p lattice-agent --features test-utils` — 43 / 43 passing across 5 suites.
- Critical regression tests verified via the full suite:
  - Phase 14 streaming-delta engine tests still green (no behavioral change to Done / Error / Exhausted paths; the new `Cancelled` arm is unreachable until Plan 15-03 wires a cancel flag through tests).
  - `agent_loop_cancel_returns_cancelled_status` (existing round-top cancel test) still green — round-top check is unchanged.
  - `agent_loop_timeout_returns_failed_status` still green — `apply_optional_timeout` shape preserved.
- Compile-time invariants from RESEARCH §7 enforced: every match-on-`StreamOutcome` site covers all four variants exhaustively; all `consume_model_stream` callers pass the new four-argument shape; `run_turn`'s call to `run_turn_inner` passes `None` for `live_events`; `run_turn_live`'s call passes `Some(&events)`.

## Threat Surface Scan

No new network endpoints, auth paths, file access patterns, or schema changes at trust boundaries. The threat model in PLAN.md (T-15-02-DOS, T-15-02-INFO, T-15-02-CANCEL, T-15-02-SC) accurately enumerates the surface; nothing additional was introduced.

## Known Stubs

None. Every new code path is wired end-to-end:
- `emit_agent_event` is called at every emission site.
- `StreamOutcome::Cancelled` is produced (consume_model_stream) and consumed (run_turn_inner).
- `run_turn_live` routes through the same shared scaffolding as `run_turn` (tools resolution, timeout wrapper, run_turn_inner).
- All five new checkpoints have working flag loads + early returns; no TODOs or `unimplemented!()` markers.

Behavioral test coverage (Plan 15-03) is the next plan in the wave; the production wiring shipped here is complete.

## Next Phase Readiness

- **Plan 15-03 (behavioral tests):** can now write `tests/live_sink_and_cancel.rs` with all seven scenarios. The new entrypoint `run_turn_live` is callable; `AgentTurnEvent: PartialEq` (Plan 15-01) lets scenario #3 use `assert_eq!`; the cancel-aware `consume_model_stream` will exercise checkpoint #2; checkpoints #3, #4, #5 are reachable via the existing `FakeStreamingLlmClient` + a `CancelOnFirstTool`-style executor wrapper. The recommended `tokio::join!(run, drain)` recipe (RESEARCH §5) applies as documented.
- **Plan 15-04 (closeout):** the workspace fmt gate Plan 15-04 owns will reformat the multi-line `emit_agent_event(...)` calls if `rustfmt` prefers a different layout; the current layout is rustfmt-stable but Plan 15-04 will catch any drift.
- **Phase 16 (public live stream / API-04..API-10):** the agent-side primitives (run_turn_live, AgentTurnEvent live mirror, StreamOutcome::Cancelled) are ready to plug into a public TurnEvent stream. No further changes to AgentLoop should be needed for Phase 16's seam.
- No blockers.

## Self-Check: PASSED

- `crates/lattice-agent/src/agent_loop/mod.rs` — FOUND
- `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-02-SUMMARY.md` — FOUND
- commit `ababdb8` — FOUND on branch `worktree-agent-a3fbf69a43342020d`
- `cargo test -p lattice-agent --features test-utils` — 43/43 passing post-commit

---
*Phase: 15-agentloop-live-sink-and-cooperative-cancellation*
*Completed: 2026-05-22*
