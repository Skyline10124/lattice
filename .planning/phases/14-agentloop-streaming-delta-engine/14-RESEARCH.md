# Phase 14: AgentLoop Streaming Delta Engine - Research

**Researched:** 2026-05-21
**Domain:** Rust async streaming consumption, agent loop state machine refactoring
**Confidence:** HIGH

## Summary

Phase 14 replaces AgentLoop's buffered `chat()` call with streaming `stream_chat()` consumption. The core change is in `crates/lattice-agent/src/agent_loop/mod.rs` line 208: `self.config.llm.chat(chat_request)` becomes `self.config.llm.stream_chat(chat_request)` followed by a per-event consumption loop that accumulates content, reasoning, and tool calls from `LlmEvent` variants.

The implementation is well-constrained: all types (`LlmEvent`, `LlmStream`, `FakeStreamingLlmClient`, `AgentTurnEvent`) already exist from Phases 12-13. The new work is (1) the stream consumption loop with delta accumulation, (2) tool call assembly with all-or-nothing JSON validation, (3) error/partial-content preservation, and (4) `RealLlmClient::stream_chat` override in `lattice-cli`. One new dependency is needed: `serde_json` for tool call argument validation.

**Primary recommendation:** Extract stream consumption into a private `consume_model_stream` method returning a result struct, keeping `run_turn_inner` readable. The tool call assembly state machine is a small `PendingToolCall` struct tracked in a `Vec`.

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| AGENT-03 | AgentLoop consumes `stream_chat` and emits ModelStarted, per-chunk TokenDelta, per-chunk ReasoningDelta, and ModelFinished | Stream consumption loop replaces chat() call; events emitted per LlmEvent variant |
| AGENT-04 | Streaming deltas finalized into transcript GraphMessage with visible content separate from reasoning | Two accumulators (`assistant_content`, `reasoning_content`) finalized on Done into GraphMessage |
| AGENT-05 | Partial content preserved on LLM stream error; incomplete tool calls omitted from transcript | Error/stream-exhaustion paths flush partial content, skip tool_calls field |
| AGENT-06 | Tool execution sequential, ToolCallStarted emitted only before real execution | Tool calls assembled from stream events, validated as batch, then executed one-by-one with event emission |
| AGENT-07 | Failed tool call does not make turn status Error | Tool execution error produces tool result message; loop continues to next round |
| VER-08 | Tests prove token deltas commit as one assistant message and reasoning doesn't pollute visible output | FakeStreamingLlmClient multi-chunk tests verify accumulation and separation |
| VER-09 | Tests prove tool-call event timing, failed-tool recovery, and sequential execution | 12 test scenarios covering assembly, validation, execution ordering, and failure modes |
</phase_requirements>

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- D-01: Override `RealLlmClient::stream_chat` to call `lattice_llm::stream(request)`. Error mapping: `LlmError` -> `Box<dyn Error + Send + Sync>`.
- D-02: Default `stream_chat` fallback remains for legacy/test clients. `FakeLlm` keeps working.
- D-03: AgentLoop switches from `self.config.llm.chat(chat_request)` to `self.config.llm.stream_chat(chat_request)`.
- D-04: `LlmEvent::Started` is ignored (no-op).
- D-05: Provider metadata events (Citation, Signature, GroundingChunk, SafetyRating) are ignored.
- D-06: Two separate accumulators: `assistant_content` and `reasoning_content`. Token -> append + emit TokenDelta. Reasoning -> append + emit ReasoningDelta.
- D-07: On Done, finalize into GraphMessage with content, reasoning_content, tool_calls.
- D-08: Tool calls assembled from ToolCallStart/ToolCallDelta/ToolCallEnd into pending list.
- D-09: All-or-nothing validation after Done. Empty args normalized to "{}". JSON validation via serde_json. Invalid -> none executed, Error status.
- D-10: ToolCallStarted emitted only immediately before real execution.
- D-10b: `lattice-agent` gains `serde_json = { workspace = true }` dependency.
- D-11: On LlmEvent::Error mid-stream: flush partial content, no ModelFinished, Error status.
- D-12: Stream exhaustion without Done: same as D-11.
- D-13: Partial messages with only reasoning still committed (content = "", reasoning_content = Some(...)).
- D-14: ModelFinished only on Done + valid response.
- D-15: ModelFinished.usage from Done { usage }. None is acceptable.
- D-16: Tests use FakeStreamingLlmClient with `--features test-utils`. 12 required test scenarios.
- D-17: Backward-compatibility regression test: FakeLlm (chat-only) still works via default fallback.
- D-18: Phase 14 does NOT test: stream_turn, public TurnEvent, CLI rendering, cancellation semantics, run_turn_live, emits-before-commit ordering.

### Claude's Discretion
- Internal helper struct shape for tool call assembly (struct name, method names).
- Whether to extract stream consumption loop into a private method or keep inline.
- How to structure "stream ended without Done" detection.
- Doc comments on modified methods.

### Deferred Ideas (OUT OF SCOPE)
- `run_turn_live` with bounded channel (Phase 15).
- Cooperative cancellation during stream consumption (Phase 15).
- Partial progress preservation on timeout (Phase 15/17).
- `LlmEvent` vocabulary rename (deferred indefinitely).
- Stream backpressure / bounded buffering.
</user_constraints>

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Stream consumption loop | Agent (lattice-agent) | -- | AgentLoop owns the model call lifecycle |
| Delta accumulation & finalization | Agent (lattice-agent) | -- | Transcript assembly is agent-layer responsibility |
| Tool call assembly from stream events | Agent (lattice-agent) | -- | Tool validation is pre-execution agent logic |
| Tool execution | Agent (lattice-agent) | -- | Sequential execution via ToolCallExecutor trait |
| RealLlmClient stream_chat override | CLI (lattice-cli) | -- | Production wiring lives in the binary crate |
| LlmStream production | LLM (lattice-llm) | -- | Already exists; consumed, not modified |
| GraphMessage persistence | Session (lattice-session) | -- | Types consumed, not modified |

## Standard Stack

### Core (already in workspace)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| futures | 0.3 | `StreamExt::next()` for async stream consumption | Already a dependency of lattice-agent |
| serde_json | 1.x | Tool call argument JSON validation | Workspace dep, used by lattice-llm and lattice-session |
| async-trait | workspace | Trait async method support | Already used for LlmClient |
| tokio | workspace | Async runtime | Already the project runtime |

### No New External Dependencies
Phase 14 adds only `serde_json` (already in workspace) to `lattice-agent/Cargo.toml`. All other dependencies are already present.

**Installation (Cargo.toml change):**
```toml
# crates/lattice-agent/Cargo.toml [dependencies]
serde_json = { workspace = true }
```

## Package Legitimacy Audit

| Package | Registry | Age | Downloads | Source Repo | slopcheck | Disposition |
|---------|----------|-----|-----------|-------------|-----------|-------------|
| serde_json | crates.io | 9+ yrs | 200M+ | github.com/serde-rs/json | N/A | Approved |

**Packages removed due to slopcheck [SLOP] verdict:** none
**Packages flagged as suspicious [SUS]:** none

*serde_json is already a workspace dependency used by lattice-llm and lattice-session. No new external packages are introduced.*

## Architecture Patterns

### System Architecture Diagram

```
AgentTurnRequest
       |
       v
[AgentLoop::run_turn_inner]
       |
       |--- build_messages() --> Vec<GraphMessage>
       |
       v  (per round)
[stream_chat(ChatRequest)] --> Result<LlmStream, Error>
       |                              |
       | Err -> Error status          | Ok
       |                              v
       |                    [consume_model_stream(stream)]
       |                              |
       |              +---------------+---------------+
       |              |               |               |
       |         LlmEvent::Token  LlmEvent::Reasoning  LlmEvent::ToolCall*
       |              |               |               |
       |              v               v               v
       |         append to        append to       PendingToolCall
       |         assistant_content reasoning_content  assembly
       |              |               |               |
       |              +-------+-------+               |
       |                      |                       |
       |                      v                       v
       |              LlmEvent::Done -----> validate tool calls
       |                      |                  |         |
       |                      |              all valid   any invalid
       |                      |                  |         |
       |                      v                  v         v
       |              emit ModelFinished    execute    Error status
       |              finalize GraphMessage  tools     (no execution)
       |                      |                  |
       |                      v                  v
       |              [no tool calls]     [tool results -> next round]
       |                      |
       v                      v
AgentTurnResult         AgentTurnResult
(Completed)             (continue loop)
```

### Recommended Project Structure

No new files beyond the existing structure. Changes are in-place:

```
crates/lattice-agent/
├── src/
│   ├── agent_loop/
│   │   ├── mod.rs          # Main change: stream consumption replaces chat()
│   │   ├── traits.rs       # Unchanged (stream_chat already defined)
│   │   └── types.rs        # Unchanged (AgentTurnEvent already has all variants)
│   ├── testing.rs          # Unchanged (FakeStreamingLlmClient ready)
│   └── lib.rs              # Unchanged
├── tests/
│   ├── agent_loop.rs       # Existing chat-based tests (backward compat)
│   └── streaming_delta.rs  # NEW: Phase 14 behavioral tests
└── Cargo.toml              # Add serde_json dependency

crates/lattice-cli/
└── src/commands/run.rs     # Add stream_chat override to RealLlmClient
```

### Pattern 1: Stream Consumption with Accumulation

**What:** Consume `LlmStream` event-by-event, accumulating content and tool calls into a result struct.
**When to use:** Every model request within the agent loop.
**Example:**
```rust
// Source: CONTEXT.md D-03, D-06, D-07
struct StreamResult {
    assistant_content: String,
    reasoning_content: String,
    tool_calls: Vec<AssembledToolCall>,
    usage: Option<TurnUsage>,
    finish_reason: Option<String>,
    outcome: StreamOutcome,
}

enum StreamOutcome {
    Done,
    Error(String),
    Exhausted, // stream ended without Done
}

async fn consume_model_stream(
    &self,
    stream: LlmStream,
    events: &mut Vec<AgentTurnEvent>,
) -> StreamResult {
    use futures::StreamExt;

    let mut assistant_content = String::new();
    let mut reasoning_content = String::new();
    let mut pending_calls: Vec<PendingToolCall> = Vec::new();
    let mut usage = None;
    let mut finish_reason = None;

    while let Some(event) = stream.next().await {
        match event {
            LlmEvent::Token { content } => {
                assistant_content.push_str(&content);
                events.push(AgentTurnEvent::TokenDelta { content });
            }
            LlmEvent::Reasoning { content } => {
                reasoning_content.push_str(&content);
                events.push(AgentTurnEvent::ReasoningDelta { content });
            }
            LlmEvent::ToolCallStart { id, name } => {
                pending_calls.push(PendingToolCall {
                    id, name, arguments: String::new(), ended: false,
                });
            }
            LlmEvent::ToolCallDelta { id, arguments_delta } => {
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    call.arguments.push_str(&arguments_delta);
                }
            }
            LlmEvent::ToolCallEnd { id } => {
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    call.ended = true;
                }
            }
            LlmEvent::Done { finish_reason: fr, usage: u } => {
                finish_reason = Some(fr);
                usage = u;
                // break out — Done is terminal
                return StreamResult { /* ... outcome: StreamOutcome::Done */ };
            }
            LlmEvent::Error { message } => {
                return StreamResult { /* ... outcome: StreamOutcome::Error(message) */ };
            }
            // D-04, D-05: ignore Started and provider metadata
            LlmEvent::Started
            | LlmEvent::Citation { .. }
            | LlmEvent::Signature { .. }
            | LlmEvent::GroundingChunk { .. }
            | LlmEvent::SafetyRating { .. } => {}
        }
    }
    // Stream exhausted without Done (D-12)
    StreamResult { /* ... outcome: StreamOutcome::Exhausted */ }
}
```

### Pattern 2: Tool Call Assembly and All-or-Nothing Validation

**What:** Assemble tool calls from stream events, then validate as a batch before execution.
**When to use:** After stream Done when pending tool calls exist.
**Example:**
```rust
// Source: CONTEXT.md D-08, D-09
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
    ended: bool,
}

fn validate_tool_calls(pending: &mut [PendingToolCall]) -> Result<Vec<GraphToolCall>, String> {
    for call in pending.iter_mut() {
        // Normalize empty arguments for no-arg tools
        if call.arguments.is_empty() {
            call.arguments = "{}".to_string();
        }
        // Validate JSON
        if serde_json::from_str::<serde_json::Value>(&call.arguments).is_err() {
            return Err(format!(
                "invalid/incomplete tool call: {} (id={})",
                call.name, call.id
            ));
        }
        // If not ended by ToolCallEnd, check if arguments are valid JSON
        // (Done may close a still-open call if args are valid)
        if !call.ended {
            // Already validated above — if we got here, it's valid JSON
            // so Done implicitly closes it (per D-09 rule 1)
        }
    }
    Ok(pending.iter().map(|c| GraphToolCall {
        id: c.id.clone(),
        name: c.name.clone(),
        arguments: c.arguments.clone(),
    }).collect())
}
```

### Pattern 3: Partial Content Preservation on Error

**What:** On stream error or exhaustion, flush accumulated content into a partial GraphMessage.
**When to use:** D-11, D-12 error paths.
**Example:**
```rust
// Source: CONTEXT.md D-11, D-12, D-13
fn flush_partial_message(
    assistant_content: &str,
    reasoning_content: &str,
) -> Option<GraphMessage> {
    if assistant_content.is_empty() && reasoning_content.is_empty() {
        return None;
    }
    Some(GraphMessage {
        role: "assistant".to_string(),
        content: assistant_content.to_string(),
        reasoning_content: if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content.to_string())
        },
        tool_calls: None, // Never include incomplete tool calls
        tool_call_id: None,
        name: None,
    })
}
```

### Anti-Patterns to Avoid
- **Synthesizing ModelFinished on error:** Never emit ModelFinished unless Done is received AND response is valid. Error paths must skip it entirely.
- **Emitting ToolCallStarted during stream assembly:** ToolCallStarted is an agent-level event for real execution, not stream assembly. Emit only before `tool_executor.execute()`.
- **Accepting missing Done as normal completion:** Unlike Phase 13's buffered fallback (which defaults to "stop"), Phase 14's streaming path treats missing Done as Error.
- **Partial tool call execution:** If any tool call in a batch is invalid, execute none. Never execute a subset.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| JSON validation | Custom JSON parser | `serde_json::from_str::<Value>()` | Edge cases in JSON parsing are numerous |
| Async stream iteration | Manual poll/waker | `futures::StreamExt::next()` | Correct async stream consumption is subtle |
| Stream type boxing | Custom trait objects | `Pin<Box<dyn Stream<Item=LlmEvent> + Send>>` (LlmStream) | Already defined, standard pattern |

**Key insight:** The streaming infrastructure (LlmStream, LlmEvent, FakeStreamingLlmClient) is already built. Phase 14 is purely a consumer-side change.

## Common Pitfalls

### Pitfall 1: Double ModelStarted Emission
**What goes wrong:** Emitting ModelStarted both before `stream_chat` AND when receiving `LlmEvent::Started` from the stream.
**Why it happens:** `LlmEvent::Started` exists in the stream but AgentLoop already emits its own `ModelStarted` before the call.
**How to avoid:** D-04 explicitly says ignore `LlmEvent::Started`. The agent-level `ModelStarted` is emitted at line 203-206 (before the stream call), unchanged.
**Warning signs:** Tests showing two ModelStarted events per round.

### Pitfall 2: Tool Call ID Mismatch in Delta Assembly
**What goes wrong:** `ToolCallDelta` with an ID that doesn't match any pending call gets silently dropped.
**Why it happens:** Provider streams may interleave tool call deltas in unexpected order.
**How to avoid:** Use `find()` on pending_calls by ID. If not found, the delta is a no-op (defensive). Real providers always send Start before Delta.
**Warning signs:** Tool calls with empty arguments despite provider sending deltas.

### Pitfall 3: Forgetting to Set assistant_output on Error Path
**What goes wrong:** `AgentTurnResult.assistant_output` is empty string on error even though partial content was accumulated.
**Why it happens:** Error path returns early without setting `assistant_output = assistant_content`.
**How to avoid:** D-11 explicitly states: "Set `assistant_output` = accumulated visible content (do not clear it)."
**Warning signs:** Tests for partial content preservation failing.

### Pitfall 4: ModelFinished with Invalid Tool Calls
**What goes wrong:** Emitting ModelFinished after Done arrives, then discovering tool calls are invalid.
**Why it happens:** Natural to emit ModelFinished immediately on Done, before validation.
**How to avoid:** D-14: ModelFinished is emitted only after Done AND validation passes. Sequence: Done -> validate -> (if valid) emit ModelFinished -> execute tools.
**Warning signs:** ModelFinished appearing in events for turns that end in Error due to invalid tool calls.

### Pitfall 5: Existing Tests Breaking After chat() -> stream_chat() Switch
**What goes wrong:** Existing `agent_loop.rs` tests use `FakeLlm` which only implements `chat()`. After switching AgentLoop to call `stream_chat()`, these tests would break.
**Why it happens:** `FakeLlm` doesn't override `stream_chat`, so it uses the default fallback which calls `chat()`.
**How to avoid:** The default `stream_chat` fallback in `traits.rs` calls `self.chat()` internally. As long as `FakeLlm` implements `chat()`, the fallback works. D-17 requires a regression test proving this.
**Warning signs:** All existing agent_loop.rs tests failing after the switch.

### Pitfall 6: Usage Accumulation Type Mismatch
**What goes wrong:** `TokenUsage` uses `u32` fields but `TurnUsage` uses `u64`. Casting incorrectly.
**Why it happens:** Two different types in two different crates.
**How to avoid:** Cast with `as u64` when converting `TokenUsage` -> `TurnUsage` (same pattern as current code line 225-228).
**Warning signs:** Compilation errors or truncation.

## Code Examples

### RealLlmClient stream_chat Override

```rust
// Source: CONTEXT.md D-01, crates/lattice-cli/src/commands/run.rs
#[async_trait::async_trait]
impl lattice_agent::agent_loop::LlmClient for RealLlmClient {
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
    {
        lattice_llm::chat(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }

    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>
    {
        lattice_llm::stream(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }
}
```

### Test: Multi-chunk Token Assembly

```rust
// Source: CONTEXT.md D-16 scenario 1
#[tokio::test]
async fn streaming_multi_chunk_tokens_assemble_into_one_message() {
    let client = Arc::new(FakeStreamingLlmClient::builder()
        .push_events(vec![
            LlmEvent::Token { content: "p".to_string() },
            LlmEvent::Token { content: "ong".to_string() },
            LlmEvent::Done { finish_reason: "stop".to_string(), usage: None },
        ])
        .build());

    let executor = Arc::new(/* ... */);
    let loop_inst = AgentLoop::new(AgentLoopConfig { llm: client, tool_executor: executor });
    let request = make_request("ping", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "pong");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].content, "pong");
}
```

### Test: Reasoning Separate from Visible Content

```rust
// Source: CONTEXT.md D-16 scenario 2
#[tokio::test]
async fn streaming_reasoning_separate_from_visible() {
    let client = Arc::new(FakeStreamingLlmClient::builder()
        .push_events(vec![
            LlmEvent::Reasoning { content: "think".to_string() },
            LlmEvent::Token { content: "answer".to_string() },
            LlmEvent::Done { finish_reason: "stop".to_string(), usage: None },
        ])
        .build());

    // ...
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.assistant_output, "answer");
    assert_eq!(result.messages[0].content, "answer");
    assert_eq!(result.messages[0].reasoning_content, Some("think".to_string()));
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Buffered `chat()` in AgentLoop | Streaming `stream_chat()` with per-chunk events | Phase 14 (now) | Enables real-time token delivery to consumers |
| Single ChatResponse processing | Event-by-event LlmStream consumption | Phase 14 (now) | Finer-grained error handling and partial content preservation |
| Tool calls from ChatResponse.tool_calls | Tool calls assembled from stream events | Phase 14 (now) | Enables all-or-nothing validation before execution |

**Deprecated/outdated:**
- `self.config.llm.chat(chat_request)` in AgentLoop: replaced by `stream_chat` (though `chat()` remains on the trait for backward compat).

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| -- | -- | -- | -- |

**All claims in this research were verified by reading the actual codebase. No user confirmation needed.**

## Open Questions

1. **Non_empty_option helper**
   - What we know: CONTEXT.md D-07 references `non_empty_option(reasoning_content)` but no such helper exists in the codebase.
   - What's unclear: Whether to create a helper or inline the check.
   - Recommendation: Inline `if reasoning_content.is_empty() { None } else { Some(reasoning_content) }` — matches existing patterns in `testing.rs` line 160-163 and `stub.rs` line 190-193. Claude's discretion.

2. **Existing tests after stream_chat switch**
   - What we know: `agent_loop.rs` tests use `FakeLlm` which only implements `chat()`. The default `stream_chat` fallback calls `chat()` internally.
   - What's unclear: Nothing — this is confirmed to work by design.
   - Recommendation: D-17 regression test confirms this explicitly.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (built-in) |
| Config file | Cargo.toml features: `test-utils` |
| Quick run command | `cargo test -p lattice-agent --features test-utils` |
| Full suite command | `cargo test -p lattice-agent --features test-utils && cargo test -p lattice-cli && cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| AGENT-03 | Stream consumption emits TokenDelta/ReasoningDelta/ModelFinished | unit | `cargo test -p lattice-agent --features test-utils streaming_multi_chunk` | Wave 0 |
| AGENT-04 | Deltas finalize into GraphMessage with separate reasoning | unit | `cargo test -p lattice-agent --features test-utils streaming_reasoning_separate` | Wave 0 |
| AGENT-05 | Partial content preserved on error, no incomplete tool calls | unit | `cargo test -p lattice-agent --features test-utils streaming_error_preserves_partial` | Wave 0 |
| AGENT-06 | Sequential tool execution, ToolCallStarted before execution only | unit | `cargo test -p lattice-agent --features test-utils streaming_tool_call_sequential` | Wave 0 |
| AGENT-07 | Failed tool call doesn't make turn Error | unit | `cargo test -p lattice-agent --features test-utils streaming_failed_tool_not_error` | Wave 0 |
| VER-08 | Token deltas -> one message, reasoning not in visible | unit | `cargo test -p lattice-agent --features test-utils streaming_multi_chunk` | Wave 0 |
| VER-09 | Tool-call timing, failed-tool recovery, sequential execution | unit | `cargo test -p lattice-agent --features test-utils streaming_tool` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p lattice-agent --features test-utils && cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings`
- **Per wave merge:** `cargo test && cargo clippy -- -D warnings && cargo fmt --all --check`
- **Phase gate:** Full suite green before verification

### Wave 0 Gaps
- [ ] `crates/lattice-agent/tests/streaming_delta.rs` -- all 12 test scenarios from D-16
- [ ] `serde_json = { workspace = true }` in `crates/lattice-agent/Cargo.toml`

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | -- |
| V3 Session Management | no | -- |
| V4 Access Control | no | -- |
| V5 Input Validation | yes | serde_json for tool call argument validation (D-09) |
| V6 Cryptography | no | -- |

### Known Threat Patterns for Agent Loop Streaming

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Malformed tool call arguments injection | Tampering | JSON validation via serde_json before execution (D-09) |
| Infinite stream / resource exhaustion | Denial of Service | MAX_TURN_ROUNDS cap + timeout (existing) |
| Partial content leakage on error | Information Disclosure | Partial content is committed to session graph (by design, not leaked externally) |

## Environment Availability

Step 2.6: SKIPPED (no external dependencies identified -- purely code/config changes within the Rust workspace).

## Sources

### Primary (HIGH confidence)
- Codebase inspection: `crates/lattice-agent/src/agent_loop/mod.rs` -- current AgentLoop implementation
- Codebase inspection: `crates/lattice-agent/src/agent_loop/traits.rs` -- LlmClient trait with stream_chat
- Codebase inspection: `crates/lattice-agent/src/testing.rs` -- FakeStreamingLlmClient
- Codebase inspection: `crates/lattice-llm/src/types.rs` -- LlmEvent enum, TokenUsage
- Codebase inspection: `crates/lattice-llm/src/stub.rs` -- LlmStream type, stream() function
- Codebase inspection: `crates/lattice-session/src/graph.rs` -- GraphMessage, GraphToolCall
- Codebase inspection: `Cargo.toml` -- workspace dependencies (serde_json, futures)

### Secondary (MEDIUM confidence)
- None needed -- all information sourced from codebase and CONTEXT.md

### Tertiary (LOW confidence)
- None

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all dependencies already in workspace, verified in Cargo.toml
- Architecture: HIGH -- all types and seams exist from Phase 12-13, change sites identified precisely
- Pitfalls: HIGH -- derived from actual code structure and CONTEXT.md constraints

**Research date:** 2026-05-21
**Valid until:** 2026-06-21 (stable internal architecture, no external dependency drift risk)
