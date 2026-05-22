# Roadmap: LATTICE

## Milestones

- ✅ **v1.1 Cut Off Cleverness** — Phases 6-11 (shipped 2026-05-20)
- 🔄 **v1.2-alpha Evented Runtime** — Phases 12-18 (planned 2026-05-21)

## Phases

<details>
<summary>✅ v1.1 Cut Off Cleverness (Phases 6-11) — SHIPPED 2026-05-20</summary>

- [x] Phase 6: Crate Topology and Main-Path Skeleton (3/3 plans) — completed 2026-05-16
- [x] Phase 7: Functional LLM and Direct Profiles (4/4 plans) — completed 2026-05-16
- [x] Phase 8: Session Graph Core (4/4 plans) — completed 2026-05-16
- [x] Phase 9: Agent Loop, Tools, and API Facade (4/4 plans) — completed 2026-05-16
- [x] Phase 10: CLI Cutover (6/6 plans) — completed 2026-05-17
- [x] Phase 11: Deletion, Budget, and Release Verification (8/8 plans) — completed 2026-05-17

</details>

<details open>
<summary>🔄 v1.2-alpha Evented Runtime (Phases 12-18) — PLANNED</summary>

- [x] Phase 12: Event and Status Vocabulary Contract — 2 plans
  Plans:
  - [x] 12-01-PLAN.md — Rewrite event/status type definitions, agent loop emissions, and mapping layer
  - [x] 12-02-PLAN.md — Update tests and CLI, add VER-07 verification tests
- [x] Phase 13: LLM Streaming Trait and Fake Streaming Client — 1/1 plans — completed 2026-05-21
  Plans:
  - [x] 13-01-PLAN.md — Add stream_chat to LlmClient with default fallback, ship lattice_agent::testing::FakeStreamingLlmClient, cover D-13.2..D-13.11b in tests/streaming_seam.rs
- [x] Phase 14: AgentLoop Streaming Delta Engine — 2/2 plans — completed 2026-05-21
  Plans:
  - [x] 14-01-PLAN.md — Stream consumption engine, tool call assembly, RealLlmClient stream_chat override
  - [x] 14-02-PLAN.md — 13 behavioral tests proving streaming delta engine (D-16 scenarios + D-17 regression)
- [x] Phase 15: AgentLoop Live Sink and Cooperative Cancellation — 4/4 plans — completed 2026-05-21
  Plans:
  - [x] 15-01-PLAN.md — Pure refactor: extract flush_partial_assistant_message + apply_optional_timeout helpers, derive PartialEq/Eq on AgentTurnEvent
  - [x] 15-02-PLAN.md — Atomic streaming + cancel + live-sink: emit_agent_event, cancel-aware consume_model_stream, StreamOutcome::Cancelled, 5 cancel checkpoints, run_turn_live entrypoint
  - [x] 15-03-PLAN.md — Behavioral suite: tests/live_sink_and_cancel.rs covering all 7 D-18 scenarios + D-21 timeout regression with concurrent-drain helper
  - [x] 15-04-PLAN.md — Workspace gate: cargo fmt + clippy + workspace test, close STATE/ROADMAP/REQUIREMENTS for AGENT-08, AGENT-09, VER-11
- [x] Phase 16: Live API Stream and Commit Transaction — 3/3 plans — completed 2026-05-22
  Plans:
  - [x] 16-01-PLAN.md — Add tokio-stream dep, extract build_commit_input/derive_terminal_event shared helpers
  - [x] 16-02-PLAN.md — Implement live path: PreparedTurn, prepare_live_turn, run_live_turn_task with select! drain, rewrite stream_turn
  - [x] 16-03-PLAN.md — Behavioral tests proving live stream_turn startup, ordering, commit, and failure
- [x] Phase 17: Failure Matrix and Run/Stream Consistency — 3/3 plans — completed 2026-05-22
  Plans:
  - [x] 17-01-PLAN.md — Cooperative deadline timeout fix replacing apply_optional_timeout
  - [x] 17-02-PLAN.md — Deterministic commit failure, receiver drop, and cancelled buffered seam tests
  - [x] 17-03-PLAN.md — Run/stream consistency integration tests with NormalizedTurnObservation
- [ ] Phase 18: CLI Live Rendering and Alpha Release Gate — planned

</details>

## Current Milestone Plan

| Phase | Goal | Requirements | Success Criteria |
|-------|------|--------------|------------------|
| 12. Event and Status Vocabulary Contract | Rename and stabilize the public/internal event vocabulary before changing execution behavior. | EVT-01, EVT-02, EVT-03, EVT-04, EVT-05, VER-07 | 4 |
| 13. LLM Streaming Trait and Fake Streaming Client | Introduce the LLM streaming seam needed by live turns while keeping existing call sites compiling. | LLM-06, LLM-07, LLM-08, LLM-09 | 4 |
| 14. AgentLoop Streaming Delta Engine | Make AgentLoop consume model streams and finalize deltas into transcript messages. | AGENT-03, AGENT-04, AGENT-05, AGENT-06, AGENT-07, VER-08, VER-09 | 5 |
| 15. AgentLoop Live Sink and Cooperative Cancellation | 4/4 | Complete    | 2026-05-21 |
| 16. Live API Stream and Commit Transaction | Implement real `stream_turn` with ordered public events and durable commit behavior. | COMM-01, COMM-02, COMM-03, COMM-04, COMM-05, API-04, API-05, API-06, API-07, API-08, API-09, API-10 | 5 |
| 17. Failure Matrix and Run/Stream Consistency | Prove terminal behavior, receiver drop, commit failure, and final `run_turn` semantics. | VER-10, VER-12, VER-13 | 4 |
| 18. CLI Live Rendering and Alpha Release Gate | Render public stream events in the CLI and run the milestone release gate. | CLI-05, CLI-06, CLI-07, VER-14 | 5 |

### Phase 12: Event and Status Vocabulary Contract

**Goal:** Stabilize the vocabulary so later streaming work does not carry ambiguous v1.1 event names.

**Requirements:** EVT-01, EVT-02, EVT-03, EVT-04, EVT-05, VER-07

**Success criteria:**
1. `AgentTurnStatus` and `RunTurnStatus` use `Error` instead of `Failed`, and `RunTurnStatus::Error` maps to `TurnCommitStatus::Error`.
2. Internal `AgentTurnEvent` has process events only: model, reasoning, token, tool, and model-finished events.
3. Public `TurnEvent` includes turn-start, terminal, committing, committed, and commit-failed events with a stable serialized discriminator.
4. Existing buffered stream tests are updated to the new names and compile failures catch removed variants.

### Phase 13: LLM Streaming Trait and Fake Streaming Client

**Goal:** Introduce the LLM streaming seam needed by live turns while keeping existing call sites compiling.

**Requirements:** LLM-06, LLM-07, LLM-08, LLM-09

**Success criteria:**
1. `LlmClient` exposes `stream_chat` returning `LlmStream`.
2. The default `stream_chat` implementation converts `ChatResponse` content, reasoning_content, tool_calls, finish_reason, and usage into reasoning, token, tool-call, and done events.
3. A fake streaming client can emit configurable event sequences for tests.
4. Existing non-streaming behavior and tests remain valid.

### Phase 14: AgentLoop Streaming Delta Engine

**Goal:** Move AgentLoop model execution from single response handling to streaming delta processing.

**Requirements:** AGENT-03, AGENT-04, AGENT-05, AGENT-06, AGENT-07, VER-08, VER-09

**Success criteria:**
1. AgentLoop emits token and reasoning deltas per chunk and accumulates final assistant output correctly.
2. Reasoning content is committed separately from visible assistant content.
3. Tool-call messages are finalized only after complete tool calls are assembled.
4. Tool execution remains sequential and failed tool results can still lead to a completed turn.
5. Stream errors preserve complete partial content and do not synthesize `ModelFinished`.

### Phase 15: AgentLoop Live Sink and Cooperative Cancellation

**Goal:** Add live event forwarding at the agent layer and enforce alpha cancellation boundaries.

**Requirements:** AGENT-08, AGENT-09, VER-11

**Success criteria:**
1. `run_turn` still returns a complete buffered `AgentTurnResult.events` vector.
2. `run_turn_live` sends the same event sequence to a provided bounded channel.
3. Dropping the live sender/receiver path does not fail the turn.
4. Cancellation before tools skips tool execution, while cancellation during a tool waits for the real tool result.

### Phase 16: Live API Stream and Commit Transaction

**Goal:** Make `stream_turn` a true live public event stream while preserving git-like commit semantics.

**Requirements:** COMM-01, COMM-02, COMM-03, COMM-04, COMM-05, API-04, API-05, API-06, API-07, API-08, API-09, API-10

**Success criteria:**
1. `stream_turn` performs startup validation and session/head preparation before returning `Ok(stream)`.
2. The live task uses one spawn, bounded channel capacity 256, and no nested bridge spawn.
3. Agent events are drained and mapped before terminal and commit events are emitted.
4. Completed, Error, and Cancelled turns commit transcript deltas and move HEAD.
5. Commit failure emits `CommitFailed`, emits no `TurnCommitted`, and leaves HEAD unchanged.

### Phase 17: Failure Matrix and Run/Stream Consistency

**Goal:** Close semantic gaps between live streaming, buffered execution, failure behavior, and session history.

**Requirements:** VER-10, VER-12, VER-13

**Success criteria:**
1. Tests cover error after partial tokens, cancellation paths, receiver drop, and commit failure.
2. `stream_turn` emits live activity before commit in at least one integration-style test.
3. Final status, usage, tool summaries, and transcript delta match between `run_turn` and `stream_turn` terminal paths.
4. HEAD movement is asserted for Completed, Error, CommitFailed, and Cancelled commit semantics via the buffered seam; public live cancellation remains deferred until `stream_turn` exposes a cancellation handle.

### Phase 18: CLI Live Rendering and Alpha Release Gate

**Goal:** Expose the live stream through the maintained CLI surface and validate the milestone end to end.

**Requirements:** CLI-05, CLI-06, CLI-07, VER-14

**Success criteria:**
1. `cargo run -p lattice-cli -- run ...` renders `TokenDelta` output as it arrives.
2. Tool activity is shown as compact status lines and reasoning remains hidden unless an explicit verbose/debug mode exists.
3. `TurnFailed` and `CommitFailed` are surfaced on stderr with clear terminal status.
4. Release verification runs targeted tests for changed crates plus formatting and workspace compile checks.
5. Planning traceability is updated before milestone completion.

## Progress

| Phase | Milestone | Plans Complete | Status | Completed |
|-------|-----------|---------------|--------|-----------|
| 6. Crate Topology | v1.1 | 3/3 | Complete | 2026-05-16 |
| 7. Functional LLM | v1.1 | 4/4 | Complete | 2026-05-16 |
| 8. Session Graph | v1.1 | 4/4 | Complete | 2026-05-16 |
| 9. Agent/Tools/API | v1.1 | 4/4 | Complete | 2026-05-16 |
| 10. CLI Cutover | v1.1 | 6/6 | Complete | 2026-05-17 |
| 11. Deletion/Verify | v1.1 | 8/8 | Complete | 2026-05-17 |
| 12. Event Vocabulary | v1.2-alpha | 2/2 | Complete | 2026-05-21 |
| 13. LLM Streaming | v1.2-alpha | 1/1 | Complete | 2026-05-21 |
| 14. Agent Streaming | v1.2-alpha | 2/2 | Complete | 2026-05-21 |
| 15. Agent Live Sink | v1.2-alpha | 4/4 | Complete | 2026-05-21 |
| 16. API Live Stream | v1.2-alpha | 3/3 | Complete | 2026-05-22 |
| 17. Failure Matrix | v1.2-alpha | 3/3 | Complete | 2026-05-22 |
| 18. CLI Live Rendering | v1.2-alpha | 0/0 | Planned | — |

---
*Roadmap updated for v1.2-alpha milestone initialization: 2026-05-21*
*Phase 16 plans drafted: 2026-05-22*
*Phase 17 plans drafted: 2026-05-22*
