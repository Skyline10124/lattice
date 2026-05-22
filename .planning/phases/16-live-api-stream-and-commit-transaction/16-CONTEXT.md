# Phase 16: Live API Stream and Commit Transaction - Context

**Gathered:** 2026-05-22
**Status:** Ready for planning

<domain>
## Phase Boundary

Convert `stream_turn` from post-execution buffered replay into a real live event stream. The spawned task maps agent events to public `TurnEvent` values in flight, then commits the turn and emits commit lifecycle events. This phase delivers:

1. `stream_turn` performs startup validation (profile/model/session/HEAD/branch) and returns `Result::Err` before the stream if any preparation fails.
2. On success, `stream_turn` spawns one live task, returns `Ok(TurnEventStream)` backed by a bounded `mpsc` channel (capacity 256) wrapped in `ReceiverStream`.
3. The live task uses a `tokio::select!` loop to drain `AgentTurnEvent` from `run_turn_live` and map each to `TurnEvent` inline — no nested bridge spawn.
4. After the agent loop returns, remaining agent events are drained, terminal event is derived from `AgentTurnResult.status`, and commit lifecycle events (`TurnCommitting`, `TurnCommitted` or `CommitFailed`) are emitted.
5. Commit stores only the current turn's transcript delta plus metadata. Completed, Error, and Cancelled turns all commit and move HEAD. Commit failure emits `CommitFailed` and leaves HEAD unchanged.
6. Dropping the event receiver does not cancel or fail the turn; the background task continues and commits.

### Hard boundary

```text
Phase 16 = live stream_turn + PreparedTurn + commit transaction + map/terminal/commit event emission.
stream_turn_with_deps remains unchanged as the buffered replay seam (not deprecated, not deleted).
run_turn and run_turn_with_deps unchanged.
TurnDeps unchanged — live path bypasses it.
No public cancellation handle on stream_turn (FUT-06, post-alpha).
No timeout on the live path (RunTurnRequest has no timeout field; AgentTurnRequest.timeout = None).
No CLI rendering (Phase 18).
No receiver-drop-cancels-turn semantics.
No durable event log.
```

</domain>

<decisions>
## Implementation Decisions

### stream_turn_with_deps disposition
- **D-01:** `stream_turn_with_deps` is preserved unchanged as the buffered replay seam for existing tests. The production `stream_turn(session, agent_loop, request)` is rewritten to use the live path. Comment on `stream_turn_with_deps` should read: "Buffered replay seam for tests. Production stream_turn is live and bypasses TurnDeps."

### cancel_flag and timeout in live path
- **D-02:** Live `stream_turn` builds the `AgentTurnRequest` with `cancel_flag: None` and `timeout: None`. No internal cancel flag is created. Receiver drop is observation-stop, not execution-stop. The background task runs to completion and commits regardless of whether anyone is listening. This directly implements API-10.
- **D-03:** "receiver drop = stop observing, not cancel execution." Send errors inside `emit_turn_event` are silently ignored (`let _ = sender.send(...).await`).

### PreparedTurn and code organization
- **D-04:** Live path has its own `prepare_live_turn(session: &SessionApi, request: &RunTurnRequest) -> Result<PreparedTurn>` function. It does not call `execute_turn_inner` or use `TurnDeps`. Session orchestration (open/create, head_transcript, status) is allowed to have minor repetition with the buffered path — the two paths are intentionally decoupled.
- **D-05:** Pure helpers are shared between buffered and live paths to avoid semantic drift:
  - `resolve_model_and_prompt(request)` — already exists in `mapping.rs`
  - `build_agent_request(...)` — already exists in `mapping.rs`; live path calls it with the resolved values from `PreparedTurn`
  - `map_agent_event(event)` — already exists in `execute.rs`
  - `derive_terminal_event(result) -> Option<TurnEvent>` — extract from the existing match in `stream_turn_with_deps`
  - `build_commit_input(request, prepared, agent_result) -> TurnCommitInput` — new shared helper
  - `map_status(status)` — already exists in `mapping.rs`
- **D-06:** `PreparedTurn` struct lives in `turn/execute.rs` (or a new `turn/live.rs` — Claude's discretion on file organization):
  ```rust
  struct PreparedTurn {
      session_id: String,
      parent_commit_id: Option<String>,
      parent_ids: Vec<String>,
      branch: Option<String>,
      model: String,
      provider: String,
      system_prompt: Option<String>,
      transcript: HeadTranscript,
  }
  ```

### branch information
- **D-07:** `prepare_live_turn` calls `session.status(&session_id)` to extract branch name. `TurnStarted.branch` is populated from the first live event:
  ```rust
  branch = match session.status(&session_id)?.head {
      GraphHead::Branch { name } => Some(name),
      GraphHead::Detached { .. } => None,
  };
  ```
  If `status()` fails, it is a startup failure — `stream_turn` returns `Err` before the stream is created.
- **D-08:** The buffered `stream_turn_with_deps` continues to emit `branch: None` in `TurnStarted`. Phase 17 consistency tests may assert that the live path populates branch when available, but the buffered path is not required to match.

### Live task architecture
- **D-09:** Single `tokio::spawn` + `tokio::select!` drain loop (design §16.6). No nested spawn. The select-loop guarantees all agent events are forwarded before terminal/commit events. After `agent_fut` resolves, `while let Some(event) = agent_rx.recv().await` drains any buffered agent events before proceeding to terminal emission.
- **D-10:** Terminal event derivation uses `AgentTurnResult.status`, never the presence/absence of `ModelFinished` in events (WR-01 from Phase 15 addendum). Completed status emits no terminal event; Error emits `TurnFailed`; Cancelled emits `TurnCancelled`.
- **D-11:** Commit construction: user message is prepended to `agent_result.messages` (same as buffered path). `derive_commit_message` is called on the combined messages. Commit message is bounded to 72 chars, no status prefix (COMM-04).
- **D-12:** `terminal_reason` stays a short human-readable string (COMM-05). No recovery hints, no structured error payloads.

### Commit failure handling
- **D-13:** `session.commit_turn(input)` is called directly (not through `TurnDeps`). On `Ok(commit_id)`: emit `TurnCommitted { session_id, commit_id, status }`. On `Err(e)`: emit `CommitFailed { reason: e.to_string() }`. HEAD is unchanged on failure. No retry.
- **D-14:** `TurnEventStream` is `Stream<Item = TurnEvent>` (not `Result<TurnEvent>`). All runtime outcomes — including commit failure — are encoded as `TurnEvent` variants within the stream. The stream ends (sender drops) after the final event.

### Provider field semantics
- **D-15:** `PreparedTurn.provider` is always a `String` (from `resolve_model_and_prompt`). When building `AgentTurnRequest`, filter it:
  ```rust
  let agent_provider = Some(prepared.provider.clone()).filter(|p| p != "unknown");
  ```
  `TurnCommitInput.provider` keeps the raw `prepared.provider` string. This matches the current buffered path behavior at `execute_turn_inner` and Phase 12 repair's stable provider semantics.

### build_commit_input is mandatory shared
- **D-16:** `build_commit_input` is a must-have shared helper, not optional. Extract a neutral `TurnCommitContext` that both paths can construct:
  ```rust
  struct TurnCommitContext {
      session_id: String,
      parent_ids: Vec<String>,
      model: String,
      provider: String,
      profile_snapshot_id: Option<String>,
  }
  ```
  `PreparedTurn` contains or can produce a `TurnCommitContext`. Buffered path constructs one from its local variables without requiring `PreparedTurn`. Helper signature:
  ```rust
  fn build_commit_input(
      user_input: &str,
      ctx: &TurnCommitContext,
      agent_result: &AgentTurnResult,
  ) -> TurnCommitInput
  ```
  This helper prepends the user message, extends with `agent_result.messages`, calls `derive_commit_message`, maps status, and fills terminal_reason/usage. Both live and buffered paths call it. Phase 17 consistency is enforced by having one code path for commit construction.

### Channel and stream type
- **D-17:** `const TURN_EVENT_CHANNEL_CAPACITY: usize = 256;` in the turn module.
- **D-18:** Return type: `TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>`. Wrap `mpsc::Receiver<TurnEvent>` with `tokio_stream::wrappers::ReceiverStream`.
- **D-19:** Dependency changes require TWO files:
  1. Workspace `Cargo.toml`: add `tokio-stream = "0.1"` to `[workspace.dependencies]`
  2. `crates/lattice-api/Cargo.toml`: add `tokio-stream = { workspace = true }` to `[dependencies]`

### select! loop is concurrent drain (mandatory pattern)
- **D-20:** The live task MUST drain `agent_rx` concurrently with `agent_fut` via `tokio::select!`. Sequential "await run_turn_live, then drain rx" is a deadlock: the bounded channel fills, `emit_agent_event`'s awaited send blocks, and `run_turn_live` never returns. The correct pattern:
  ```rust
  let (agent_tx, mut agent_rx) = mpsc::channel(TURN_EVENT_CHANNEL_CAPACITY);
  let mut agent_fut = Box::pin(agent_loop.run_turn_live(agent_request, agent_tx));
  let agent_result = loop {
      tokio::select! {
          maybe_event = agent_rx.recv() => {
              if let Some(event) = maybe_event {
                  emit_turn_event(&turn_tx, map_agent_event(event)).await;
              }
          }
          result = &mut agent_fut => {
              break result;
          }
      }
  };
  // Drain remaining buffered events after agent completes
  while let Some(event) = agent_rx.recv().await {
      emit_turn_event(&turn_tx, map_agent_event(event)).await;
  }
  ```
  This is not discretionary — it is the only correct architecture. Plan MUST specify this pattern explicitly.

### Claude's Discretion
- File organization: whether live path code goes in `turn/execute.rs` or a new `turn/live.rs`.
- Internal naming of helpers (`emit_turn_event`, `run_live_turn_task`, `prepare_live_turn`).
- Whether to add `derive_terminal_event` as a free function or keep it inline.
- Doc comments on new public and internal functions.
- Whether `PreparedTurn` is pub(crate) or fully private.
- Whether `TurnCommitContext` is a standalone struct or embedded in `PreparedTurn` (as long as buffered path can construct one without `PreparedTurn`).

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Design Source
- `.planning/alpha-v1.2-evented-runtime-design.md` — §16 (API live stream model: spawn+channel, startup vs runtime failure, PreparedTurn, mapping, emit wrapper, live task shape), §17.1 (TurnDeps remains buffered seam), §16.2 (startup failure returns Err, runtime outcomes are TurnEvent)

### Requirements
- `.planning/REQUIREMENTS.md` — COMM-01, COMM-02, COMM-03, COMM-04, COMM-05, API-04, API-05, API-06, API-07, API-08, API-09, API-10

### API Turn Layer (primary modification site)
- `crates/lattice-api/src/turn/execute.rs` — current `stream_turn` (to be rewritten), `stream_turn_with_deps` (preserved), `map_agent_event` (reused), `execute_turn_inner` (unchanged)
- `crates/lattice-api/src/turn/deps.rs` — `TurnDeps` trait, `RealTurnDeps` (both unchanged; live path bypasses)
- `crates/lattice-api/src/turn/types.rs` — `TurnEvent` (unchanged), `TurnEventStream` (unchanged), `RunTurnRequest`, `RunTurnStatus`, `TurnUsageSummary`
- `crates/lattice-api/src/turn/mapping.rs` — `resolve_model_and_prompt`, `build_agent_request`, `map_status`, `map_tool_results` (all reused by live path)
- `crates/lattice-api/src/session.rs` — `SessionApi` methods: `create_session`, `open_session`, `status`, `head_transcript`, `commit_turn`

### Session/Graph Layer (consumed, not modified)
- `crates/lattice-session/src/graph.rs:86` — `GraphHead` enum (`Branch { name }`, `Detached { commit_id }`)
- `crates/lattice-session/src/graph.rs:169` — `SessionStatus { head: GraphHead, ... }`
- `crates/lattice-session/src/graph.rs:244` — `derive_commit_message` (72 char bound from first user message)
- `crates/lattice-session/src/lib.rs` — re-exports `TurnCommitInput`, `HeadTranscript`, `GraphHead`, `SessionStatus`

### Agent Layer (consumed, not modified)
- `crates/lattice-agent/src/agent_loop/mod.rs` — `AgentLoop::run_turn_live(request, sender)` (Phase 15)
- `crates/lattice-agent/src/agent_loop/types.rs` — `AgentTurnEvent` (6 process variants), `AgentTurnResult`, `AgentTurnStatus`, `AgentTurnRequest`

### Phase 14-15 Context (carry-forward)
- `.planning/phases/14-agentloop-streaming-delta-engine/14-CONTEXT.md` — stream consumption, tool validation, delta accumulation
- `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-CONTEXT.md` — `run_turn_live`, `emit_agent_event`, cancel checkpoints, sender-drop tolerance
- `.planning/phases/15-agentloop-live-sink-and-cooperative-cancellation/15-ADDENDUM.md` — WR-01 (terminal status from result.status, not ModelFinished presence), IN-01 (PartialEq only)

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `map_agent_event` (`execute.rs:150-179`) — 1:1 mapping already implemented, reuse directly in live task.
- `resolve_model_and_prompt` (`mapping.rs:10-41`) — model/provider resolution, directly callable from `prepare_live_turn`.
- `build_agent_request` (`mapping.rs:44-80`) — builds `AgentTurnRequest` from resolved values; live path passes `cancel_flag: None, timeout: None`.
- `map_status` (`mapping.rs:97-103`) — agent→API status mapping.
- `derive_commit_message` (`lattice-session graph.rs:244`) — first user message, 72 char bound.
- `SessionApi::status` (`session.rs:76`) — returns `SessionStatus` with `head: GraphHead`.
- `AgentLoop::run_turn_live` (Phase 15) — accepts `mpsc::Sender<AgentTurnEvent>`, returns `AgentTurnResult`.
- `tokio_stream::wrappers::ReceiverStream` — wraps `mpsc::Receiver` into a `Stream`.

### Established Patterns
- Buffered path in `execute_turn_inner`: prepare → agent → commit → return. Live path follows the same logical order but with events emitted inline.
- `let _ = sender.send(event).await` — fire-and-forget pattern established by Phase 15's `emit_agent_event`.
- `stream::iter(events)` for the buffered path; `ReceiverStream::new(rx)` for the live path. Both satisfy `TurnEventStream`.
- Commit input construction: user message prepended, `derive_commit_message` called, profile_snapshot_id from loaded_profile.

### Integration Points
- `crates/lattice-api/src/turn/execute.rs:250-257` — `stream_turn` function to be rewritten as live path.
- `Cargo.toml` (workspace root) — add `tokio-stream = "0.1"` to `[workspace.dependencies]`.
- `crates/lattice-api/Cargo.toml` — add `tokio-stream = { workspace = true }` to `[dependencies]`.
- `crates/lattice-api/src/turn/mod.rs` — may need to re-export new items or add `mod live`.

</code_context>

<specifics>
## Specific Ideas

- The `TurnCommitting` event should be emitted immediately before calling `session.commit_turn(...)` — it signals "persistence is about to happen" to the stream consumer.
- After commit (success or failure), the sender is dropped, ending the stream. No explicit "stream end" event — channel close is the signal.
- `build_commit_input` is mandatory shared infrastructure. It takes `(&str, &TurnCommitContext, &AgentTurnResult)` and returns `TurnCommitInput`. Both buffered and live paths must use it. Buffered path refactored to call it instead of inlining commit construction.
- Live path's `build_agent_request` call uses the same `mapping::build_agent_request` but with values unpacked from `PreparedTurn`. Provider filtering (`Some(provider).filter(|p| p != "unknown")`) must happen at the call site, matching current buffered behavior at `execute_turn_inner:48`.
- The select-loop concurrent drain pattern (D-20) is the single most critical correctness detail. The plan must present it as mandatory architecture, not discretionary. Sequential await+drain will deadlock.
- `tokio::pin!(agent_fut)` or `Box::pin(...)` — either is acceptable. The key is that `agent_fut` is pinned so `&mut agent_fut` can be polled repeatedly inside the select loop.

</specifics>

<deferred>
## Deferred Ideas

- **Public cancellation handle on `stream_turn`** — FUT-06, post-alpha. Today callers cannot cancel a live turn from the API surface.
- **Timeout on live path** — `RunTurnRequest` has no timeout field. If needed in future, add it there and thread to `AgentTurnRequest.timeout`.
- **Receiver-drop tests** — Phase 17 (VER-12) proves receiver drop does not cancel and commit still succeeds.
- **`run_turn` consuming `stream_turn` internally** — design §17.2 says "not in first live PR." Phase 17 may align semantics, future milestone unifies.
- **`stream_turn_with_deps` live equivalent** — if tests ever need injected live streaming, add a `LiveTurnDeps` later. Not in alpha.
- **Buffered path `branch` population** — `stream_turn_with_deps` keeps `branch: None`; if Phase 17 consistency tests demand it, add status() call there too.

</deferred>

---

*Phase: 16-Live API Stream and Commit Transaction*
*Context gathered: 2026-05-22*
