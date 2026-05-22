---
gsd_state_version: 1.0
milestone: v1.2-alpha
milestone_name: Evented Runtime
status: milestone_ready_to_complete
last_updated: 2026-05-22T13:24:02.063Z
last_activity: 2026-05-22 -- Phase 18 complete; milestone ready to close
progress:
  total_phases: 7
  completed_phases: 6
  total_plans: 18
  completed_plans: 18
  percent: 86
stopped_at: Phase 18 complete; milestone ready to close
---

# Planning State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-21)

**Core value:** A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.
**Current focus:** Milestone ready to close — pending /gsd:complete-milestone

## Current Position

Phase: 18
Plan: Not started
Status: Ready to complete — pending /gsd:complete-milestone
Last activity: 2026-05-22

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
- [Phase 16-01] `build_commit_input`, `TurnCommitContext`, `derive_terminal_event` extracted as pub(super) helpers in `execute.rs`; shared by buffered and live paths. tokio-stream added as workspace dep; tokio promoted to lattice-api runtime dep.
- [Phase 16-02] `stream_turn` is now a true live event stream: `prepare_live_turn` validates startup synchronously (returns Err before stream on failure), `run_live_turn_task` uses mandatory `tokio::select!` concurrent drain loop, terminal event derived from `AgentTurnResult.status` (not ModelFinished presence per WR-01). `stream_turn_with_deps` preserved as buffered test seam (D-01). Channel capacity 256 (D-17). Receiver drop does not cancel the turn.
- [Phase 16-03] 7 behavioral tests prove live path: startup failure as Err (API-04/05), event ordering, Completed/Error commit, commit failure as CommitFailed (COMM-03), commit stores delta only (COMM-01), branch population.
- [Phase 17 closeout] Timeout documentation now describes cooperative deadline coverage for stream creation, stream consumption, and tool execution; live channel backpressure remains an accepted alpha gap. Phase 17 validation/verification docs and stale live-stream findings were refreshed before Phase 18.
- [Phase 18] CLI `run` subcommand swapped from buffered `run_turn` to live `lattice_api::stream_turn` with per-event rendering (TokenDelta to stdout, tool/commit lifecycle to stderr), JSONL mode, accumulated `RunRenderState` for exit-code derivation, and `scripts/release-gate.sh` shipped as the VER-14 release gate.
- [Phase 18] REQUIREMENTS.md traceability closed for CLI-05/06/07/VER-14; ROADMAP.md marks Phase 18 complete with 3/3 plans; all v1.2-alpha requirements are now mapped Complete.
- [Phase 18] The v1.2-alpha milestone is ready to close — pending `/gsd:complete-milestone`.

### Blockers

(None)

### Todos

- Phase 17 accepted gap: `emit_agent_event` still awaits bounded live-channel `send`, so a receiver that stays alive but stops draining can apply backpressure outside the cooperative deadline. This is intentionally not widened in the review fix because deadline-aware event emission needs a separate live/result event-consistency contract. Future work should either make `emit_agent_event` deadline-aware or explicitly narrow/document timeout semantics around live observer backpressure.
- Phase 17: Reconcile `RunTurnStage` semantics with observed events/tool results or weaken/remove the field.
