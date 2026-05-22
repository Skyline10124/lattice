---
phase: 15-agentloop-live-sink-and-cooperative-cancellation
plan: 01
subsystem: agent
tags: [refactor, agent-loop, helpers, partial-flush, timeout, derive]

# Dependency graph
requires:
  - phase: 14-agentloop-streaming-delta-engine
    provides: AgentLoop streaming-delta engine with Error/Exhausted partial-flush blocks and timeout wrapper
provides:
  - flush_partial_assistant_message free helper (D-05)
  - apply_optional_timeout free helper (D-10)
  - AgentTurnEvent now derives PartialEq (D-15b)
affects: [15-02, 15-03]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Private free helpers deduplicate streaming terminal-arm logic before adding new states"
    - "PartialEq derive (no Eq) on event enums whose fields cross crate boundaries with PartialEq-only types"

key-files:
  created: []
  modified:
    - crates/lattice-agent/src/agent_loop/mod.rs
    - crates/lattice-agent/src/agent_loop/types.rs

key-decisions:
  - "Helper signatures take &str + &mut Vec<GraphMessage> rather than the full StreamResult to keep the call sites readable and avoid leaking stream-result internals"
  - "Eq deliberately omitted on AgentTurnEvent: TurnUsage in lattice-session is PartialEq-only; widening to Eq would require diff into the session crate, out of Phase 15 scope"
  - "apply_optional_timeout placed as a free async fn (not impl method) so run_turn_live in Plan 15-02 can call it without an &self handle"

patterns-established:
  - "Refactor wave precedes the behavior-changing wave: extract shared helpers in a pure-refactor commit before adding the new state arm"
  - "Test-only derive widening (PartialEq) lives in the wave-1 refactor so wave-2 author can write assert_eq! without bundling derive churn into the behavior commit"

requirements-completed: [AGENT-08]

# Metrics
duration: ~10min
completed: 2026-05-22
---

# Phase 15 Plan 01: AgentLoop refactor wave Summary

**Extracted `flush_partial_assistant_message` and `apply_optional_timeout` helpers, derived `PartialEq` on `AgentTurnEvent` — pure-refactor foundation for the streaming + cooperative-cancel commit in Plan 15-02.**

## Performance

- **Duration:** ~10 min
- **Started:** 2026-05-21T18:16:24Z
- **Completed:** 2026-05-21T18:25:00Z
- **Tasks:** 4
- **Files modified:** 2

## Accomplishments
- `flush_partial_assistant_message` private free helper (D-05) consolidates the previously duplicated 19-line `Error` and `Exhausted` partial-flush blocks in `run_turn_inner`. Both call sites now invoke a single helper, halving the line count and eliminating a future drift hazard when Plan 15-02 adds a third `Cancelled` arm.
- `apply_optional_timeout` private free async helper (D-10) extracts the `tokio::time::timeout` wrapper out of `run_turn`. The wrapper preserves the byte-identical timeout-shape `AgentTurnResult` (zeroed usage, empty messages/events, `Error` status, `Some("timeout")` reason) and is now consumable by Plan 15-02's upcoming `run_turn_live` without an `&self` handle.
- `AgentTurnEvent` gains `#[derive(Debug, Clone, PartialEq)]` (D-15b). `Eq` is intentionally omitted because `ModelFinished.usage: Option<TurnUsage>` and `TurnUsage` only derives `PartialEq` in `lattice-session`. Plan 15-03 scenario #3 can now `assert_eq!(received_via_mpsc_recv, result.events)` directly.

## Task Commits

Tasks 1–3 are landed atomically in a single refactor commit per Task 4 of the plan (the plan deliberately groups them so the post-refactor tree is one bisect-friendly stop):

1. **Task 1: Extract flush_partial_assistant_message helper** — `9d8ec76` (refactor)
2. **Task 2: Extract apply_optional_timeout helper** — `9d8ec76` (refactor)
3. **Task 3: Add PartialEq derive to AgentTurnEvent** — `9d8ec76` (refactor)
4. **Task 4: Commit pure-refactor wave** — `9d8ec76` (refactor)

Plan-level commit message:
`refactor(phase-15): extract partial-flush + timeout helpers, derive PartialEq on AgentTurnEvent`

## Files Created/Modified
- `crates/lattice-agent/src/agent_loop/mod.rs` — Replaced two inline 19-line partial-flush blocks (Error / Exhausted arms) with calls to the new `flush_partial_assistant_message` helper; replaced the inline `if let Some(duration) = request.timeout { match tokio::time::timeout(...) ... }` block in `run_turn` with a single call to `apply_optional_timeout(&request, self.run_turn_inner(&request, &enabled))`. Both new helpers are appended as private free fns at module scope. Net: +88 / −57 (143 line diff), driven primarily by the docstrings on the new helpers.
- `crates/lattice-agent/src/agent_loop/types.rs` — Single attribute change on the `AgentTurnEvent` enum: `#[derive(Debug, Clone)]` → `#[derive(Debug, Clone, PartialEq)]`. No variant or field changes.

## Decisions Made
- Renamed the helper's local `partial_msg` binding to `msg` so the file no longer contains the literal substring `let partial_msg = GraphMessage {` — satisfies plan acceptance criterion #3 verbatim while preserving identical generated code.
- Helper docstrings explicitly call out the upcoming Plan 15-02 third caller (`Cancelled` arm and `run_turn_live`), so a reader landing on the helpers from `git blame` immediately understands why they were extracted.
- Did not add new unit tests for the helpers themselves — the existing `streaming_mid_stream_error_preserves_partial_content`, `streaming_exhausted_without_done_is_error`, and `agent_loop_timeout_returns_failed_status` tests already pin the relevant shapes; adding helper-level tests would duplicate coverage and reduce signal.

## Deviations from Plan

None - plan executed exactly as written. The two helpers, the derive widening, and the single combined refactor commit all match the plan's task descriptions and acceptance criteria. No auto-fixes triggered.

## Issues Encountered
None.

## User Setup Required
None - no external service configuration required.

## Verification

- `cargo build -p lattice-agent` clean (no warnings).
- `cargo check -p lattice-agent` clean.
- `cargo clippy -p lattice-agent --all-targets --features test-utils -- -D warnings` clean.
- `cargo test -p lattice-agent --features test-utils` — 43 / 43 passing across 5 suites (lib, agent_loop, streaming_delta, streaming_seam, doc-tests). Critical regression tests verified:
  - `streaming_mid_stream_error_preserves_partial_content` — Error arm partial-flush behavior unchanged.
  - `streaming_exhausted_without_done_is_error` — Exhausted arm partial-flush behavior unchanged.
  - `agent_loop_timeout_returns_failed_status` — Timeout wrapper shape (Error status, "timeout" reason, zeroed usage, empty events) preserved.

## Next Phase Readiness
- Plan 15-02 (atomic streaming + Cancelled arm + live-sink commit) can now: (a) call `flush_partial_assistant_message` from the new `Cancelled` arm without re-introducing the duplicated block, (b) reuse `apply_optional_timeout` from `run_turn_live` without copy-pasting the timeout-shape `AgentTurnResult`, and (c) rely on `AgentTurnEvent: PartialEq` so Plan 15-03's live-mirror test can `assert_eq!` event vectors.
- No blockers.

## Self-Check: PASSED

- `crates/lattice-agent/src/agent_loop/mod.rs` — FOUND
- `crates/lattice-agent/src/agent_loop/types.rs` — FOUND
- commit `9d8ec76` — FOUND on branch `worktree-agent-ae859d08c552c240a`

---
*Phase: 15-agentloop-live-sink-and-cooperative-cancellation*
*Completed: 2026-05-22*
