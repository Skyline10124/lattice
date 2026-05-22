# Phase 16: Live API Stream and Commit Transaction - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-22
**Phase:** 16-live-api-stream-and-commit-transaction
**Areas discussed:** stream_turn_with_deps disposition, cancel_flag/timeout in live path, PreparedTurn code organization, branch information

---

## stream_turn_with_deps disposition

| Option | Description | Selected |
|--------|-------------|----------|
| A) Preserve unchanged | Keep as buffered replay seam for tests, rewrite top-level stream_turn to live | ✓ |
| B) Delete | Remove stream_turn_with_deps entirely, test via live path only | |
| C) Deprecate | Keep but mark deprecated | |

**User's choice:** A — preserve unchanged. Buffered replay seam is useful for TurnDeps-based tests. Update comments to clarify the live/buffered distinction.
**Notes:** User specifically noted that TurnDeps's `execute_agent_turn` returns completed `(Vec<AgentTurnEvent>, AgentTurnResult)` and is inherently buffered — it cannot test live streaming. No reason to delete or deprecate.

---

## cancel_flag / timeout in live path

| Option | Description | Selected |
|--------|-------------|----------|
| A) Strict None | No cancel_flag, no timeout, receiver drop is fire-and-forget | ✓ |
| B) Internal cancel flag | Create internal flag, trigger cancel on receiver drop | |

**User's choice:** A — strict None. Receiver drop = stop observing, NOT cancel execution.
**Notes:** User emphasized this distinction is critical for API-10 compliance. No sneaky timeout either — Phase 15's agent-level timeout acceptance is orthogonal; Phase 16's RunTurnRequest has no timeout input.

---

## PreparedTurn and code organization

| Option | Description | Selected |
|--------|-------------|----------|
| A) Shared prepare_turn | Extract common prep from execute_turn_inner, both paths call it | |
| B) Independent + shared helpers | Live path has own prepare_live_turn, share pure commit/mapping helpers | ✓ |

**User's choice:** B — independent orchestration, shared pure helpers.
**Notes:** User's rationale: pulling live path back into TurnDeps would blur the boundary. Allowed repetition: session open/create/head_transcript orchestration. Shared helpers: commit payload construction, terminal event derivation, model/prompt resolution. Phase 17 run/stream consistency tests will lock both paths to same semantics.

---

## branch information

| Option | Description | Selected |
|--------|-------------|----------|
| A) Query session.status() now | Add status() call in prepare_live_turn for branch | ✓ |
| B) branch: None, defer to Phase 18 | Skip the extra DB query, fill branch later | |

**User's choice:** A — query now. TurnStarted.branch should be stable from Phase 16 onward.
**Notes:** User noted this is a startup operation — if status() fails, stream_turn returns Err before the stream starts. Buffered path keeps branch: None (acceptable divergence for now).

---

## Claude's Discretion

- File organization: live path in `turn/execute.rs` or new `turn/live.rs`
- Naming of internal helpers (emit_turn_event, run_live_turn_task, prepare_live_turn, build_commit_input)
- Whether derive_terminal_event is a free function or inline match
- Doc comments
- PreparedTurn visibility (pub(crate) vs private)

## Deferred Ideas

- Public cancellation handle on stream_turn (FUT-06, post-alpha)
- Timeout on live path (no RunTurnRequest.timeout field yet)
- Receiver-drop tests (Phase 17 VER-12)
- run_turn consuming stream_turn internally (future milestone)
- LiveTurnDeps for injected live streaming tests (not in alpha)
