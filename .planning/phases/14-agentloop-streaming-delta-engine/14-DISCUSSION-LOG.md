# Phase 14: AgentLoop Streaming Delta Engine - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-21
**Phase:** 14-AgentLoop Streaming Delta Engine
**Areas discussed:** RealLlmClient stream wiring, Stream error partial content, Incomplete tool call handling, Test strategy

---

## RealLlmClient Stream Wiring

| Option | Description | Selected |
|--------|-------------|----------|
| Override in Phase 14 | Wire `RealLlmClient::stream_chat` to `lattice_llm::stream()` now, giving production real per-chunk streaming | ✓ |
| Defer to later phase | Keep RealLlmClient on default fallback; Phase 14 only tests via FakeStreamingLlmClient | |

**User's choice:** Override in Phase 14. Narrow scope: only the stream_chat override, no provider/router/API/CLI changes.
**Notes:** User rationale: without this, production path is still "fake streaming" — AgentLoop code consumes stream but real provider tokens are still aggregated by `chat()` then wrapped. `lattice_llm::stream(request)` convenience function already exists with same pattern as `chat()`.

---

## Stream Error Partial Content

| Option | Description | Selected |
|--------|-------------|----------|
| Flush partial to transcript | Accumulated content/reasoning → partial GraphMessage, preserve assistant_output | ✓ |
| Discard on error | Clear all partial content, return empty messages on stream failure | |

**User's choice:** Flush partial to transcript. "错误 turn 也是真历史。如果模型已经输出了半句，后面断流，这半句就应该是错误 turn 的一部分。"
**Notes:** Applies to both `LlmEvent::Error` mid-stream and stream exhaustion without `Done`. No `ModelFinished` emitted in either case. `assistant_output` reflects accumulated visible content (not cleared).

---

## Incomplete Tool Call Handling

| Option | Description | Selected |
|--------|-------------|----------|
| All-or-nothing validation | Assemble all tool calls, validate as batch; if any invalid, execute none | ✓ |
| Execute valid prefix | Execute complete/valid tool calls, skip malformed ones | |
| Per-call validation | Validate and execute each tool call independently | |

**User's choice:** All-or-nothing per model response.
**Notes:** User rationale: "工具有副作用，整个 model response 已经 malformed，执行 prefix 会让历史变得很怪。" Rules: (1) All must have ToolCallEnd or be terminated by Done. (2) Empty arguments normalized to `"{}"`. (3) Arguments must be valid JSON. (4) Any failure → no execution, no malformed tool_calls committed, assistant content/reasoning only, status Error.

---

## Test Strategy

| Option | Description | Selected |
|--------|-------------|----------|
| FakeStreamingLlmClient primary | Use multi-chunk fake for all behavioral tests, one fallback regression | ✓ |
| Mixed fake + default | Split tests between fake streaming and default fallback | |

**User's choice:** FakeStreamingLlmClient for all Phase 14 behavioral tests. One backward-compatibility regression test for existing chat-only clients via default fallback.
**Notes:** Phase 13 already tested the default fallback seam exhaustively. Phase 14 tests should prove AgentLoop is truly consuming per-chunk stream. Not testing: stream_turn, public TurnEvent, CLI, cancellation.

---

## Claude's Discretion

- Internal helper struct for tool call assembly state
- Whether to extract stream consumption into a private method vs inline
- "Stream ended without Done" detection mechanism
- Doc comments on modified methods

## Deferred Ideas

- **`run_turn_live` with bounded channel** — Phase 15
- **Cooperative cancellation during stream consumption** — Phase 15
- **Partial progress preservation on timeout** — Phase 15/17
- **`LlmEvent` vocabulary rename** — indefinitely deferred
- **Stream backpressure** — not needed in single-task sequential consumption
