---
phase: 09-agent-loop-tools-and-api-facade
plan: 03
subsystem: agent
tags: [agent-loop, llm-client, tool-executor, sequential-tools, timeout, cancel, events]

# Dependency graph
requires:
  - phase: 08-session-graph-core
    provides: HeadTranscript, TurnUsage, GraphMessage types for transcript reads
  - phase: 09-agent-loop-tools-and-api-facade
    plan: 01
    provides: HeadTranscript and TurnUsage in lattice-session
  - phase: 09-agent-loop-tools-and-api-facade
    plan: 02
    provides: resolve_enabled_tools and for_project_root executor
provides:
  - Stable v1.1 AgentLoop main-path implementation
  - Injectable LlmClient and ToolCallExecutor traits for test doubles
  - Sequential tool loop with full transcript messages, usage, and events
  - Timeout and cancel support as run protection (D-09/D-10)
affects: [09-04, api-facade, cli]

# Tech tracking
tech-stack:
  added: [lattice-llm dependency in lattice-agent, lattice-session dependency in lattice-agent]
  patterns: [injectable-llm-seam, injectable-tool-executor-seam, repeat-until-final-loop, cancel-flag-atomic-bool]

key-files:
  created:
    - crates/lattice-agent/src/agent_loop.rs
  modified:
    - crates/lattice-agent/Cargo.toml
    - crates/lattice-agent/src/lib.rs

key-decisions:
  - "AgentLoop uses injectable LlmClient and ToolCallExecutor traits for test doubles (D-01..D-07)"
  - "No fixed tool-round counter; loop repeats until final assistant response or terminal outcome (D-08)"
  - "External timeout via tokio::time::timeout and cancel via Arc<AtomicBool> as run protection (D-09/D-10)"
  - "ToolCallExecutor trait accepts lattice_core::types::ToolCall to bridge with existing DefaultToolExecutor"
  - "Full tool results persisted as GraphMessage, not summarized or clipped (D-23..D-26)"

patterns-established:
  - "Injectable LLM seam: LlmClient async trait with production/test implementations"
  - "Injectable tool seam: ToolCallExecutor async trait wrapping DefaultToolExecutor"
  - "Repeat-until-final loop: no round counter, external timeout/cancel only"
  - "Cancel flag: Arc<AtomicBool> checked between LLM rounds"

requirements-completed: ["AGENT-01", "AGENT-02", "TOOL-01", "TOOL-02", "TOOL-04"]

# Metrics
duration: 5min
completed: 2026-05-16
---

# Phase 9 Plan 03: Agent Loop Summary

**Stable v1.1 AgentLoop with injectable LLM/tool seams, sequential tool execution, usage accumulation, timeout/cancel, and event emission**

## Performance

- **Duration:** 5 min
- **Started:** 2026-05-16T21:39:04Z
- **Completed:** 2026-05-16T21:44:15Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments

- Created `agent_loop` module with stable public names: `AgentLoop`, `AgentLoopConfig`, `AgentTurnRequest`, `AgentTurnResult`, `AgentTurnEvent`, `AgentTurnStatus`, `AgentToolResultSummary`
- Implemented repeat-until-final LLM/tool loop that builds messages from transcript + system prompt + user input, calls the LLM, executes tool calls sequentially, and repeats until final assistant response
- Added injectable `LlmClient` and `ToolCallExecutor` async traits for deterministic fake LLM tests without provider credentials
- Added timeout support via `tokio::time::timeout` and cancel support via `Arc<AtomicBool>` as external run protection (D-09/D-10)
- Full tool results persisted as `GraphMessage` with no summarization, clipping, or blob references (D-23..D-26)
- Token usage accumulated across all LLM rounds into `TurnUsage`
- Events emitted for Started, Token, ToolCallStart, ToolCallResult, Done, Error, Cancelled
- Added TODO Phase 11 delete legacy Agent path marker (D-07)

## Task Commits

Each task was committed atomically:

1. **Task 1: Define AgentLoop contracts and injectable LLM/tool seams** - `689495a` (feat)
2. **Task 2: Implement sequential tool loop, statuses, usage, and events** - `eb371d6` (feat)

## Files Created/Modified

- `crates/lattice-agent/src/agent_loop.rs` - v1.1 AgentLoop implementation with injectable LLM/tool seams, sequential tool loop, timeout/cancel, usage, and events
- `crates/lattice-agent/Cargo.toml` - Added lattice-llm and lattice-session workspace dependencies
- `crates/lattice-agent/src/lib.rs` - Added agent_loop module, re-exports, and TODO Phase 11 marker

## Decisions Made

- `LlmClient` trait uses `lattice_llm::ChatRequest`/`ChatResponse` at the model boundary
- `ToolCallExecutor` trait accepts `lattice_core::types::ToolCall` to bridge with existing `DefaultToolExecutor` (necessary boundary bridge, not old main-path leakage)
- Cancel uses `Arc<AtomicBool>` instead of `tokio_util::sync::CancellationToken` (tokio-util not in workspace)
- Timeout wraps the entire turn in `tokio::time::timeout`; on expiry returns Failed with empty messages

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Replaced tokio-util CancellationToken with Arc<AtomicBool>**
- **Found during:** Task 1
- **Issue:** `tokio-util` is not a workspace dependency; `CancellationToken` unavailable
- **Fix:** Used `Arc<AtomicBool>` as cancel flag, checked between LLM rounds
- **Files modified:** crates/lattice-agent/src/agent_loop.rs
- **Verification:** Cancel test passes with AtomicBool-based flag
- **Committed in:** 689495a (Task 1 commit)

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Minor API shape change (AtomicBool vs CancellationToken), same behavioral contract. No scope creep.

## Issues Encountered

- `lattice_core::types::ToolCall` references in `ToolCallExecutor` trait are necessary for bridging to existing `DefaultToolExecutor`. These are full-path references, not `use` imports. The source guard `rg "lattice_core"` catches them, but this is a correct boundary bridge pattern.

## User Setup Required

None - no external service configuration required.

## Known Stubs

None - all functionality is fully wired.

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| T-09-02 mitigated | agent_loop.rs | resolve_enabled_tools called before LLM call; unknown names fail fast |
| T-09-03 mitigated | agent_loop.rs | Result carries status, terminal reason, messages, usage, tool summaries |
| T-09-04 mitigated | agent_loop.rs | External timeout/cancel; no heuristic round limits |

## Next Phase Readiness

- `AgentLoop` is ready for `lattice-api` to consume in Plan 09-04
- `LlmClient` and `ToolCallExecutor` traits enable production wiring via real `lattice-llm::chat`/`stream` and `DefaultToolExecutor`
- `AgentTurnEvent` vocabulary is ready for API stream event mapping
- No blockers for downstream API facade integration

## Self-Check: PASSED

- All 3 created/modified files verified present
- All 2 task commits verified in git log
- 12 tests pass (4 contract + 8 behavioral)

---
*Phase: 09-agent-loop-tools-and-api-facade*
*Completed: 2026-05-16*
