# Phase 14: AgentLoop Streaming Delta Engine - Context

**Gathered:** 2026-05-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Switch AgentLoop's inner model execution from buffered `chat()` to streaming `stream_chat()` delta processing. This phase delivers:

1. AgentLoop consumes `stream_chat` per-chunk, emitting `ModelStarted`, per-chunk `TokenDelta`, per-chunk `ReasoningDelta`, and `ModelFinished` on successful model completion.
2. Streaming deltas are finalized into transcript `GraphMessage` values: visible content separate from reasoning content.
3. Tool calls are assembled from stream events (`ToolCallStart`/`ToolCallDelta`/`ToolCallEnd`), validated as a batch, then executed sequentially. `ToolCallStarted` events are emitted only immediately before real execution.
4. Stream errors or missing `Done` preserve partial assistant content/reasoning and do not synthesize `ModelFinished`.
5. `RealLlmClient::stream_chat` overridden to call `lattice_llm::stream()` — production path gets real per-chunk streaming.

### Hard boundary

```text
Phase 14 = AgentLoop stream consumer switch + RealLlmClient override + streaming behavior tests.
No run_turn_live / live sender (Phase 15).
No public TurnEvent / stream_turn API changes (Phase 16).
No CLI rendering changes (Phase 18).
No LlmEvent vocabulary changes.
No parallel tool execution.
No cancellation semantics changes (Phase 15).
```

</domain>

<decisions>
## Implementation Decisions

### RealLlmClient stream wiring
- **D-01:** Override `RealLlmClient::stream_chat` to call `lattice_llm::stream(request)` (which does model resolution + `dispatch_stream` internally). Same pattern as `chat()` calling `lattice_llm::chat(request)`. Error mapping: `LlmError` → `Box<dyn Error + Send + Sync>`.
- **D-02:** The default `stream_chat` fallback remains available for legacy/test clients that only implement `chat()`. `FakeLlm` and friends in existing tests keep working without modification.

### AgentLoop stream consumption
- **D-03:** AgentLoop's inner model call switches from `self.config.llm.chat(chat_request)` to `self.config.llm.stream_chat(chat_request)`. The returned `LlmStream` is consumed event-by-event in a `while let Some(event) = stream.next().await` loop.
- **D-04:** `LlmEvent::Started` is **ignored** (no-op, not forwarded). AgentLoop already emits `AgentTurnEvent::ModelStarted` before calling `stream_chat`; ignoring provider-side `Started` avoids double-start.
- **D-05:** Provider metadata events (`LlmEvent::Citation`, `LlmEvent::Signature`, `LlmEvent::GroundingChunk`, `LlmEvent::SafetyRating`) are ignored — same list as Phase 13's `FakeStreamingLlmClient::chat()` aggregation.

### Delta accumulation and finalization
- **D-06:** Two separate accumulators per model request:
  ```rust
  let mut assistant_content = String::new();
  let mut reasoning_content = String::new();
  ```
  `LlmEvent::Token { content }` → append to `assistant_content`, emit `AgentTurnEvent::TokenDelta { content }`.
  `LlmEvent::Reasoning { content }` → append to `reasoning_content`, emit `AgentTurnEvent::ReasoningDelta { content }`.
- **D-07:** On successful stream completion (`LlmEvent::Done`), finalize into `GraphMessage`:
  ```rust
  GraphMessage {
      role: "assistant",
      content: assistant_content,
      reasoning_content: non_empty_option(reasoning_content),
      tool_calls: assembled_tool_calls_if_any,
      ..
  }
  ```
  `assistant_output` = accumulated `assistant_content` from the final model response (same as today).

### Tool call assembly and validation
- **D-08:** Tool calls are assembled from stream events into a pending list during stream consumption:
  - `ToolCallStart { id, name }` → start new pending tool call entry
  - `ToolCallDelta { id, arguments_delta }` → append to matching entry's arguments
  - `ToolCallEnd { id }` → mark entry as stream-complete
- **D-09:** Validation is **all-or-nothing per model response**. After `LlmEvent::Done` arrives with tool calls pending:
  1. Check completeness: `ToolCallEnd` marks one call complete. `Done` may close a still-open pending call **only if** its accumulated arguments normalize to valid JSON. If any pending call has no valid JSON by `Done`, the response is invalid.
  2. Normalize empty arguments to `"{}"` for no-arg tools. **The normalized value is used both for tool execution AND for the committed assistant `GraphToolCall.arguments`** — no divergence between transcript and execution input.
  3. Validate arguments are syntactically valid JSON (via `serde_json::from_str::<serde_json::Value>()`).
  4. If ALL valid → emit `ModelFinished`, commit assistant message with `tool_calls`, execute sequentially.
  5. If ANY invalid/incomplete → execute **none**, commit assistant content/reasoning only (no malformed `tool_calls`), return `Error` with `terminal_reason = "invalid/incomplete tool call: {detail}"`.
- **D-10:** `ToolCallStarted` (the agent-level event) is emitted only immediately before real tool execution — never during stream assembly. Same timing as current code, just moved after the all-or-nothing validation gate.

### New dependency: serde_json
- **D-10b:** `lattice-agent` gains `serde_json` as a dependency for tool call argument validation. Add `serde_json = { workspace = true }` to `crates/lattice-agent/Cargo.toml` (or the workspace-appropriate pattern). This must be done before implementing D-09's JSON validation.

### Stream error and partial content preservation
- **D-11:** On `LlmEvent::Error { message }` mid-stream:
  - Flush accumulated `assistant_content`/`reasoning_content` into a partial assistant `GraphMessage` (if non-empty).
  - Set `assistant_output` = accumulated visible content (do not clear it).
  - Do **not** emit `ModelFinished`.
  - Return with `status = Error`, `terminal_reason = Some("LLM error: {message}")`.
- **D-12:** On stream exhaustion without `Done` (stream ends, no `Done` event received):
  - Same behavior as D-11: flush partial content, no `ModelFinished`, `status = Error`, `terminal_reason = Some("LLM stream ended without completion")`.
  - **Distinction from Phase 13:** Phase 13's `FakeStreamingLlmClient::chat()` may fallback to `"stop"` finish_reason for buffered compatibility. Phase 14's AgentLoop streaming path does NOT accept missing `Done` as a normal completion — it is always an error. These are two different layers with different contracts.
- **D-13:** Partial assistant messages with only reasoning (no visible content) are still committed: `content = ""`, `reasoning_content = Some(...)`. This preserves full turn history for debugging.

### ModelFinished semantics
- **D-14:** `ModelFinished` is emitted **only** when `LlmEvent::Done` is received AND the model response is valid (either pure text, or all tool calls pass validation). If `Done` arrives but tool calls are invalid, no `ModelFinished` is emitted.
- **D-15:** `ModelFinished.usage` comes from `LlmEvent::Done { usage }`. If provider returns `None`, `ModelFinished.usage = None` (acceptable per design §11.2).

### Test strategy
- **D-16:** Phase 14 behavioral tests use `FakeStreamingLlmClient` exclusively (multi-chunk real stream simulation). Tests require `--features test-utils` because `lattice_agent::testing` is feature-gated (Phase 13 D-08). Build/test commands:
  ```bash
  cargo test -p lattice-agent --features test-utils
  cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings
  ```
  Required test scenarios:
  1. Multi-chunk token deltas assemble into one assistant message (`[Token("p"), Token("ong"), Done]` → content "pong").
  2. Reasoning separate from visible content (`[Reasoning("think"), Token("answer"), Done]` → `reasoning_content = "think"`, `assistant_output = "answer"`).
  3. Complete tool call assembly and sequential execution (ToolCallStart → ToolCallDelta → ToolCallEnd → Done → execute).
  4. Incomplete tool call (missing ToolCallEnd, stream error) → no execution, error status.
  5. Invalid tool call arguments (malformed JSON) → no execution, error status.
  6. Multiple tool calls, all valid → sequential execution with correct event ordering.
  7. Multiple tool calls, one invalid → none executed (all-or-nothing).
  8. Mid-stream `LlmEvent::Error` → partial content preserved, no `ModelFinished`.
  9. Stream ends without `Done` → same as error case.
  10. Failed tool call does not make turn status `Error` (AGENT-07).
  11. Multi-round tool loop: model → tools → model → final response.
  12. `LlmEvent::Started` is ignored (not forwarded, no double-start).
- **D-17:** One backward-compatibility regression test: existing `FakeLlm` (implements only `chat()`) still works via default `stream_chat` fallback after AgentLoop switches to `stream_chat`.
- **D-18:** Phase 14 does NOT test: `stream_turn`, public `TurnEvent`, CLI rendering, cancellation semantics, `run_turn_live`, emits-before-commit ordering.

### Claude's discretion
- Internal helper for tool call assembly state (struct shape, method names).
- Whether to extract the stream consumption loop into a private method or keep it inline in `run_turn_inner`.
- How to structure the "stream ended without Done" detection (e.g., flag after loop vs. explicit check).
- Doc comments on modified methods.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Design Source
- `.planning/alpha-v1.2-evented-runtime-design.md` — §9.3 (AgentLoop uses stream_chat), §10 (reasoning vs visible content), §11.1 (ModelStarted/ModelFinished semantics), §11.2 (usage on ModelFinished), §12 (tool call semantics), §13 (streaming delta finalization)

### Requirements
- `.planning/REQUIREMENTS.md` — AGENT-03, AGENT-04, AGENT-05, AGENT-06, AGENT-07, VER-08, VER-09

### AgentLoop (modified)
- `crates/lattice-agent/src/agent_loop/mod.rs` — Main loop switching from `chat()` to `stream_chat()`. Lines 208 (`self.config.llm.chat(chat_request).await`) is the primary change site.
- `crates/lattice-agent/src/agent_loop/traits.rs` — `LlmClient` trait with `stream_chat` default (from Phase 13)
- `crates/lattice-agent/src/agent_loop/types.rs` — `AgentTurnEvent`, `AgentTurnResult`, `AgentTurnStatus`

### RealLlmClient (modified)
- `crates/lattice-cli/src/commands/run.rs:20-33` — `RealLlmClient` gains `stream_chat` override calling `lattice_llm::stream()`

### LLM Layer (not modified, consumed)
- `crates/lattice-llm/src/types.rs:161` — `LlmEvent` enum (Token, Reasoning, ToolCallStart, ToolCallDelta, ToolCallEnd, Done, Error, Started, Citation, Signature, GroundingChunk, SafetyRating)
- `crates/lattice-llm/src/stub.rs:206` — `lattice_llm::stream(request)` convenience function (resolve + dispatch_stream)
- `crates/lattice-llm/src/client.rs:315` — `dispatch_stream(request, resolved)` returns `Result<LlmStream, LlmError>`

### Test Infrastructure (from Phase 13)
- `crates/lattice-agent/src/testing.rs` (or equivalent) — `FakeStreamingLlmClient` with `push_events`/`push_error` builder
- `crates/lattice-agent/tests/agent_loop.rs` — Existing `FakeLlm` (chat-only) must keep working via default fallback

### Phase 12 Context (carry-forward)
- `.planning/phases/12-event-and-status-vocabulary-contract/12-CONTEXT.md` — AgentTurnEvent is process-only, AgentTurnStatus uses `Error`/`Completed`/`Cancelled`

### Phase 13 Context (carry-forward)
- `.planning/phases/13-llm-streaming-trait-and-fake-streaming-client/13-CONTEXT.md` — `stream_chat` seam, FakeStreamingLlmClient builder API, two-channel error model

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `lattice_llm::stream(request)` — public convenience that does ModelRouter resolve + dispatch_stream, returns `LlmStream`. Direct call target for `RealLlmClient::stream_chat`.
- `FakeStreamingLlmClient` (Phase 13) — builder with `push_events(Vec<LlmEvent>)`, `push_error(String)`, multi-round queuing. Directly usable for Phase 14 tests.
- `LlmStream = Pin<Box<dyn Stream<Item = LlmEvent> + Send>>` — the type AgentLoop will consume via `StreamExt::next()`.
- `futures::StreamExt` — already in the dependency graph (used by `lattice_llm::chat` internally).

### Established Patterns
- Current AgentLoop round loop: build messages → call LLM → accumulate events → check tool calls → execute tools → repeat. Phase 14 preserves this structure, replacing the `chat()` call and post-response processing with stream consumption.
- `AgentTurnEvent::ModelStarted` is emitted before the LLM call (line 203-206). This stays unchanged.
- `AgentTurnEvent::ModelFinished` is emitted after successful response (line 254-257). This moves to after stream completion + tool validation.
- Error returns set `status = Error` and `terminal_reason = Some(...)`. Same pattern for stream errors.

### Integration Points
- `crates/lattice-agent/src/agent_loop/mod.rs:208` — `self.config.llm.chat(chat_request).await` → replaced by `self.config.llm.stream_chat(chat_request).await` + stream consumption loop.
- `crates/lattice-agent/src/agent_loop/mod.rs:224-257` — Post-response accumulation (usage, reasoning, token, ModelFinished) → restructured into per-event stream processing.
- `crates/lattice-agent/src/agent_loop/mod.rs:260-280` — Assistant message construction → moved to after stream finalization.
- `crates/lattice-cli/src/commands/run.rs:23-33` — `RealLlmClient` impl block gains `stream_chat` method.

</code_context>

<specifics>
## Specific Ideas

- The stream consumption loop should be a private method (e.g., `consume_model_stream`) that returns a struct with accumulated content, reasoning, tool calls, finish_reason, usage, and a success/error outcome. This keeps `run_turn_inner` readable.
- Tool call assembly state: a small internal struct (e.g., `PendingToolCall { id, name, arguments, ended: bool }`) tracked in a `Vec` during stream consumption.
- The all-or-nothing tool validation: after `Done`, check `pending_calls.iter().all(|c| c.ended && serde_json::from_str::<serde_json::Value>(&c.arguments).is_ok())`.
- For no-arg tools: if `arguments.is_empty()`, normalize to `"{}"` before JSON validation. This prevents false rejection of legitimate no-argument tool calls.

</specifics>

<deferred>
## Deferred Ideas

- **`run_turn_live` with bounded channel** — Phase 15 adds live event forwarding. Phase 14's stream consumption loop will naturally compose with a sender, but the sender interface is not introduced here.
- **Cooperative cancellation during stream consumption** — Phase 15 scope. Phase 14 preserves the existing pre-round cancel check but does not check cancel mid-stream.
- **Partial progress preservation on timeout** — STATE.md todo notes this for Phase 14/17. Timeout currently returns empty messages. Preserving partial content on timeout requires the stream consumer to be cancel-safe or use a separate task. Defer to Phase 15/17 where the live path architecture makes this natural.
- **`LlmEvent` vocabulary rename** (Token→TokenDelta, Done→ModelDone, etc.) — deferred indefinitely per Phase 13 D-05.
- **Stream backpressure / bounded buffering** — not needed while consumption is sequential within a single task.

</deferred>

---

*Phase: 14-AgentLoop Streaming Delta Engine*
*Context gathered: 2026-05-21*
