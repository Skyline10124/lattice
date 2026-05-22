---
gsd_state_version: 1.0
milestone: v1.2-alpha
milestone_name: Evented Runtime
status: milestone_complete
last_updated: 2026-05-21T19:06:12.553Z
last_activity: 2026-05-21 -- Phase 15 execution started
progress:
  total_phases: 4
  completed_phases: 3
  total_plans: 9
  completed_plans: 9
  percent: 75
stopped_at: Milestone complete (Phase 15 was final phase)
---

# Planning State

## Project Reference

See: .planning/PROJECT.md (updated 2026-05-21)

**Core value:** A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.
**Current focus:** Milestone complete

## Current Position

Phase: 15
Plan: Not started
Status: Milestone complete
Last activity: 2026-05-21

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

### Blockers

(None)

### Todos

- Phase 14: AgentLoop Streaming Delta Engine.
- Phase 15: Implement cooperative cancellation checkpoints deferred from Phase 14: before `stream_chat`, after each `LlmEvent` chunk, after model `Done` before tool execution, before each tool starts, and after each tool finishes before the next model round.
- Phase 14/17: Preserve partial progress on timeout via cooperative deadline or streaming loop state.
- Phase 17: Reconcile `RunTurnStage` semantics with observed events/tool results or weaken/remove the field.
- Run `cargo fmt --all` during Phase 14 to clean up pre-existing drift in `crates/lattice-llm/src/client.rs:289` (logged in `.planning/phases/12-event-and-status-vocabulary-contract/deferred-items.md`).
