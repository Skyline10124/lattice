# Phase 15: AgentLoop Live Sink and Cooperative Cancellation - Context

**Gathered:** 2026-05-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Add live event forwarding to the AgentLoop and lock the alpha cancellation contract. This phase delivers:

1. `AgentLoop::run_turn_live(request, sender)` mirrors the existing buffered `result.events` to a `tokio::sync::mpsc::Sender<AgentTurnEvent>` while preserving the complete `AgentTurnResult.events` vector and `run_turn` behavior. Both entrypoints share the existing `AgentTurnRequest.timeout` wrapper (extracted into a small private helper); live-path timeout partial preservation is deferred to Phase 17.
2. A single `emit_agent_event(buffered, live, event)` helper used at every event emission site (round entry `ModelStarted`, every `LlmEvent`-derived delta inside the stream consumer, `ModelFinished`, `ToolCallStarted`, `ToolCallFinished`).
3. `consume_model_stream` becomes cancel-aware: it accepts a `cancel_flag: Option<&Arc<AtomicBool>>`, checks the flag *after* processing each non-`Done` `LlmEvent`, and returns `StreamOutcome::Cancelled` when the flag is set. Partial `assistant_content` / `reasoning_content` are preserved.
4. Cancellation checkpoints land at six tightly-defined sites in `run_turn_inner`: round top (existing); **after `ModelStarted` emit and before `stream_chat`** (new — covers the yield introduced by the awaited emit); after each in-stream `LlmEvent` (within `consume_model_stream`); after `Done` finalize and *before* the first tool of the round; before each tool starts; after each tool's real result is recorded. Done itself is not a cancel point — the model response is finalized first.
5. Drop semantics: dropping the live `mpsc::Receiver` (or any single `send` failing) does not cancel or fail the turn. `send` errors are silently ignored.
6. `AgentTurnEvent` gains `PartialEq, Eq` derives so the live-mirror test can `assert_eq!` event vectors without a custom comparator.
7. New behavioral test file `crates/lattice-agent/tests/live_sink_and_cancel.rs` (feature `test-utils`) covers VER-11 + live-sink behavior in seven scenarios. Tests **MUST drain the live receiver concurrently** with `run_turn_live` to avoid bounded-channel deadlock (`tokio::join!` or a spawned drain task).

### Hard boundary

```text
Phase 15 = run_turn_live + emit helper + cancel-aware stream consumer + 6 cancel checkpoints + tests.
No public TurnEvent / stream_turn changes (Phase 16).
No CLI rendering (Phase 18).
No LiveTurnDeps; TurnDeps stays the buffered seam (design §17.1).
No public cancellation handle on stream_turn (FUT-06, post-alpha).
No in-flight tool cancellation — running tools must complete (design §14.3).
No live-path timeout partial-content preservation (deferred to Phase 17). Phase 15 shares the existing buffered timeout shape across run_turn and run_turn_live.
No LlmEvent vocabulary changes.
No bounded-channel capacity decision (caller picks; Phase 16 will use 256 at the API boundary).
No AgentTurnEvent terminal variants (Phase 12 locked process-only). PartialEq/Eq derive added (D-15b) — variant set unchanged.
```

</domain>

<decisions>
## Implementation Decisions

### Mid-stream cancellation preserves partial content
- **D-01:** Extend `StreamOutcome` with a fourth variant: `Cancelled`. Final shape:
  ```rust
  enum StreamOutcome {
      Done,
      Error(String),
      Cancelled,
      Exhausted,
  }
  ```
  This keeps cancellation distinct from `Error(String)` at the data layer; status + terminal_reason mapping happens upstream in `run_turn_inner`.
- **D-02:** `consume_model_stream` gains a `cancel_flag: Option<&Arc<AtomicBool>>` parameter. Final signature:
  ```rust
  async fn consume_model_stream(
      &self,
      stream: lattice_llm::LlmStream,
      events: &mut Vec<AgentTurnEvent>,
      live: Option<&mpsc::Sender<AgentTurnEvent>>,
      cancel_flag: Option<&Arc<AtomicBool>>,
  ) -> StreamResult
  ```
  The cancel check happens *after* processing each non-`Done` `LlmEvent` (so the most-recent token/reasoning/tool-call event is buffered and forwarded before exit). On cancel, the function returns immediately with `outcome = Cancelled` carrying whatever has been accumulated so far.
- **D-03:** `Done` is never a cancel point inside `consume_model_stream`. When `Done` arrives, the function returns `StreamOutcome::Done` exactly as today; the upstream loop then runs all-or-nothing tool validation, emits `ModelFinished`, builds the assistant message (with tool_calls if valid), and *only then* checks cancel before entering the tool execution loop. This keeps "successful model response was finalized" decoupled from "no further work after cancel."
- **D-04:** On `StreamOutcome::Cancelled`, `run_turn_inner` flushes partial `assistant_content` / `reasoning_content` (when non-empty) into a single partial assistant `GraphMessage` via the same shape currently used by the `Error` and `Exhausted` branches, then returns:
  ```text
  status            = AgentTurnStatus::Cancelled
  terminal_reason   = Some("cancelled by caller")
  assistant_output  = accumulated visible content (not cleared)
  events            = all_events as accumulated (no ModelFinished, no synthesized terminal event)
  ```
  Reasoning-only partials are preserved (`content = ""`, `reasoning_content = Some(...)`) — same rule as Phase 14 D-13.
- **D-05:** Extract a small private helper to deduplicate the partial-flush code now repeated in the `Error`, `Exhausted`, and (new) `Cancelled` branches:
  ```rust
  fn flush_partial_assistant_message(
      messages: &mut Vec<GraphMessage>,
      all_messages: &mut Vec<GraphMessage>,
      assistant_content: &str,
      reasoning_content: &str,
  );
  ```
  Pure refactor; behavior unchanged for existing branches.

### Five cancellation checkpoints
- **D-06:** Checkpoint placement (in execution order through `run_turn_inner`):

  | # | Site | Where in code |
  |---|------|---------------|
  | 1 | Round top — before LLM request build | Existing check at `agent_loop/mod.rs:158-170`. Keep as-is. |
  | 1b | After emitting `ModelStarted`, before `stream_chat` | New: `emit_agent_event` is awaited (it can yield). The cancel flag may flip between checkpoint #1 and the actual stream open. Check the flag here; if set, return `Cancelled` with empty content (`stream_chat` was never called, no partial to preserve). |
  | 2 | After each non-`Done` `LlmEvent` inside the stream | Inside `consume_model_stream` per D-02. |
  | 3 | After `Done` finalize, before tool execution loop | New: after `ModelFinished` is emitted and the assistant tool-call message is appended, *before* the `for call in &validated_calls` loop. Done's "successful model response was finalized" semantics is preserved — the assistant message (including any valid `tool_calls`) is committed before the cancel branch returns. |
  | 4 | Before each tool starts | New: at the top of the tool execution loop iteration, *before* `ToolCallStarted` is emitted. Skips the rest of the batch on cancel; `ToolCallStarted` is not emitted for the skipped tool. |
  | 5 | After each tool finishes (real result recorded) | New: after `ToolCallFinished` event, summary, and tool `GraphMessage` are pushed. If cancelled, return Cancelled before next iteration / next round. |

- **D-07:** When cancellation is observed at checkpoints 3, 4, or 5, no new model request and no new tool starts. Already-running tools are never aborted (no `ToolCallFinished { ok: false, error_summary: Some("cancelled") }` synthesis — design §14.3). The current loop is sequential, so "already running" only applies to the in-flight tool that the agent is `await`-ing on; that tool's real result is recorded before the cancel branch runs.
- **D-08:** Cancel observed at checkpoint 3 (post-`Done`, pre-tools) means the assistant tool-call message has already been pushed (D-04 covers no-tool-calls case via the existing path that already returns Completed before reaching tool loop). The committed transcript will contain the assistant tool-call message but no tool result messages — same rule design §14.4 example "cancel before tool starts" describes.

### `run_turn_live` API shape
- **D-09:** Add a second public method on `AgentLoop`:
  ```rust
  pub async fn run_turn_live(
      &self,
      request: AgentTurnRequest,
      events: tokio::sync::mpsc::Sender<AgentTurnEvent>,
  ) -> AgentTurnResult
  ```
  `run_turn(request)` keeps its current signature and behavior. Both go through `run_turn_inner`.
- **D-10:** Refactor `run_turn_inner` to accept an optional live sender:
  ```rust
  async fn run_turn_inner(
      &self,
      request: &AgentTurnRequest,
      enabled: &EnabledTools,
      live_events: Option<&mpsc::Sender<AgentTurnEvent>>,
  ) -> AgentTurnResult
  ```
  Both `run_turn` and `run_turn_live` thread their respective `Option<&Sender>` into `run_turn_inner`. **`run_turn_live` honors `AgentTurnRequest.timeout` via the same wrapper used by `run_turn`** — refactor the existing `tokio::time::timeout(...)` block in `run_turn` (currently at `agent_loop/mod.rs:95-115`) into a small private helper (e.g. `apply_optional_timeout(request, fut)`) so both entrypoints share it. On timeout, `run_turn_live` returns the same `AgentTurnResult { status: Error, terminal_reason: Some("timeout"), .. }` shape as `run_turn`. Live-path timeout partial preservation (flushing accumulated content into the transcript) remains deferred to Phase 17 (STATE.md todo); Phase 15's wrapper preserves v1.1 buffered behavior — empty `messages`/`assistant_output` on timeout — for both methods.
- **D-11:** Do **not** add a sender field to `AgentTurnRequest`. The request struct stays a pure input description (session, transcript, user_input, model, provider, enabled_tools, project_root, cancel_flag, timeout). The live sender is a runtime-only side-channel and belongs on the call.

### Emit helper
- **D-12:** Add a free function in `agent_loop/mod.rs` (no trait, no struct):
  ```rust
  async fn emit_agent_event(
      buffered: &mut Vec<AgentTurnEvent>,
      live: Option<&mpsc::Sender<AgentTurnEvent>>,
      event: AgentTurnEvent,
  ) {
      buffered.push(event.clone());
      if let Some(sender) = live {
          let _ = sender.send(event).await;
      }
  }
  ```
  - **Clone cost:** events carry `String`s (token/reasoning content). Acceptable in alpha; if profiling shows hot-loop allocation, optimize later (e.g. shared `Arc<str>`). Not in scope here.
  - **Send error:** `let _ = ...` — receiver drop or any send failure is silently ignored.
  - **Backpressure:** the awaited `send` blocks the live caller when the channel is full. This is the alpha policy (design §18.1). No `try_send`, no buffering.
- **D-13:** Replace every `all_events.push(...)` site with `emit_agent_event(&mut all_events, live_events, event).await`. Sites:
  - `ModelStarted` (round entry)
  - `TokenDelta`, `ReasoningDelta` (inside `consume_model_stream`)
  - `ModelFinished` (after Done in both no-tool-calls and valid-tool-calls branches)
  - `ToolCallStarted`, `ToolCallFinished` (tool execution loop)
- **D-14:** `consume_model_stream` accepts `live: Option<&mpsc::Sender<AgentTurnEvent>>` (D-02) and uses the same helper internally. The current `events: &mut Vec<AgentTurnEvent>` parameter stays as the buffered backing store.

### What stays unchanged
- **D-15:** `LlmClient::stream_chat` semantics, `FakeStreamingLlmClient`, `RealLlmClient::stream_chat` override, `AgentTurnStatus` variants, and Phase 14's all-or-nothing tool validation are all unchanged.
- **D-15b:** `AgentTurnEvent` gains `#[derive(PartialEq, Eq)]` (added to the existing `#[derive(Debug, Clone)]` in `agent_loop/types.rs`). All existing variants carry only `String`/`Option<String>`/`bool`/`usize`/`Option<TurnUsage>` — `TurnUsage` already derives `PartialEq` and `Eq` in `lattice-session`, so the derive is mechanical. This is required by D-18 scenario #3 (live mirror equality test) and lets downstream tests use `assert_eq!` on event vectors without a custom comparator. Compile guard: any external code that pattern-matches on `AgentTurnEvent` continues to work; the derive only widens capability.
- **D-16:** Public `TurnEvent`, `stream_turn`, and `run_turn_with_deps` remain unchanged. No mapping helper updates. Phase 16 owns the public live stream.
- **D-17:** `AgentTurnRequest.cancel_flag: Option<Arc<AtomicBool>>` is the sole input channel for cancel. No new field, no token type, no handle.

### Test scope (`crates/lattice-agent/tests/live_sink_and_cancel.rs`)
- **D-18:** Seven required scenarios. Most reuse `FakeStreamingLlmClient` + a `FakeToolExecutor`-style helper, but **scenario #5 needs a custom test-controlled stream client** (see below). Tests require `--features test-utils`.

  1. **Cancel before tool execution** *(FakeStreamingLlmClient + cancel-signaling tool)* — model returns one valid tool call, `cancel_flag` is set after `Done` (e.g. by a `CancelSignalingTool`-style hook or by setting via shared `Arc<AtomicBool>` after the fake's `Done`). Assert: no `ToolCallStarted` event, status = `Cancelled`, `terminal_reason = Some("cancelled by caller")`, assistant message with `tool_calls` IS in transcript, no tool result message.
  2. **Cancel during tool execution** *(FakeStreamingLlmClient + tool that flips cancel)* — model returns one valid tool call, fake tool executor sets cancel_flag while running, then returns its real result. Assert: `ToolCallStarted` and `ToolCallFinished` both appear (with the tool's real result), no next `ModelStarted`, status = `Cancelled`, the real tool message is in transcript.
  3. **Live sender mirrors result.events** *(FakeStreamingLlmClient)* — successful run with tokens, reasoning, one tool call round, then final tokens. Assert via `assert_eq!`: events received on `mpsc::Receiver` equal `result.events` element-wise (same order, same payloads). Relies on the `PartialEq` derive added in D-15b.
  4. **Receiver drop does not cancel turn** *(FakeStreamingLlmClient)* — drop the receiver after the first event arrives (or before any). Assert: `run_turn_live` returns with status = `Completed`, full `result.events` populated, transcript correctly committed.
  5. **Cancel mid-stream preserves partial content** *(custom test-controlled stream LlmClient)* — `FakeStreamingLlmClient` emits its queued events synchronously from a `Vec` and would not let the test toggle `cancel_flag` between events. Use a small inline `LlmClient` impl (or a custom stream helper) that yields events through a channel / generator and checks a shared `Arc<AtomicBool>` between yields, mirroring the `CancelAwareLlm` pattern at `tests/agent_loop.rs:572`. The driver: emit `Token("p")`, `Token("ar")`, set cancel_flag, then attempt to emit more events. Assert: status = `Cancelled`, partial assistant content `"par"` in transcript, no `ModelFinished`, `terminal_reason = Some("cancelled by caller")`.
  6. **Cancel after first tool in multi-tool batch** *(FakeStreamingLlmClient + tool that flips cancel)* — model returns two valid tool calls; fake tool executor sets cancel_flag during the first tool's execution. Assert: first tool's `ToolCallStarted` + `ToolCallFinished` appear, second tool's events do NOT appear, status = `Cancelled`.
  7. **`run_turn` still buffers complete events** *(FakeStreamingLlmClient)* — backward-compat regression. No live sender. Assert: full `result.events` matches the same content as a parallel `run_turn_live` consumer would see.

- **D-19:** Test helpers needed:
  - A small helper to drive `run_turn_live` with a `tokio::sync::mpsc::channel(N)` and collect events from the receiver into a `Vec<AgentTurnEvent>`.
  - **The collector MUST drain the receiver concurrently with `run_turn_live`** (e.g. `tokio::join!(run_turn_live(...), drain_receiver(...))` or a spawned drain task awaited at the end). Sequential "run, then drain" deadlocks: the bounded channel fills, `emit_agent_event`'s awaited `send` blocks, and `run_turn_live` never returns. This applies to scenarios #1, #2, #3, #5, #6, #7 whenever a live sender is used and event count may exceed channel capacity.
  - A `CancelSignalingTool` (or equivalent inline closure pattern) that sets a shared `Arc<AtomicBool>` when invoked. Compose with `FakeToolExecutor` rather than re-creating it.
  - For scenario #2, a tool that takes a deterministic short delay (`tokio::task::yield_now()` is enough — no real timing dependency) and sets cancel_flag mid-execution.
  - For scenario #5, the custom stream LlmClient (see test description above). Place it inline in `live_sink_and_cancel.rs`; do not add it to `lattice_agent::testing`.
- **D-20:** Tests must use `#[tokio::test]` (multi-threaded not required; tools and consumer are sequential) and avoid timing assertions. Channel capacity in tests: 32 is plenty for non-overflow scenarios. The concurrent-drain rule (D-19) applies regardless of capacity, since it is also the right pattern for any future test that exceeds the channel.
- **D-21:** Phase 15 does NOT add tests for: `stream_turn` public live stream (Phase 16), CLI rendering (Phase 18), receiver-drop on the public API stream (Phase 17 VER-12), commit failure paths (Phase 17), timeout cancellation semantics on the buffered path (already covered in `run_turn` timeout wrapper). The shared timeout wrapper from D-10 means `run_turn_live` timeout uses the same `AgentTurnResult { status: Error, terminal_reason: Some("timeout"), .. }` shape; one minimal regression test in this file or `tests/agent_loop.rs` confirming `run_turn_live` returns Error/timeout for a too-short timeout is acceptable but optional.

### Claude's Discretion
- Exact naming of the partial-flush helper (`flush_partial_assistant_message` vs. `commit_partial_assistant`).
- Internal shape of test helpers (closure captures vs. dedicated structs).
- Doc comments on `run_turn_live`, `emit_agent_event`, and the new `StreamOutcome::Cancelled` variant.
- Whether `consume_model_stream`'s cancel check is a small inline `if let Some(flag) = cancel_flag { if flag.load(Ordering::Relaxed) { return ... } }` block per iteration or a tiny `is_cancelled` helper. Inline is fine; prefer readability.
- Whether to add a small `RunPhase` (or similar) marker for diagnostics on cancel — **discretion to skip**, design doesn't ask for it.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Design Source
- `.planning/alpha-v1.2-evented-runtime-design.md` — §14 (cancellation semantics: cooperative only, Cancelled = explicit external, no in-flight tool cancellation), §15 (AgentLoop live event sink: dual-method shape, emit helper, no public accumulator), §17.1 (TurnDeps remains buffered seam; live path bypass is Phase 16 concern), §13.3 (partial content preservation pattern reused for Cancelled)

### Requirements
- `.planning/REQUIREMENTS.md` — AGENT-08 (run_turn_live), AGENT-09 (cooperative cancellation explicit), VER-11 (cancel-before-tool / cancel-during-tool tests)

### AgentLoop (modified)
- `crates/lattice-agent/src/agent_loop/mod.rs` — primary change site:
  - Round-top cancel check: 158-170 (kept)
  - LLM call site: 209-222 (becomes routed through emit helper for `ModelStarted`)
  - `consume_model_stream`: 523-670 (gains `live` and `cancel_flag` params, post-event cancel check, new `Cancelled` outcome)
  - Stream outcome match: 234-294 (new `Cancelled` arm, partial flush helper)
  - Tool execution loop: 425-478 (cancel checks before `ToolCallStarted` and after tool result recording)
  - Add: `run_turn_live` method, `emit_agent_event` free fn, `flush_partial_assistant_message` helper, `StreamOutcome::Cancelled` variant
- `crates/lattice-agent/src/agent_loop/types.rs` — `cancel_flag` field already on `AgentTurnRequest`; no changes
- `crates/lattice-agent/src/agent_loop/traits.rs` — unchanged
- `crates/lattice-agent/src/lib.rs` — re-export check: `run_turn_live` accessible via the same path as `run_turn`

### Test Infrastructure (Phase 13/14)
- `crates/lattice-agent/src/testing.rs` — `FakeStreamingLlmClient` reused for stream control; gated behind `test-utils`
- `crates/lattice-agent/tests/streaming_delta.rs` — `FakeToolExecutor` (VecDeque + Arc<Mutex<Vec<String>>> execution-order tracking) is the model for Phase 15 tool helpers
- `crates/lattice-agent/tests/agent_loop.rs:572-621` — existing `CancelAwareLlm` pattern (`Arc<AtomicBool>` plumbed through tool/LLM); Phase 15 tests adapt this for the multi-checkpoint cases

### Phase 12-14 Context (carry-forward)
- `.planning/phases/12-event-and-status-vocabulary-contract/12-CONTEXT.md` — `AgentTurnEvent` is process-only, `AgentTurnStatus` uses `Cancelled` (no `Failed`); no terminal events allowed in agent vocabulary
- `.planning/phases/13-llm-streaming-trait-and-fake-streaming-client/13-CONTEXT.md` — `FakeStreamingLlmClient` builder API, two-channel error model
- `.planning/phases/14-agentloop-streaming-delta-engine/14-CONTEXT.md` — D-09 (all-or-nothing tool validation), D-11/D-12/D-13 (partial content preservation pattern that D-04/D-05 generalize to Cancelled)

### Tokio
- `tokio::sync::mpsc::Sender::send` semantics — awaited send blocks on full bounded channel; returns `Err(SendError)` if receiver is dropped. Phase 15 ignores both error and backpressure timing.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `FakeStreamingLlmClient` (Phase 13) — multi-round event queue, mid-stream error injection. Directly usable for scenarios 1, 2, 3, 5, 6, 7.
- `FakeToolExecutor` from `tests/streaming_delta.rs` — VecDeque FIFO + execution-order tracking. Compose with a cancel-signaling wrapper for scenarios 2 and 6.
- `CancelAwareLlm` from `tests/agent_loop.rs:572` — pattern for plumbing `Arc<AtomicBool>` into a fake. Phase 15 tests will use a similar shared cancel flag set by tools / set by the test driver between events.
- `tokio::sync::mpsc::channel` + `tokio_stream::wrappers::ReceiverStream` (already a Phase 16 concern) — Phase 15 tests collect via `recv()` directly; no `tokio-stream` dep needed yet.
- `consume_model_stream` already returns a struct with accumulated state — extending the outcome enum and signature is a low-risk refactor.

### Established Patterns
- Round-top cancel check (`agent_loop/mod.rs:158-170`) uses `Ordering::Relaxed` and returns a fully-formed `AgentTurnResult` with empty `messages`. Phase 15 cancel returns from non-top sites must populate `messages` / `assistant_output` / `events` correctly.
- Partial-flush pattern in `Error` and `Exhausted` arms (`mod.rs:234-294`) is duplicated; D-05 consolidates it before adding the third user.
- `all_events.push(event)` is the universal emission. D-12/D-13 wrap it; existing `consume_model_stream` already pushes via `events.push(...)` (a `&mut Vec`) and will use the helper too.
- `run_turn` timeout wrapper at `mod.rs:95-115` — Phase 15 confirms `run_turn_live` does NOT apply timeout (alpha scope).

### Integration Points
- `crates/lattice-agent/src/agent_loop/mod.rs:73` — `run_turn` public entrypoint; `run_turn_live` lives next to it.
- `crates/lattice-agent/src/agent_loop/mod.rs:118` — `run_turn_inner` signature changes (new `live_events: Option<&mpsc::Sender<AgentTurnEvent>>` param).
- `crates/lattice-agent/src/agent_loop/mod.rs:204-207`, `:302-305`, `:395-398`, `:426-429`, `:451-457` — all `all_events.push` sites become `emit_agent_event(...).await`.
- `crates/lattice-agent/src/agent_loop/mod.rs:523` — `consume_model_stream` signature widens.
- `crates/lattice-agent/src/agent_loop/mod.rs:486-516` — `PendingToolCall` / `StreamOutcome` / `StreamResult` definitions; `StreamOutcome::Cancelled` lands here.
- `crates/lattice-agent/Cargo.toml` — `tokio = { workspace = true }` already present (used for `time::timeout`); `mpsc` is in tokio's `sync` feature, which is part of `full`/default workspace usage. Verify before planning.
- No new dependencies expected. `serde_json` is already in for Phase 14 tool validation.

### What Phase 15 must NOT touch
- `crates/lattice-api/src/turn/execute.rs` — `stream_turn`, `stream_turn_with_deps`, `run_turn`, `run_turn_with_deps` — all unchanged.
- `crates/lattice-api/src/turn/deps.rs` — `TurnDeps`, `RealTurnDeps` — unchanged.
- `crates/lattice-api/src/turn/types.rs` — `TurnEvent` variants and `RunTurnStage` — unchanged.
- `crates/lattice-cli/src/commands/run.rs` — `RealLlmClient` — unchanged.
- `crates/lattice-llm/**` — entire crate — unchanged.

</code_context>

<specifics>
## Specific Ideas

- The cancel check inside `consume_model_stream` is "after processing each non-`Done` event" specifically because the stream has already yielded that event and the consumer must not silently drop it. The order is: take event from stream → match arm processes it (push delta into accumulator, emit via helper) → check cancel → return `Cancelled` if set, else `continue`. Tool-call assembly events (Start/Delta/End) follow the same rule; Phase 14's per-event `StreamOutcome::Error` early-returns for malformed assemblies stay above the cancel check.
- For checkpoint 5 (after-tool), the cancel branch returns the full `AgentTurnResult` with `messages`/`assistant_output`/`tool_results`/`events` populated through the most recent tool. The current `Error` and `Exhausted` paths show the shape; Cancelled mirrors them with `status = Cancelled`.
- Test scenario #5 specifically wants the cancel to land *between* `Token` events. `FakeStreamingLlmClient` emits events from a `Vec` synchronously; to get the cancel set "between" events, scenario #5 wraps the fake's stream or uses a custom stream-emitting LlmClient that checks a shared flag — pattern is in `tests/agent_loop.rs::CancelAwareLlm`. Alternatively, add a tiny inline `LlmClient` impl that yields tokens through a channel and toggles the flag from the test driver. Planner picks the cleanest route.
- Channel capacity for `run_turn_live`: callers in Phase 16 will pick 256; tests can pick anything (32 is fine). Phase 15 does not pin a constant.

</specifics>

<deferred>
## Deferred Ideas

- **`stream_turn` public live stream** — Phase 16 (API-04..API-10). The select-loop pattern in design §16.6 maps `AgentTurnEvent` to `TurnEvent` and derives terminal events from `AgentTurnResult`. Phase 15 leaves the agent-side primitives ready.
- **Receiver-drop test on the public API stream** — Phase 17 (VER-12). Phase 15 only proves the agent-level send-error tolerance.
- **Public cancellation handle on `stream_turn`** — FUT-06, post-alpha. Today `stream_turn` callers cannot set cancel_flag; the alpha cancellation contract is testable at the agent level only.
- **Timeout partial-content preservation** — STATE.md todo, deferred to Phase 17. Phase 15's cancel partial-flush is reusable by Phase 17 timeout if it adopts a cooperative deadline pattern.
- **In-flight tool cancellation** — design §14.3 explicitly out of scope for v1.2-alpha. Future requirement only after the cooperative model proves itself.
- **`AgentTurnRequest.cancel_token: tokio::sync::CancellationToken`** — alternative to `Arc<AtomicBool>`. Not needed now; `cancel_flag` is sufficient. Revisit if Phase 16+ wants composable cancellation semantics.
- **Backpressure metrics on the live channel** — none in alpha; observation deferred until live rendering exists.

</deferred>

---

*Phase: 15-AgentLoop Live Sink and Cooperative Cancellation*
*Context gathered: 2026-05-21*
