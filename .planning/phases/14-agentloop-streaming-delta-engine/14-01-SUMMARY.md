---
phase: 14-agentloop-streaming-delta-engine
plan: 01
subsystem: agent
tags: [streaming, delta-engine, tool-call-assembly, serde_json, futures]

# Dependency graph
requires:
  - phase: 13-llm-streaming-seam
    provides: LlmClient::stream_chat default buffered fallback, FakeStreamingLlmClient, LlmStream type, LlmEvent enum
provides:
  - AgentLoop streaming consumption engine replacing buffered chat()
  - consume_model_stream async helper with Done/Error/Exhausted outcomes
  - PendingToolCall assembly from ToolCallStart/Delta/End stream events
  - All-or-nothing tool call JSON validation via serde_json
  - RealLlmClient::stream_chat override calling lattice_llm::stream()
  - Partial content preservation on stream Error and Exhausted paths
affects: [14-02-live-stream-turn, cli-rendering]

# Tech tracking
tech-stack:
  added: [serde_json workspace dep in lattice-agent]
  patterns: [stream-consumption-engine, pending-tool-call-assembly, all-or-nothing-validation]

key-files:
  created: []
  modified:
    - crates/lattice-agent/Cargo.toml
    - crates/lattice-agent/src/agent_loop/mod.rs
    - crates/lattice-cli/src/commands/run.rs

key-decisions:
  - "consume_model_stream is async (not sync) because stream.next().await requires async context"
  - "PendingToolCall.ended is diagnostic-only; Done + valid JSON is sufficient for execution eligibility"
  - "Empty tool call arguments normalized to {} before serde_json validation"

patterns-established:
  - "Stream consumption: iterate LlmStream, match LlmEvent variants, accumulate into StreamResult"
  - "All-or-nothing validation: reject entire tool call batch if any single argument fails serde_json parse"
  - "Partial preservation: on Error/Exhausted, push partial GraphMessage (content+reasoning, no tool_calls), skip ModelFinished"

requirements-completed: [AGENT-03, AGENT-04, AGENT-05, AGENT-06, AGENT-07]

# Metrics
duration: 9min
completed: 2026-05-21
---

# Phase 14 Plan 01: AgentLoop Streaming Delta Engine Summary

**Streaming delta engine replacing buffered chat() with per-chunk stream consumption, tool call assembly from stream events, all-or-nothing JSON validation, and partial content preservation on errors**

## Performance

- **Duration:** 9 min
- **Started:** 2026-05-21T12:36:44Z
- **Completed:** 2026-05-21T12:45:09Z
- **Tasks:** 2
- **Files modified:** 4 (Cargo.toml, mod.rs, run.rs, Cargo.lock)

## Accomplishments
- AgentLoop now calls stream_chat() instead of chat() for model requests, accumulating Token/Reasoning deltas per-chunk
- Tool calls assembled from ToolCallStart/Delta/End stream events into PendingToolCall structs with diagnostic `ended` tracking
- All-or-nothing JSON validation via serde_json::from_str before any tool execution; empty arguments normalized to "{}"
- Stream Error/Exhausted paths preserve partial assistant content in GraphMessage and skip ModelFinished emission
- RealLlmClient overrides stream_chat to delegate to lattice_llm::stream() for real provider streaming

## Task Commits

Each task was committed atomically:

1. **Task 1: Add serde_json dependency and implement stream consumption engine in AgentLoop** - `0877ab9` (feat)
2. **Task 2: Add RealLlmClient stream_chat override in lattice-cli** - `cc16a1e` (feat)

## Files Created/Modified
- `crates/lattice-agent/Cargo.toml` - Added serde_json workspace dependency
- `crates/lattice-agent/src/agent_loop/mod.rs` - Stream consumption engine replacing buffered chat(); PendingToolCall, StreamOutcome, StreamResult types; consume_model_stream async method; all-or-nothing tool call validation; partial content preservation on Error/Exhausted
- `crates/lattice-cli/src/commands/run.rs` - RealLlmClient::stream_chat override calling lattice_llm::stream()
- `Cargo.lock` - Updated lockfile for serde_json addition

## Decisions Made
- Made consume_model_stream async (not sync) since stream.next().await requires async context -- plan specified a private helper method but did not explicitly state async, though the semantics require it
- PendingToolCall.ended field tracks ToolCallEnd receipt for diagnostics only; Done + valid JSON is sufficient for execution eligibility, matching provider variance where some skip ToolCallEnd
- Empty tool call arguments normalized to "{}" before serde_json validation, written back into PendingToolCall.arguments so the normalized value flows to both transcript (GraphToolCall.arguments) and execution input (ToolCall.function.arguments) with no divergence

## Deviations from Plan

None - plan executed exactly as written. The only minor adjustment was making consume_model_stream async, which is implicit in the semantics (it consumes an async stream) rather than an explicit deviation.

## Issues Encountered
- Initial implementation had consume_model_stream as a synchronous fn, causing an E0728 compiler error ("await only allowed inside async functions"). Fixed by adding async and .await at the call site.
- Initial struct/enum/type definitions were placed inside the first impl AgentLoop block due to indentation mismatch from the edit. Fixed by restructuring the file so types are at module level and methods are in a single impl block.

## Next Phase Readiness
- AgentLoop streaming consumption is complete and all existing tests pass (28 lattice-agent, 109 lattice-cli)
- Ready for Plan 14-02 which will implement live stream_turn with bounded channel and deterministic fake streaming tests
- FakeLlmClient/StubLlmClient continue working via the default buffered stream_chat fallback from Phase 13

---
*Phase: 14-agentloop-streaming-delta-engine*
*Completed: 2026-05-21*