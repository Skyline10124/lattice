---
phase: 15-agentloop-live-sink-and-cooperative-cancellation
kind: addendum
created: 2026-05-22
relates_to:
  - 15-CONTEXT.md
  - 15-RESEARCH.md
  - 15-PATTERNS.md
  - 15-VALIDATION.md
  - 15-DISCUSSION-LOG.md
  - crates/lattice-agent/src/agent_loop/types.rs
---

# Phase 15 Addendum — Doc/Code Drift Notes

This file records documented intent that did not match the final
implementation, so future phases (especially Phase 16 event mapping and
Phase 17 reconciliation) read the correct contract.

## IN-01 — `AgentTurnEvent` derives `PartialEq` only, not `PartialEq, Eq`

**Where the drift lives:** `15-CONTEXT.md` §16.6, `15-RESEARCH.md` §1.3
and §2.2, `15-PATTERNS.md` (modify-row + Sequence #2),
`15-DISCUSSION-LOG.md` line 110, and `15-VALIDATION.md` row 15-01-02
all describe the derive as `PartialEq, Eq`. They were drafted under the
assumption that `lattice_session::TurnUsage` already derived `Eq`.

**What actually shipped:** `crates/lattice-agent/src/agent_loop/types.rs`
derives **only** `PartialEq` on `AgentTurnEvent`. `TurnUsage` in
`lattice-session` is `PartialEq`-only, so adding `Eq` here would either
require widening the session crate (out of Phase 15 scope) or boxing
`Option<TurnUsage>` behind a wrapper.

**Why this is fine:** Every test added in Plan 15-03 uses `assert_eq!`
on `Vec<AgentTurnEvent>`, which only needs `PartialEq`. `Eq` would be
a stronger guarantee (no `f64`-style NaN-like inequality), but no event
payload contains a non-`Eq` type — `String`, `bool`, `usize`, and the
internal fields of `TurnUsage` are all reflexive — so this is a
documentation/scope drift, not a semantic one.

**Action for Phase 16+:** Treat `PartialEq` as the load-bearing derive.
If a future test or mapping needs `Eq`, the right scope is the
`lattice-session` crate, not `agent_loop/types.rs`.

## WR-01 — `ModelFinished` doc comment understated the cancel envelope

**Where the drift lived:** Original doc comment on `AgentTurnEvent::ModelFinished`
(types.rs:115-119 pre-fix) said: *"Error and cancel paths do not emit
ModelFinished; their outcome is observable on AgentTurnResult.status."*

**What actually ships:** Plan 15-02 cancel checkpoint #3 fires **after**
`ModelFinished` has already been emitted (mod.rs:484, immediately
before checkpoint #3 at mod.rs:515) and checkpoint #4 fires
pre-each-tool — both observe cancel after the model has cleanly
completed. A `Cancelled` turn whose cancel was raised between Done and
the first tool therefore carries a trailing `ModelFinished` in
`AgentTurnResult.events`.

**Fix:** The doc comment was rewritten in the same commit as this file
to reflect the real envelope (`ModelFinished` may appear on cancel
paths that observed cancellation post-Done; Phase 16 mapping must not
infer Completed status from "saw ModelFinished").

**Action for Phase 16:** When mapping `AgentTurnEvent` →
`TurnEvent` for the public stream, **derive terminal status from
`AgentTurnResult.status`**, not from the presence/absence of
`ModelFinished` in the event vector. This is consistent with the
[Phase 12-01] decision that `AgentTurnEvent` is process-only and
public terminal events come from `AgentTurnResult.status` mapping.
