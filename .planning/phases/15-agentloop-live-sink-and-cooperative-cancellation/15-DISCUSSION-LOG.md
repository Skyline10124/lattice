# Phase 15: AgentLoop Live Sink and Cooperative Cancellation - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-21
**Phase:** 15-AgentLoop Live Sink and Cooperative Cancellation
**Areas discussed:** Mid-stream cancel partial preservation, Cancellation checkpoint placement, run_turn_live API shape, Test scope

---

## Mid-stream Cancel Partial Preservation

| Option | Description | Selected |
|--------|-------------|----------|
| A. Preserve partial via new `StreamOutcome::Cancelled` | Add fourth `StreamOutcome` variant; `consume_model_stream` returns Cancelled with accumulated content; upstream flushes partial assistant message via shared helper; status = Cancelled, no ModelFinished. | ✓ |
| B. Discard partial on cancel | Clear partial accumulators, commit empty content. | |
| C. Reuse `StreamOutcome::Error("cancelled")` | Funnel cancel through the existing Error string variant. | |

**User's choice:** A — explicit `StreamOutcome::Cancelled` variant.
**Notes:** "错误 turn 也是真历史" precedent from Phase 14 applies — cancelled turns also need partial content preserved. User pushed for the explicit variant (vs. abusing `Error(String)`) to keep cancellation distinct at the data layer; status + terminal_reason mapping happens upstream. User asked to extract a `flush_partial_assistant_message` helper to consolidate the now-three branches (Error, Exhausted, Cancelled).

Final mid-stream cancel semantics:
```text
preserve assistant_content / reasoning_content
no ModelFinished
status = Cancelled
terminal_reason = "cancelled by caller"
```

---

## Cancellation Checkpoint Placement

| Option | Description | Selected |
|--------|-------------|----------|
| A. 5 checkpoints, with after-event cancel check inside stream consumer | Round top (existing) + after each non-Done LlmEvent + post-Done pre-tools + before each tool + after each tool. Done is *not* a cancel point — finalize model response first. | initial |
| A'. 6 checkpoints (added pre-stream check after ModelStarted emit) | Same as A plus an explicit cancel check after `emit_agent_event(ModelStarted)` and before `stream_chat`, because the awaited emit can yield. | ✓ (revision) |
| B. 6 checkpoints (also cancel on Done) | Add cancel check directly at Done before any post-processing. | |
| C. 3 checkpoints (round top, before tools, after tools only) | Skip in-stream and per-tool checks. | |

**User's choice:** A', revised after review feedback.
**Notes:**
- Initial selection was A (5 checkpoints); revision adds checkpoint #1b after `ModelStarted` emit and before `stream_chat` because `emit_agent_event(...).await` is an async yield point. Without #1b, a cancel set between checkpoint #1 and `stream_chat` would still open the LLM stream.
- Done semantics preserved exactly: `consume_model_stream` returns `StreamOutcome::Done`, upstream finalizes (validation, ModelFinished, assistant tool-call message), *then* checkpoint #3 fires before the tool execution loop.
- In-stream cancel check happens **after** processing each non-`Done` event, not before — most-recent token/reasoning is buffered and forwarded before exit.
- Tool checkpoints (#4 before-start, #5 after-finish) align with design §14.3: in-flight tool waits for real result; subsequent tools and rounds are skipped.

Final placement (6 checkpoints):
```text
1.  round top                                              (existing, unchanged)
1b. after emit(ModelStarted), before stream_chat           (new; covers awaited-emit yield)
2.  after each non-Done LlmEvent in consume_model_stream   (new; in-stream)
3.  after Done finalize, before tool execution loop        (new)
4.  before each tool starts (before ToolCallStarted)       (new)
5.  after each tool finishes (real result recorded)        (new)
```

---

## `run_turn_live` API Shape

| Option | Description | Selected |
|--------|-------------|----------|
| A. Dual public methods, `run_turn_inner` takes `Option<&mpsc::Sender>`, no timeout on `run_turn_live` | `run_turn(req)` unchanged; `run_turn_live(req, sender)` added. Both go through `run_turn_inner` with optional live sender. Initially: `run_turn_live` ignores `request.timeout`. | initial |
| A'. Dual public methods + shared timeout wrapper | Same as A, but extract `run_turn`'s `tokio::time::timeout` block into a small private helper that both entrypoints use. `run_turn_live` honors `request.timeout` with the same buffered timeout shape. | ✓ (revision) |
| B. Single method with optional sender on AgentTurnRequest | Add `events: Option<mpsc::Sender<AgentTurnEvent>>` field to `AgentTurnRequest`. | |
| C. Builder pattern on AgentLoop | `agent.turn(req).with_events(sender).run().await` | |

**User's choice:** A', revised after review feedback.
**Notes:**
- Reviewer flagged that silently ignoring `AgentTurnRequest.timeout` on `run_turn_live` violates the principle of least surprise: callers passing `timeout = Some(...)` would expect the live path to honor it.
- Resolution: extract the existing timeout block at `agent_loop/mod.rs:95-115` into a private helper (e.g. `apply_optional_timeout(request, fut)`) and reuse it for both entrypoints.
- Live-path timeout partial-content preservation (flushing accumulated content into transcript on timeout) remains deferred to Phase 17 (STATE.md todo). Phase 15's shared wrapper preserves the v1.1 buffered behavior — empty `messages`/`assistant_output` on timeout — for both methods.
- `AgentTurnRequest` stays a pure input description (session/transcript/user_input/model/provider/enabled_tools/project_root/cancel_flag/timeout) — no runtime side-channel mixed in.

Emit helper agreed:
```rust
async fn emit_agent_event(
    buffered: &mut Vec<AgentTurnEvent>,
    live: Option<&mpsc::Sender<AgentTurnEvent>>,
    event: AgentTurnEvent,
) {
    buffered.push(event.clone());
    if let Some(sender) = live {
        let _ = sender.send(event).await;
    }
}
```

Free function (not a trait, not a struct). `consume_model_stream` also takes `live: Option<&mpsc::Sender<AgentTurnEvent>>` and uses the same helper.

Reviewer's note (folded into D-15b): the awaited `send` makes every `emit_agent_event` site a yield point. This is why checkpoint #1b exists.

---

## Test Scope

| Option | Description | Selected |
|--------|-------------|----------|
| A. 7 scenarios in new `live_sink_and_cancel.rs`, "drop sender" reframed as "drop receiver" | VER-11 mandatory pair (cancel-before-tool, cancel-during-tool) + live sink mirror + receiver drop + mid-stream cancel + multi-tool batch cancel + run_turn buffered regression. | initial |
| A'. Same 7 scenarios; #5 needs custom inline LlmClient; concurrent receiver-drain mandated; PartialEq derive on AgentTurnEvent | Reviewer flagged that `FakeStreamingLlmClient` emits queued events synchronously and can't observe cancel toggled mid-stream — scenario #5 needs a custom inline `LlmClient` (CancelAwareLlm-style). Bounded channel deadlocks if the receiver isn't drained concurrently with `run_turn_live`. Scenario #3 needs `PartialEq` on `AgentTurnEvent` for `assert_eq!`. | ✓ (revision) |
| B. Minimum VER-11 only (2 tests) | Just cancel-before-tool and cancel-during-tool. | |
| C. Combine drop + mirror into one test | Reduce to 6 scenarios. | |

**User's choice:** A', revised after review feedback.
**Notes:**
- Reviewer flagged that `FakeStreamingLlmClient` yields queued events synchronously from a `Vec`. The driver can't toggle `cancel_flag` between events, so scenario #5 (cancel mid-stream) needs a custom inline `LlmClient` — pattern from `tests/agent_loop.rs::CancelAwareLlm` (yields events through a channel/generator and checks the shared cancel flag between yields).
- Reviewer flagged a deadlock risk: `emit_agent_event`'s awaited `send` blocks when the bounded channel fills. Tests that call `run_turn_live(...).await` and then drain the receiver sequentially will hang. **Mandatory pattern:** drain concurrently via `tokio::join!(run_turn_live(...), drain_receiver(...))` or a spawned drain task awaited at the end. Documented as D-19's "MUST drain concurrently" rule.
- Scenario #3 (live mirror equality) requires `assert_eq!(received_events, result.events)`; that needs `PartialEq` on `AgentTurnEvent`. D-15b adds the derive — variant payloads are all `String`/`Option<String>`/`bool`/`usize`/`Option<TurnUsage>` and `TurnUsage` already has `PartialEq, Eq` in `lattice-session`.
- "Drop sender" reframed to "drop receiver" — sender drop is an internal lifecycle issue, not externally controllable. The thing that causes `send` to fail in practice is receiver drop.
- New test file `crates/lattice-agent/tests/live_sink_and_cancel.rs` (separate from `streaming_delta.rs` for clarity of phase ownership).
- Tests require `--features test-utils` (uses `FakeStreamingLlmClient` for 6 of 7 scenarios; scenario #5 uses inline custom LlmClient).
- No timing assertions; channel capacity 32 is plenty for tests.
- Optional minimal regression: `run_turn_live` honors `request.timeout` (returns Error/timeout shape), confirming D-10's shared wrapper.

Scenario list:
1. Cancel before tool execution skips tool, returns Cancelled (VER-11)
2. Cancel during tool execution records real tool result, then returns Cancelled (VER-11)
3. `run_turn_live` sender receives same event sequence as `result.events`
4. Receiver dropped before/during live send does not cancel or fail the turn
5. Cancel mid-stream preserves partial content/reasoning, no `ModelFinished`, status Cancelled — **needs custom inline LlmClient**
6. Cancel after first tool in multi-tool batch skips remaining tools
7. `run_turn` still buffers complete `result.events` (backward-compat regression)

---

## Claude's Discretion

- Naming of partial-flush helper (`flush_partial_assistant_message` vs. shorter alternative).
- Internal shape of test helpers (closure captures vs. dedicated structs vs. wrapper around `FakeStreamingLlmClient`).
- Doc comments on `run_turn_live`, `emit_agent_event`, `StreamOutcome::Cancelled`.
- Cancel check style inside `consume_model_stream` (inline `if let Some(flag)...` vs. tiny `is_cancelled` helper). Inline is fine; prefer readability.
- Whether to add a `RunPhase` diagnostic marker on cancel — discretion to skip, design doesn't ask for it.

## Deferred Ideas

- **`stream_turn` public live stream** → Phase 16 (API-04..API-10).
- **Receiver-drop test on public API stream** → Phase 17 (VER-12).
- **Public cancellation handle on `stream_turn`** → FUT-06, post-alpha.
- **Timeout partial-content preservation** → Phase 17 (STATE.md todo).
- **In-flight tool cancellation** → design §14.3 explicitly out of scope for v1.2-alpha.
- **`tokio::sync::CancellationToken` alternative to `Arc<AtomicBool>`** → revisit if Phase 16+ wants composable cancellation.
- **Backpressure metrics on live channel** → not in alpha.
