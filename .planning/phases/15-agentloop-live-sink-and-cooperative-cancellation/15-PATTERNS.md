# Phase 15 Pattern Map

**Phase:** 15 — AgentLoop Live Sink and Cooperative Cancellation
**Created:** 2026-05-22

> Maps each new/modified file in Phase 15 to its closest existing analog in the
> codebase, with concrete code excerpts so plan tasks can cite real anchors in
> `<read_first>` and `<action>` fields. See `15-RESEARCH.md` §3 for full
> excerpts and rationale.

---

## File-to-Analog Map

| Phase 15 file | Action | Closest analog | Why this analog |
|--------------|--------|----------------|------------------|
| `crates/lattice-agent/src/agent_loop/mod.rs` | **modify** | itself (Phase 14 left it in shape we extend) | Same file; widen `consume_model_stream` + `run_turn_inner` signatures, add `run_turn_live`, `emit_agent_event`, `flush_partial_assistant_message`, `apply_optional_timeout`, `StreamOutcome::Cancelled` |
| `crates/lattice-agent/src/agent_loop/types.rs` | **modify** | itself | Add `#[derive(PartialEq, Eq)]` to `AgentTurnEvent` (D-15b). All variants already use trivially-comparable types (`String`, `bool`, `usize`, `Option<TurnUsage>`; `TurnUsage` already derives both). |
| `crates/lattice-agent/tests/live_sink_and_cancel.rs` | **create** | `crates/lattice-agent/tests/streaming_delta.rs` | Same `#![cfg(feature = "test-utils")]` gate, same `FakeStreamingLlmClient` builder usage, same `FakeToolExecutor` pattern. Phase 15 adds `tokio::sync::mpsc::channel` + `tokio::join!` drain. |

---

## Component-to-Pattern Map

### `emit_agent_event` (free fn)
- **Closest analog:** the existing `events.push(AgentTurnEvent::TokenDelta { content })` calls inside `consume_model_stream` (`mod.rs:538, 542`). New helper wraps both `events.push` and the optional `Sender::send` await behind one signature.
- **Excerpt to imitate:** see RESEARCH §3.1 — the helper body is 4 lines.
- **Risk:** every call site becomes `.await`-bearing. Must land atomically with `consume_model_stream` async refactor (RESEARCH §4 commit 3).

### `flush_partial_assistant_message` (private helper, D-05)
- **Closest analog:** the duplicated `Error`-arm block at `mod.rs:236-254` and `Exhausted`-arm block at `mod.rs:266-284` — currently identical 19-line bodies.
- **Excerpt to consolidate:** see RESEARCH §3.2.
- **Risk:** none — pure refactor. Land first (commit 1).

### `apply_optional_timeout` (private helper, D-10)
- **Closest analog:** `mod.rs:94-114` (current `tokio::time::timeout` wrapper inside `run_turn`). New helper accepts `request: &AgentTurnRequest` + `fut: impl Future<Output = AgentTurnResult>`.
- **Excerpt:** see RESEARCH §3.7.
- **Risk:** none — pure refactor. Land first (commit 1).

### `run_turn_live` (new public method)
- **Closest analog:** the existing `run_turn` (`mod.rs:73-115`). New method has identical scaffolding (resolve enabled tools, optional timeout) but threads `Some(&events)` into `run_turn_inner`.
- **Risk:** must land in same commit as `run_turn_inner`'s widened signature (RESEARCH §7 invariant table).

### `StreamOutcome::Cancelled` variant
- **Closest analog:** existing `StreamOutcome::Error(String)` variant at `mod.rs:499-506` and its arm at `mod.rs:235-264`. New variant carries no payload (the cancel reason is fixed: `"cancelled by caller"`); the partial flush logic is the *same* helper as Error/Exhausted (D-05).
- **Risk:** every match-on-`StreamOutcome` site must add the new arm. There is exactly **one** such site in the crate (`mod.rs:234`). Must land atomically (RESEARCH §7).

### Cancel checkpoint #1b (post-`ModelStarted`, pre-`stream_chat`)
- **Closest analog:** the existing round-top cancel check at `mod.rs:158-170`. New check has identical shape (`flag.load(Ordering::Relaxed)`) but lands *after* the `emit_agent_event(...).await` for `ModelStarted` and *before* `self.config.llm.stream_chat(chat_request).await`. Returns `Cancelled` with empty content (no partial — the stream never opened).

### Cancel checkpoint #2 (after each non-`Done` `LlmEvent`, inside `consume_model_stream`)
- **Closest analog:** none yet. New check; runs *after* the per-event match arm body finishes (after `events.push`/`emit_agent_event` and after pending-call mutations) and *before* the next `stream.next().await`. Returns `StreamResult { outcome: Cancelled, .. }` with accumulated `assistant_content`/`reasoning_content`/`pending_calls` preserved.

### Cancel checkpoints #3, #4, #5
- **Closest analog:** the existing round-top check at `mod.rs:158-170` for #3 and #4 (return early with cumulative result). #5 (post-tool finish) reuses the same shape.
- **Insertion sites** (per CONTEXT D-06):
  - #3: between `mod.rs:422` (assistant tool-call message pushed) and `mod.rs:425` (`for call in &validated_calls` loop).
  - #4: top of `for call in &validated_calls` body, before `mod.rs:426` (`emit_agent_event(.., ToolCallStarted)`).
  - #5: after `mod.rs:477` (tool message pushed to `all_messages`), before next iteration.

### `CancelBetweenTokensLlm` (new test-only LlmClient for scenario #5)
- **Closest analog:** `CancelAwareLlm` in `tests/agent_loop.rs:570-606` (buffered-chat cancel pattern). Phase 15 needs a *streaming* variant — `FakeStreamingLlmClient` emits synchronously from a `Vec` (`testing.rs:94`) which can't interleave a cancel-flag flip between events.
- **Excerpt:** see RESEARCH §3.3.
- **Recommendation:** stay no-new-dep — implement `Stream` via `futures::stream::unfold` rather than adding `tokio-stream` as a dev-dep.

### `CancelOnFirstTool` (new test helper for scenarios #2, #6)
- **Closest analog:** `FakeToolExecutor` (`tests/streaming_delta.rs:40-69`). Wrap an inner `FakeToolExecutor`, flip `Arc<AtomicBool>` from inside `execute`, then forward.
- **Excerpt:** see RESEARCH §3.5.

### `drive_run_turn_live` (new test helper)
- **Closest analog:** none — Phase 14 tests don't use mpsc. Phase 15 introduces the `tokio::join!(run, drain)` recipe (D-19, RESEARCH §5).
- **Critical:** sequential drain deadlocks (capacity 32, scenario #3 emits ~10-15 events per round). Use `tokio::join!` or spawn a dedicated drain task.

---

## Existing Test Infrastructure (reuse as-is)

| Asset | File:line | Use in Phase 15 |
|-------|-----------|-----------------|
| `FakeStreamingLlmClient` | `crates/lattice-agent/src/testing.rs:23-98` | Scenarios #1, #2, #3, #4, #6, #7 (#5 needs custom variant) |
| `FakeToolExecutor` | `crates/lattice-agent/tests/streaming_delta.rs:40-69` | All scenarios that execute tools (#1, #2, #6) — wrapped by `CancelOnFirstTool` for #2/#6 |
| `make_request` helper | `crates/lattice-agent/tests/streaming_delta.rs:19-36` | All 7 scenarios — duplicate inline (test files don't share helpers) |
| `AgentLoop`/`AgentLoopConfig` constructors | `crates/lattice-agent/src/agent_loop/mod.rs:60-68` | All scenarios — same `AgentLoop::new(AgentLoopConfig { llm, tool_executor })` pattern |
| `Arc<AtomicBool>` cancel pattern | `crates/lattice-agent/tests/agent_loop.rs:570-621` | Direct reuse — Phase 15 tests construct cancel flag identically |

---

## Compile-time Coupling Map

Per RESEARCH §7, the following changes must land **in the same commit** to keep `cargo check -p lattice-agent` green:

```
StreamOutcome::Cancelled variant
  ⇄ run_turn_inner's match-on-outcome (new arm)
    ⇄ consume_model_stream signature widening (live + cancel_flag params)
      ⇄ all five callers of consume_model_stream emit-event pattern
        ⇄ run_turn_inner signature (live_events param)
          ⇄ run_turn passes None
          ⇄ run_turn_live passes Some(&events)
```

**Pre-coupled refactors (safe to commit independently first):**
- `flush_partial_assistant_message` extraction (commit 1)
- `apply_optional_timeout` extraction (commit 1)
- `#[derive(PartialEq, Eq)]` on `AgentTurnEvent` (commit 2)

These three are pure refactors / mechanical derives; existing tests stay green and the planner can verify them with `cargo test -p lattice-agent --features test-utils` before continuing to commit 3.
