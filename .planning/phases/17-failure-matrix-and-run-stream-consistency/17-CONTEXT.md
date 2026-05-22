# Phase 17: Failure Matrix and Run/Stream Consistency - Context

**Gathered:** 2026-05-22
**Status:** Ready for planning

<domain>
## Phase Boundary

Prove correctness of the streaming infrastructure built in Phases 14-16 through comprehensive behavioral tests. Close semantic gaps between `run_turn` (buffered) and `stream_turn` (live) paths, and fix the timeout partial-preservation defect so that live events and returned results cannot diverge.

This phase delivers:

1. Tests covering error after partial tokens, cancellation paths (agent + API buffered layer), receiver drop, and commit failure.
2. `stream_turn` emits live activity before commit in at least one integration-style test.
3. Final status, usage, tool summaries, and transcript delta match between `run_turn` and `stream_turn` terminal paths (excluding Cancelled on live public path, which is unreachable in alpha).
4. HEAD movement is asserted for Completed, Error, Cancelled (buffered), and CommitFailed cases.
5. Timeout reconciliation: `run_turn_live` timeout no longer produces a split between emitted live events and returned result.

### Hard boundary

```text
Phase 17 = behavioral tests + timeout reconciliation + run/stream consistency assertions.
No public cancellation handle on stream_turn (FUT-06, post-alpha).
No public API-level timeout field (RunTurnRequest has no timeout field).
No TUI/CLI work.
No changes to TurnEvent vocabulary or stream_turn public signature.
stream_turn_with_deps remains unchanged.
```

</domain>

<decisions>
## Implementation Decisions

### Commit failure injection (VER-12)
- **D-01:** Use real SQLite parent conflict to prove commit failure, not a `FailingSessionApi` wrapper or mock. `commit_turn_inner` checks `input.parent_ids[0]` against current HEAD in a transaction — parent mismatch returns `SessionError::Conflict` and rolls back.
- **D-02:** Deterministic conflict pattern: (1) create session, commit first turn to establish parent HEAD; (2) start second `stream_turn` with `BlockingLlmClient` that pauses mid-stream; (3) `prepare_live_turn` reads stale parent (guaranteed: `stream_turn(...).await` returning `Ok(stream)` means `prepare_live_turn` has completed); (4) `BlockingLlmClient` blocks agent before commit — test thread writes racing commit via separate `SessionApi`, advancing HEAD; (5) release `BlockingLlmClient`, let second turn reach commit; (6) assert `CommitFailed` in stream, HEAD = first + racing commits only. Two synchronization points: (a) stream returned = prepare complete with stale parent, (b) BlockingLlmClient blocks until test thread signals "racing commit inserted".
- **D-03:** Phase 16's existing commit failure test (`stream_turn_commit_failure` or equivalent) has race condition — fast fake LLM may complete before racing commit is inserted. Phase 17 **replaces** that test with the deterministic blocking pattern. The racy test is removed, not left alongside.

### API-level cancellation scope (VER-10, COMM-02)
- **D-04:** Cancelled public-live-path generation is unreachable in alpha because `stream_turn` exposes no cancellation handle (`cancel_flag: None` always). Phase 17 does NOT introduce a hidden cancel flag or internal test entry point.
- **D-05:** Cancelled commit behavior is tested at two layers:
  - **Agent layer:** Phase 15's `live_sink_and_cancel.rs` already proves `AgentTurnStatus::Cancelled` preserves events and returns proper result.
  - **API buffered seam:** Use `stream_turn_with_deps` (preferred over `run_turn_with_deps`) with a fake `TurnDeps` that returns `AgentTurnStatus::Cancelled`. This exercises `derive_terminal_event` and verifies: `TurnCancelled` emitted, `TurnCommitting` emitted, `TurnCommitted.status == Cancelled`, commit input status maps correctly, terminal_reason preserved. `run_turn_with_deps` only verifies final `RunTurnResult.status` and commit input — it misses public terminal event mapping.
- **D-06:** Context for traceability: "Cancelled public-live-path generation is unreachable in alpha because stream_turn exposes no cancellation handle. Phase 17 covers Cancelled at agent layer and API buffered/mapping layer; public stream cancellation remains FUT-06."

### Run/stream consistency assertion (VER-13)
- **D-07:** Same FakeLlm + FakeToolExecutor script, two independent tempdirs/sessions. Do NOT share DB or session between runs.
- **D-08:** Comparison uses a normalized observation struct that strips unstable fields (session_id, commit_id, timestamps):
  ```rust
  struct NormalizedTurnObservation {
      status: RunTurnStatus,
      usage: Option<TurnUsageSummary>,
      assistant_output: String,
      tool_results: Vec<ToolResultSummary>,
      transcript_roles_and_content: Vec<(String, String)>,
      terminal_reason: Option<String>,
  }
  ```
- **D-09:** Observation sources must be explicit per field:
  - **status:** run side from `RunTurnResult.status`; stream side from `TurnCommitted.status`.
  - **usage:** from committed metadata, normalized.
  - **assistant_output:** from committed transcript assistant visible content (both sides read session store after commit). Do NOT reconstruct from TokenDelta events — durable history is the consistency anchor.
  - **tool_results:** run side from `RunTurnResult.tool_results`; stream side from committed tool messages (or `ToolCallFinished` events where equivalent).
  - **terminal_reason:** from committed metadata on both paths; Error stream paths also verify public `TurnFailed.reason` matches the committed reason.
  - **transcript_roles_and_content:** both sides read session log after commit, compare committed message sequence (role, content).
- **D-10:** No snapshot testing. Structured `assert_eq!` on `NormalizedTurnObservation` is clearer than snapshot with masking.
- **D-11:** Minimum three consistency scenarios: (1) Completed content-only, (2) Error after partial token, (3) Tool call path with `ToolCallFinished` and tool messages. Tool consistency scenario must build identical `LoadedAgentProfile` / enabled tool config for both `run_turn` and `stream_turn`, and use the same scripted `FakeToolExecutor` with deterministic outputs.

### Timeout reconciliation
- **D-12:** The timeout defect is fixed in Phase 17. The old outer `tokio::time::timeout` behavior dropped in-flight state; the current cooperative deadline preserves accumulated events/messages.
- **D-13:** Fix approach: replace outer `tokio::time::timeout(run_turn_inner)` wrapper with a cooperative deadline that covers `stream_chat(request).await` (stream creation), subsequent `stream.next()` calls, and `ToolCallExecutor::execute`. If timeout fires before the stream is created, return Error("timeout") with events emitted so far (usually just ModelStarted). If timeout fires during stream consumption or tool execution, preserve accumulated partial content/reasoning and do not synthesize completion events that did not happen.
- **D-14:** Timeout reconciliation scope is limited to agent-internal LLM/tool await points. No public API timeout field is added to `RunTurnRequest`. No complex in-flight tool kill semantics, and live channel backpressure remains an accepted alpha gap.
- **D-15:** Agent-layer timeout test asserts: (1) received live events == result.events (same events, same order — test must concurrently drain receiver, never await result before reading events); (2) status == Error; (3) terminal_reason contains "timeout"; (4) partial content preserved in `AgentTurnResult.messages` (not transcript — transcript is an API-layer commit concern, and public `RunTurnRequest` has no timeout field in alpha).

### Claude's Discretion
- File organization for consistency test helpers (inline in test file vs shared test module).
- Naming of `BlockingLlmClient` or equivalent synchronization primitive.
- Whether `NormalizedTurnObservation` is a test-only struct or a `#[cfg(test)]` utility in the crate.
- Whether to introduce a `test-utils` feature on `lattice-api` for shared test infrastructure, or keep helpers per-file.
- Internal naming of the cooperative deadline mechanism (e.g., `DeadlineGuard`, remaining-duration threading, or `select!` with `tokio::time::sleep`).
- Doc comments on new test helpers.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Design Source
- `.planning/alpha-v1.2-evented-runtime-design.md` — §17 (run_turn/stream_turn relationship, TurnDeps as buffered seam, short-term vs long-term unification), §18 (receiver drop, bounded channel, cancellation separation), §19 (commit input construction)

### Requirements
- `.planning/REQUIREMENTS.md` — VER-10, VER-12, VER-13, COMM-02

### API Turn Layer (primary test target)
- `crates/lattice-api/src/turn/live.rs` — `stream_turn_live`, `prepare_live_turn`, `run_live_turn_task`, `emit_turn_event`
- `crates/lattice-api/src/turn/execute.rs` — `run_turn_with_deps`, `run_turn`, `build_commit_input`, `derive_terminal_event`, `TurnCommitContext`
- `crates/lattice-api/src/turn/types.rs` — `TurnEvent`, `TurnEventStream`, `RunTurnRequest`, `RunTurnResult`, `RunTurnStatus`, `TurnUsageSummary`
- `crates/lattice-api/src/turn/mapping.rs` — `map_status`, `map_tool_results`
- `crates/lattice-api/tests/usage_turn.rs` — existing API-level tests (test infrastructure to extend)

### Agent Layer (consumed for cancel/timeout)
- `crates/lattice-agent/src/agent_loop/mod.rs` — `run_turn_live`, `run_turn`, timeout handling (modification site for D-13)
- `crates/lattice-agent/src/agent_loop/types.rs` — `AgentTurnEvent`, `AgentTurnResult`, `AgentTurnStatus`, `AgentTurnRequest`
- `crates/lattice-agent/tests/live_sink_and_cancel.rs` — Phase 15 cancel tests (reference, not modified)

### Session Layer (commit conflict mechanism)
- `crates/lattice-session/src/graph.rs` — `commit_turn_inner` parent validation, `SessionError::Conflict`
- `crates/lattice-api/src/session.rs` — `SessionApi::commit_turn`

### Prior Phase Context
- `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-CONTEXT.md` — cancel checkpoints, `run_turn_live`, sender-drop tolerance
- `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-ADDENDUM.md` — WR-01 (terminal from status), IN-01 (PartialEq only)
- `.planning/phases/16-live-api-stream-and-commit-transaction/16-CONTEXT.md` — live path architecture, D-20 select! loop, commit failure handling

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `FakeLlm` in `usage_turn.rs` — buffered fake LLM; extend or create a streaming variant for consistency tests.
- `FakeStreamingLlmClient` in `lattice-agent testing` (`feature = "test-utils"`) — two-channel error model (startup Err, mid-stream LlmEvent::Error).
- `setup_workspace` / `build_agent_loop` helpers in `usage_turn.rs` — tempdir + profile + agent loop construction.
- `build_commit_input` / `derive_terminal_event` in `execute.rs` — shared helpers to test in isolation for Cancelled path.
- `SessionApi::sqlite(&path)` — trivial SQLite session store construction for test isolation.

### Established Patterns
- Phase 15 tests use `drive_run_turn_live` helper + channel collection pattern; Phase 17 API tests follow the same shape but at the `stream_turn` level.
- `let _ = sender.send(event).await` fire-and-forget — receiver drop tests verify this doesn't panic or fail the turn.
- `futures::StreamExt::collect::<Vec<_>>()` to drain stream_turn events in tests.

### Integration Points
- `crates/lattice-agent/src/agent_loop/mod.rs` — timeout mechanism refactor (D-13). Current outer `tokio::time::timeout` wrapping to be replaced with cooperative deadline.
- `crates/lattice-api/tests/` — new test file(s) for Phase 17 behavioral tests.
- Phase 16's `stream_turn_error_still_commits_error` and `stream_turn_commit_failure` tests — may be superseded or extended by deterministic commit failure test.

</code_context>

<specifics>
## Specific Ideas

- `BlockingLlmClient`: a streaming LLM fake that yields initial events (ModelStarted, TokenDelta) then blocks on a `tokio::sync::Notify` or `oneshot::Receiver` before emitting done. This gives the test thread deterministic control over when the agent loop progresses past model consumption.
- Commit failure test must verify not just `CommitFailed` presence, but that HEAD points to `first + racing` commits only, confirming rollback.
- For consistency tests, read session log after both runs and compare transcript messages (role, content) after stripping IDs. This proves the durable history matches regardless of which execution path produced it.
- Timeout cooperative deadline: `select!` between `stream.next()` and `tokio::time::sleep(remaining)` inside the stream consumption loop. On timeout arm: set `timed_out = true`, break out, run partial-flush, return Error status with accumulated events.
- `RunTurnStage` field reconciliation (from STATE.md todo): either document that it reflects "stages attempted before interruption" or weaken/remove it. Decision: Claude's discretion based on what makes the test assertions cleanest.

</specifics>

<deferred>
## Deferred Ideas

- **Public cancellation handle on `stream_turn`** — FUT-06, post-alpha. Cancelled live path cannot be tested until this lands.
- **Public API timeout field** — `RunTurnRequest` gains no timeout field in alpha. If needed later, thread to `AgentTurnRequest.timeout`.
- **`run_turn` consuming `stream_turn` internally** — design §17.3 long-term unification. Not in Phase 17.
- **`stream_turn_with_deps` live equivalent (`LiveTurnDeps`)** — if tests ever need injected live streaming. Not in alpha.
- **Buffered path branch population** — `stream_turn_with_deps` keeps `branch: None`; could be added if consistency tests demand it but not required.
- **Complex in-flight tool cancellation on timeout** — timeout breaks at stream granularity, not mid-tool. Tool-level cancellation is a separate concern.

</deferred>

---

*Phase: 17-Failure Matrix and Run/Stream Consistency*
*Context gathered: 2026-05-22*
