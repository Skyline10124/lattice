# Phase 17: Failure Matrix and Run/Stream Consistency - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-22
**Phase:** 17-failure-matrix-and-run-stream-consistency
**Areas discussed:** Commit failure injection, API-level cancellation, Run/stream consistency, Timeout reconciliation

---

## Commit Failure Injection

| Option | Description | Selected |
|--------|-------------|----------|
| FailingSessionApi wrapper | Inject failure via trait mock | |
| Mock SQLite | Intercept at storage layer | |
| Real SQLite parent conflict | Use `commit_turn_inner` parent validation with racing commit | ✓ |

**User's choice:** Real SQLite parent conflict with deterministic blocking pattern (BlockingLlmClient pauses mid-stream, test thread inserts racing commit, then releases).
**Notes:** User identified race condition in Phase 16's existing commit failure test — fast fake LLM may complete before racing commit is inserted. Phase 17 must use blocking synchronization to make conflict deterministic. No wrappers, no mock SQLite — proves VER-12 with real persistence semantics.

---

## API-Level Cancellation

| Option | Description | Selected |
|--------|-------------|----------|
| Inject hidden cancel_flag into stream_turn | Internal test entry point for cancelled live path | |
| New internal test-only API | Expose cancel_flag threading for tests | |
| Layer separation — no public live cancel test | Cover Cancelled at agent + API buffered seam only | ✓ |

**User's choice:** Do not pollute stream_turn with test-only cancel infrastructure. Cancelled is tested at agent layer (Phase 15 existing) and API buffered seam (TurnDeps fake returning Cancelled status). Public live Cancelled is unreachable without FUT-06.
**Notes:** User emphasized maintaining Phase 16/17 boundary integrity. Explicit documentation in CONTEXT.md that Cancelled public-live-path is out of alpha scope.

---

## Run/Stream Consistency

| Option | Description | Selected |
|--------|-------------|----------|
| Snapshot testing | Serialize and compare outputs | |
| Structural normalized comparison | Strip unstable fields, assert_eq on semantic struct | ✓ |

**User's choice:** NormalizedTurnObservation struct with structured assert_eq. Two independent tempdirs, same FakeLlm/FakeToolExecutor script. Read session store after stream_turn commit for durable comparison.
**Notes:** User rejected snapshot testing due to masking overhead (random IDs, timestamps). Minimum three scenarios: completed content-only, error after partial token, tool call path. Cancelled excluded from live path per D-04/D-06.

---

## Timeout Reconciliation

| Option | Description | Selected |
|--------|-------------|----------|
| Defer to later phase | Document as known gap | |
| Fix in Phase 17 with cooperative deadline | Replace outer timeout wrapper with deadline in stream loop | ✓ |

**User's choice:** Fix now. Replace outer `tokio::time::timeout` with cooperative deadline inside stream consumption loop. Timeout fires at `stream.next()` granularity, walks partial-flush path, returns Error with all emitted events.
**Notes:** User argued this defect spans observability and durability — leaving it open would prevent Phase 17 from claiming full run/stream consistency. Scope limited to agent-internal path; no public API timeout field, no in-flight tool kill.

---

## Claude's Discretion

- File organization for test helpers (inline vs shared module)
- BlockingLlmClient naming and synchronization primitive choice
- NormalizedTurnObservation placement (test-only struct vs `#[cfg(test)]` utility)
- Whether to introduce `test-utils` feature on `lattice-api`
- Internal naming of cooperative deadline mechanism
- `RunTurnStage` field reconciliation approach

## Deferred Ideas

- Public cancellation handle on `stream_turn` — FUT-06, post-alpha
- Public API timeout field on `RunTurnRequest`
- `run_turn` consuming `stream_turn` internally — design §17.3
- `LiveTurnDeps` for injected live streaming tests
- Buffered path branch population (`stream_turn_with_deps` keeps `branch: None`)
- Complex in-flight tool cancellation on timeout
