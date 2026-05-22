---
gsd_state_version: 1.0
milestone: v1.2-alpha
milestone_name: Evented Runtime
status: in_progress
last_updated: 2026-05-22T02:30:00.000Z
last_activity: 2026-05-22 -- Phase 15 complete; ready for Phase 16
progress:
  total_phases: 7
  completed_phases: 4
  total_plans: 9
  completed_plans: 9
  percent: 57
stopped_at: Phase 15 complete; ready for Phase 16
---

# Planning State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-21)

**Core value:** A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.
**Current focus:** Phase 16 — Live API Stream and Commit Transaction

## Current Position

Phase: 16
Plan: Not started
Status: Ready for planning
Last activity: 2026-05-22 -- Phase 15 complete

## Accumulated Context

### Decisions

- v1.2-alpha follows `.planning/alpha-v1.2-evented-runtime-design.md`.
- Continue phase numbering from v1.1; this milestone starts at Phase 12.
- Events are live observations, not durable history; commits store transcript deltas and terminal metadata.
- `lattice-api` remains a thin facade and transaction boundary; no runtime crate is introduced in alpha.
- `stream_turn` live path bypasses `TurnDeps`; `TurnDeps` remains the buffered test seam.
- [Phase 12-01] AgentTurnEvent reduced to six process-only variants; AgentTurnStatus and RunTurnStatus rename Failed -> Error.
- [Phase 12-01] TurnEvent expanded to 12-variant tagged contract via `#[serde(tag = "type", rename_all = "snake_case")]`; API mapping derives TurnFailed/TurnCancelled from AgentTurnResult.status.
- [Phase 12-02] Cancel test no longer inspects events for terminal variants; the agent vocabulary has none, so the assertion was dropped instead of forced.
- [Phase 12-02] stream_turn_error_still_commits_error pins TurnFailed-before-TurnCommitted ordering, locking the contract introduced by Plan 12-01's execute.rs change.
- [Phase 12-02] VER-07 verification uses `serde_json::to_value` plus `json["type"]` equality to pin the snake_case discriminator shape, with one Debug-format compile guard.
- [Phase 12 repair] Routed provider metadata now flows from API model resolution into `AgentTurnRequest.provider` and `ModelStarted.provider`; `ProviderKind::as_str()` provides stable commit/event strings.
- [Phase 13-01] `LlmClient` now exposes `stream_chat` with a default buffered fallback that emits reasoning, token, tool-call, and done events without synthesizing provider stream-open events.
- [Phase 13-01] `lattice_agent::testing::FakeStreamingLlmClient` is available only under `feature = "test-utils"` and preserves the two-channel error model: startup failures as `Result::Err`, mid-stream failures as `LlmEvent::Error`.
- [Phase 13-01] AgentLoop, RealLlmClient, `lattice-llm` source, public `TurnEvent`, and `stream_turn` remained unchanged; those consumer changes start in Phase 14+.
- [Phase 15-01] Extracted `flush_partial_assistant_message` and `apply_optional_timeout` helpers in `agent_loop/mod.rs`; derived `PartialEq` (not `Eq`) on `AgentTurnEvent` because `TurnUsage` in `lattice-session` is `PartialEq`-only.
- [Phase 15-02] `emit_agent_event` is the single emission seam; `live_events: Option<&Sender<AgentTurnEvent>>` threaded through `run_turn_inner`; sender drop is fire-and-forget via `let _ = sender.send(event).await`. Five cancel checkpoints (round-top + 1b + 2 + 3 + 4 + 5) all use `flag.load(Ordering::Relaxed)` and return `terminal_reason: Some("cancelled by caller")`. `StreamOutcome::Cancelled` is the cancel-aware return from `consume_model_stream`.
- [Phase 15-02] `run_turn_live` mirrors `run_turn` buffered output onto a bounded `mpsc::Sender<AgentTurnEvent>` while still returning the full `AgentTurnResult.events` vector. Receiver drop does not fail the turn; cancel-during-tool waits for the real tool result before observing the flag.
- [Phase 15 addendum] Cancel checkpoints #3 and #4 fire **after** `ModelFinished` has been emitted (model finished cleanly, then cancel observed before/between tools), so a `Cancelled` `AgentTurnResult.events` vector may legitimately contain a trailing `ModelFinished`. Phase 16 event mapping MUST derive terminal `TurnEvent` status from `AgentTurnResult.status`, never from the presence/absence of `ModelFinished` in the buffered events. See `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-ADDENDUM.md` (WR-01).
- [Phase 15 addendum] `AgentTurnEvent` derives `PartialEq` only, not `PartialEq, Eq` (CONTEXT/RESEARCH/PATTERNS describe `Eq` aspirationally). `TurnUsage` in `lattice-session` is `PartialEq`-only and widening it is out of Phase 15 scope. Tests use `assert_eq!`, which only needs `PartialEq`; if a future need for `Eq` arises, the right scope is `lattice-session`, not `agent_loop/types.rs`. See 15-ADDENDUM.md (IN-01).

### Blockers

(None)

### Todos

- Phase 16: Live API Stream and Commit Transaction.
- Phase 14/17: Preserve partial progress on timeout via cooperative deadline or streaming loop state.
- Phase 17: Timeout may drop in-flight `run_turn_live` after live events have already been sent, while returned `AgentTurnResult` uses the legacy empty timeout shape (zero usage, empty messages, empty events). Phase 17 must reconcile timeout partial preservation **and** live/result event consistency — fixing only transcript partial without locking `live_received == result.events` would leave a silent observability gap. Scenario 08 in `crates/lattice-agent/tests/live_sink_and_cancel.rs` currently asserts only the result shape; extend it (or add a sibling) to assert received-event consistency once the reconciliation lands.
- Phase 17: Reconcile `RunTurnStage` semantics with observed events/tool results or weaken/remove the field.
