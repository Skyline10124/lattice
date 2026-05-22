# Phase 15 Research: AgentLoop Live Sink and Cooperative Cancellation

**Phase:** 15 — AgentLoop Live Sink and Cooperative Cancellation
**Researcher:** main-orchestrator (subagent timed out without writing; orchestrator filled in inline)
**Date:** 2026-05-22
**Requirements addressed:** AGENT-08, AGENT-09, VER-11

---

## 1. Summary

CONTEXT.md (D-01..D-21) already locks the API surface, checkpoint placement, helper signatures, and full seven-scenario test matrix. This research file does **not** re-derive those — it captures the *concrete* artifacts the planner needs to write `<read_first>` and `<action>` fields without ambiguity:

- The current code positions in `agent_loop/mod.rs` after Phase 14 (line numbers in CONTEXT.md drifted slightly during Phase 14 execution; corrected here).
- The exact Phase 14 partial-flush blocks that D-05 must consolidate (`Error` + `Exhausted` arms), and the new `Cancelled` arm shape.
- The minimum compile-time invariants per commit so `cargo check -p lattice-agent` stays green.
- The bounded-channel deadlock recipe required by D-19 — drain concurrent with `run_turn_live`.
- The Tokio mpsc edge-case spec: which errors `emit_agent_event` swallows vs. which propagate.

**Key risks**
1. Async `emit_agent_event` insertion turns every emission site `.await`-bearing — must land alongside `consume_model_stream` async refactor or the file fails to compile.
2. `StreamOutcome::Cancelled` variant must land in the same commit as the `match stream_result.outcome` arm in `run_turn_inner`, or the match becomes non-exhaustive.
3. Live-mirror test scenario #3 needs `PartialEq, Eq` on `AgentTurnEvent` (D-15b); without it the test does not compile.
4. Test scenarios #1, #2, #3, #5, #6, #7 use a live `mpsc::Sender` and *must* drain concurrently — sequential drain deadlocks the bounded channel.

**Suggested sequencing** (minimizes broken-state windows, see §4 for full plan):

1. **Pure refactor commit** — extract `flush_partial_assistant_message` helper + `apply_optional_timeout` helper. No semantic change. Existing tests stay green.
2. **Pure derive commit** — add `#[derive(PartialEq, Eq)]` on `AgentTurnEvent`. Mechanical; no callers affected.
3. **Atomic emit + cancel-aware stream commit** — introduce `emit_agent_event`, widen `consume_model_stream` to take `live + cancel_flag`, add `StreamOutcome::Cancelled`, update all `match` arms in `run_turn_inner`, add 5 new cancel checkpoints, add `run_turn_live` + `run_turn_inner` `live_events` parameter. Single commit because these changes are mutually dependent at the type level.
4. **Test commit** — add `tests/live_sink_and_cancel.rs` with all 7 scenarios.
5. **Tracking commit** — STATE.md updates noting Phase 15 closeout.

---

## 2. Validation Architecture (Nyquist Sampling Strategy)

The Phase 15 contract has **four orthogonal behavioral dimensions**. The seven required scenarios in CONTEXT.md D-18 sample each dimension at least twice (Nyquist: minimum two samples per behavioral frequency band so the test suite cannot alias a regression as success).

### Behavioral Frequency Bands

| Band | Dimension | What changes per sample | Min samples |
|------|-----------|------------------------|-------------|
| **B1** | Cancellation timing | When in the round lifecycle the cancel flag flips: pre-tools, mid-tool, mid-stream, multi-tool batch | 4 (one per checkpoint family) |
| **B2** | Partial-content preservation | Whether a `Cancelled` outcome retains accumulated `assistant_content` / `reasoning_content` / `tool_calls` in the transcript | 2 (mid-stream cancel + post-tool cancel) |
| **B3** | Live-sink fidelity | Whether the `mpsc::Sender` mirror is byte-equivalent to `result.events`, AND whether the channel/receiver lifecycle affects turn outcome | 3 (full mirror + receiver drop + bounded-channel safety) |
| **B4** | Backward-compat invariant | `run_turn` (no live sink) still produces the same buffered behavior as before Phase 15 | 1 (regression test) |

### Scenario-to-Band Mapping

| D-18 Scenario | B1 | B2 | B3 | B4 | Critical Assertions |
|--|--|--|--|--|--|
| #1 Cancel before tool execution | ✓ (checkpoint 3) | ✓ (assistant tool-call msg in transcript, no tool result) | — | — | no `ToolCallStarted`, status=`Cancelled`, `terminal_reason="cancelled by caller"`, assistant tool-call msg present |
| #2 Cancel during tool execution | ✓ (checkpoint 5) | ✓ (real tool result recorded) | — | — | `ToolCallStarted` + `ToolCallFinished` both present, real tool msg in transcript, no second `ModelStarted`, status=`Cancelled` |
| #3 Live sender mirrors result.events | — | — | ✓ (full mirror) | — | `assert_eq!(received_via_mpsc, result.events)` element-wise |
| #4 Receiver drop does not cancel turn | — | — | ✓ (drop tolerance) | — | status=`Completed`, `result.events` populated, transcript committed |
| #5 Cancel mid-stream preserves partial content | ✓ (checkpoint 2) | ✓ (`"par"` in transcript, no `ModelFinished`) | — | — | partial `"par"`, status=`Cancelled`, no `ModelFinished` event, `terminal_reason="cancelled by caller"` |
| #6 Cancel after first tool in multi-tool batch | ✓ (checkpoint 5) | — | — | — | first tool `Started`+`Finished` present, second tool absent, status=`Cancelled` |
| #7 `run_turn` still buffers complete events | — | — | — | ✓ | `run_turn` and `run_turn_live` produce identical `result.events` for the same input |

### Coverage Verification

- **B1** sampled by #1 (checkpoint 3), #2 (checkpoint 5), #5 (checkpoint 2 inside `consume_model_stream`), #6 (checkpoint 5 between tools). Checkpoint 1 (round top) is already covered by `agent_loop_cancel_returns_cancelled_status` in `tests/agent_loop.rs:570`. Checkpoint 1b (post-`ModelStarted`, pre-`stream_chat`) is **the only band-1 sample without dedicated coverage** — Phase 15 should either accept that gap (the awaited `emit_agent_event` is short — yields once before `stream_chat` is called) or add an 8th scenario. **Recommendation:** document the gap in PLAN.md as a known sampling limit; the same code path is exercised on every successful round, so a regression there fails every other test indirectly.
- **B2** sampled by #1 (post-Done, pre-tools — full assistant tool-call msg flushed) and #5 (mid-stream — partial content + reasoning preserved). #2 and #6 add an indirect sample (real tool message preserved through cancel).
- **B3** sampled by #3 (full equality), #4 (drop tolerance), and the channel capacity selected in tests (32) plus the concurrent-drain pattern (D-19) which is the implicit safety margin.
- **B4** sampled by #7 only. This is the minimum but acceptable — `run_turn` is a thin wrapper around `run_turn_inner(.., None)`; if the wrapper breaks, all six other scenarios that share `run_turn_inner` also catch the regression.

### Validation Architecture Rationale

The "minimum two samples per band" Nyquist rule justifies seven scenarios:
- 4 (B1) + 2 (B2 dedicated) + 3 (B3) + 1 (B4) = **10 sample slots**, achieved with **7 scenarios** because #1, #2, #5, #6 each cover B1 and B2 simultaneously (timing + content preservation are coupled in any non-trivial cancel).

---

## 3. Concrete Code Excerpts

### 3.1 `consume_model_stream` (current — `crates/lattice-agent/src/agent_loop/mod.rs:518-670`)

CONTEXT.md cited 523-670; after Phase 14 commits the `impl AgentLoop` block opens at line 518, signature at line 523. Phase 15 widens this signature.

```rust
// crates/lattice-agent/src/agent_loop/mod.rs:518-527 (current)
impl AgentLoop {
    /// Consume an LLM stream, accumulating deltas and assembling tool calls.
    async fn consume_model_stream(
        &self,
        mut stream: lattice_llm::LlmStream,
        events: &mut Vec<AgentTurnEvent>,
    ) -> StreamResult {
        // ... loop over stream.next() ...
    }
}
```

**After Phase 15** (per D-02, D-14):

```rust
async fn consume_model_stream(
    &self,
    mut stream: lattice_llm::LlmStream,
    events: &mut Vec<AgentTurnEvent>,
    live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
    cancel_flag: Option<&Arc<AtomicBool>>,
) -> StreamResult { /* ... */ }
```

The `events.push(AgentTurnEvent::TokenDelta { content })` site at line 538 and `AgentTurnEvent::ReasoningDelta { content }` at line 542 become `emit_agent_event(events, live, AgentTurnEvent::TokenDelta { content }).await`. The cancel check goes **after** each non-`Done` event match arm (after the emit, after pending-call mutations).

### 3.2 Partial-flush blocks targeted by D-05 helper extraction

Two near-identical blocks in `mod.rs` (Phase 14 left them duplicated):

```rust
// mod.rs:236-254 (Error arm)
if !stream_result.assistant_content.is_empty()
    || !stream_result.reasoning_content.is_empty()
{
    let partial_msg = GraphMessage {
        role: "assistant".to_string(),
        content: stream_result.assistant_content.clone(),
        reasoning_content: if stream_result.reasoning_content.is_empty() {
            None
        } else {
            Some(stream_result.reasoning_content.clone())
        },
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };
    messages.push(partial_msg.clone());
    all_messages.push(partial_msg);
}
```

```rust
// mod.rs:266-284 (Exhausted arm)
// IDENTICAL to the Error arm block above.
```

**D-05 helper** (one of two acceptable shapes):

```rust
fn flush_partial_assistant_message(
    messages: &mut Vec<GraphMessage>,
    all_messages: &mut Vec<GraphMessage>,
    assistant_content: &str,
    reasoning_content: &str,
) {
    if assistant_content.is_empty() && reasoning_content.is_empty() {
        return;
    }
    let partial_msg = GraphMessage {
        role: "assistant".to_string(),
        content: assistant_content.to_string(),
        reasoning_content: if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content.to_string())
        },
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };
    messages.push(partial_msg.clone());
    all_messages.push(partial_msg);
}
```

Callers reduce to one line each, plus the new `Cancelled` arm.

### 3.3 `CancelAwareLlm` pattern (`tests/agent_loop.rs:570-621`)

This is the pattern Phase 15 scenario #5 imitates with a stream-aware variant:

```rust
// tests/agent_loop.rs:570-606 (existing — buffered chat path)
#[tokio::test]
async fn agent_loop_cancel_returns_cancelled_status() {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel_flag.clone();

    struct CancelAwareLlm {
        cancel: Arc<AtomicBool>,
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmClient for CancelAwareLlm {
        async fn chat(&self, _request: lattice_llm::ChatRequest)
            -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count > 0 { self.cancel.store(true, Ordering::Relaxed); }
            Ok(/* tool-call response */)
        }
    }
    // ...
}
```

For Phase 15 scenario #5, the **streaming** variant needs to set `cancel_flag` *between* `LlmEvent::Token` items. `FakeStreamingLlmClient::stream_chat` returns `Box::pin(futures::stream::iter(events))` (`testing.rs:94`) which is synchronous — the test cannot interleave a cancel-flag flip. Use a custom inline `LlmClient` whose stream is a `tokio::sync::mpsc::Receiver`-backed async stream and toggles the cancel flag from the test driver between `send`s. Skeleton:

```rust
struct CancelBetweenTokensLlm {
    cancel: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl LlmClient for CancelBetweenTokensLlm {
    async fn chat(&self, _r: ChatRequest) -> Result<ChatResponse, _> {
        unreachable!("stream-only fake")
    }

    async fn stream_chat(&self, _r: ChatRequest)
        -> Result<LlmStream, Box<dyn std::error::Error + Send + Sync>>
    {
        let (tx, rx) = tokio::sync::mpsc::channel::<LlmEvent>(8);
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            tx.send(LlmEvent::Token { content: "p".into() }).await.ok();
            tx.send(LlmEvent::Token { content: "ar".into() }).await.ok();
            cancel.store(true, Ordering::Relaxed);
            // Subsequent send may or may not arrive before consumer notices cancel.
            let _ = tx.send(LlmEvent::Token { content: "tial".into() }).await;
            let _ = tx.send(LlmEvent::Done {
                finish_reason: "stop".into(),
                usage: None,
            }).await;
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}
```

`tokio_stream` adds a dev-dependency. The current Cargo.toml does not declare it; check the workspace `Cargo.toml` first. **Alternative without new deps**: implement `Stream` directly via `futures::stream::unfold` + a shared `Vec` + `Notify`. The planner picks the cleaner route.

### 3.4 `FakeStreamingLlmClient` (`crates/lattice-agent/src/testing.rs:23-98`)

Builder API:
```rust
let llm = FakeStreamingLlmClient::builder()
    .push_events(vec![
        LlmEvent::Token { content: "hi".into() },
        LlmEvent::Done { finish_reason: "stop".into(), usage: None },
    ])
    .build();
```

`push_events` queues a successful round; `push_error` queues a startup-error round (pre-stream `Result::Err`). Mid-stream errors are inline `LlmEvent::Error` items inside `push_events`. Behind the `test-utils` feature flag (`Cargo.toml:19`).

### 3.5 `FakeToolExecutor` (`crates/lattice-agent/tests/streaming_delta.rs:40-69`)

```rust
struct FakeToolExecutor {
    results: Mutex<VecDeque<Result<String, String>>>,
    execution_order: Arc<Mutex<Vec<String>>>,
}

impl FakeToolExecutor {
    fn new(results: Vec<Result<String, String>>) -> Self { /* ... */ }
    fn execution_order(&self) -> Arc<Mutex<Vec<String>>> { self.execution_order.clone() }
}

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, String> {
        self.execution_order.lock().unwrap().push(call.id.clone());
        let mut results = self.results.lock().unwrap();
        match results.pop_front() {
            Some(Ok(output)) => Ok(output),
            Some(Err(msg)) => Err(msg),
            None => Err("no more fake results".into()),
        }
    }
}
```

Phase 15 scenarios #2 and #6 wrap a `FakeToolExecutor` (or implement `ToolCallExecutor` directly) so the executor flips a shared `Arc<AtomicBool>` *during* `execute` and then returns the queued result. Pattern:

```rust
struct CancelOnFirstTool {
    inner: FakeToolExecutor,
    cancel: Arc<AtomicBool>,
    fired: AtomicBool,
}

#[async_trait::async_trait]
impl ToolCallExecutor for CancelOnFirstTool {
    async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, String> {
        if !self.fired.swap(true, Ordering::Relaxed) {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.inner.execute(call).await
    }
}
```

### 3.6 `tokio::sync::mpsc::Sender::send` semantics (Tokio docs)

- **Bounded channel** (`channel::<T>(capacity)`): `send(value).await` returns `Result<(), SendError<T>>`. `SendError` is *only* returned when the receiver has been dropped. The await suspends if the channel is full and resumes when capacity frees.
- **Receiver drop**: all subsequent `send`s return `Err(SendError(value))` immediately. The unsent value is returned in the error so the caller can recover it. Phase 15 swallows the value (`let _ = sender.send(event).await;` per D-12).
- **Capacity = 0** is invalid (panics at construction). Phase 15 callers pick capacity (Phase 16 will use 256; tests use 32).
- **No `try_send`** is used in Phase 15 — backpressure waits, never drops (D-12 backpressure rule).

### 3.7 Existing `tokio::time::timeout` wrapper (`mod.rs:94-114`) — D-10 extraction target

```rust
// mod.rs:94-114 (current)
if let Some(duration) = request.timeout {
    match tokio::time::timeout(duration, self.run_turn_inner(&request, &enabled)).await {
        Ok(result) => result,
        Err(_elapsed) => AgentTurnResult {
            messages: Vec::new(),
            assistant_output: String::new(),
            usage: TurnUsage { /* zeros */ },
            tool_results: Vec::new(),
            status: AgentTurnStatus::Error,
            terminal_reason: Some("timeout".to_string()),
            events: Vec::new(),
        },
    }
} else {
    self.run_turn_inner(&request, &enabled).await
}
```

**D-10 helper extraction** (signature):

```rust
async fn apply_optional_timeout<F>(
    request: &AgentTurnRequest,
    fut: F,
) -> AgentTurnResult
where
    F: std::future::Future<Output = AgentTurnResult>,
{
    match request.timeout {
        Some(duration) => match tokio::time::timeout(duration, fut).await {
            Ok(result) => result,
            Err(_elapsed) => AgentTurnResult { /* timeout shape */ },
        },
        None => fut.await,
    }
}
```

Both `run_turn` and `run_turn_live` call `apply_optional_timeout(&request, run_turn_inner(&request, &enabled, live_events)).await`.

---

## 4. Implementation Sequencing

The single hardest constraint is that adding `emit_agent_event` (async, awaited) and `StreamOutcome::Cancelled` are **mutually dependent** at the type level — they cannot land in separate commits without breaking the build. The plan must commit them atomically. Other refactors are pure and can land first.

### Recommended commit sequence

| # | Commit | Files | Compile invariant after | Risk |
|---|--------|-------|------------------------|------|
| 1 | **Pure refactor** — extract `flush_partial_assistant_message` + `apply_optional_timeout`. Replace duplicated `Error`/`Exhausted` arm bodies with helper calls. Replace `run_turn`'s timeout `match` with helper call. | `agent_loop/mod.rs` | All existing tests pass (no semantic change). | Low — pure extraction. |
| 2 | **Derive expansion** — add `#[derive(PartialEq, Eq)]` to `AgentTurnEvent` in `types.rs`. | `agent_loop/types.rs` | All existing tests pass (mechanical derive). | Low — `TurnUsage` already derives them. |
| 3 | **Atomic streaming + cancel + live-sink** — | `agent_loop/mod.rs` | `cargo check -p lattice-agent` passes; tests not yet exercising new code paths still pass. New cancel checkpoints exist but are unreachable via existing tests. | **Medium** — touches the largest function in the crate; landmines below. |
| | a. Add `emit_agent_event` free function. | | | |
| | b. Widen `consume_model_stream` signature: `live: Option<&Sender>, cancel_flag: Option<&Arc<AtomicBool>>`. Convert all `events.push(...)` to `emit_agent_event(events, live, ...).await`. Add post-event cancel check. | | | |
| | c. Add `StreamOutcome::Cancelled` variant. | | | |
| | d. Add `Cancelled` arm to `match stream_result.outcome` in `run_turn_inner` — flush partial via helper, return `AgentTurnResult { status: Cancelled, terminal_reason: Some("cancelled by caller"), .. }`. | | | |
| | e. Replace remaining 4 `all_events.push(...)` sites in `run_turn_inner` with `emit_agent_event(...).await`. Threaded `live_events: Option<&Sender>` parameter through. | | | |
| | f. Add 5 new cancel checkpoints (1b post-`ModelStarted`, 2 inside `consume_model_stream`, 3 post-`Done` pre-tools, 4 pre-`ToolCallStarted`, 5 post tool finish). | | | |
| | g. Add `run_turn_live(&self, request, sender) -> AgentTurnResult` public method calling `apply_optional_timeout(&request, run_turn_inner(&request, &enabled, Some(&sender)))`. | | | |
| 4 | **Test suite** — `tests/live_sink_and_cancel.rs` with all 7 scenarios. Gated `#![cfg(feature = "test-utils")]`. | `tests/live_sink_and_cancel.rs` | `cargo test -p lattice-agent --features test-utils` passes. | Medium — concurrency assertions. See §5. |
| 5 | **STATE.md + ROADMAP.md** — update Phase 15 to "Complete", increment counters. | `.planning/*.md` | n/a | Trivial. |

### Sequencing risks the planner must encode in tasks

- **Commit 3 is one big commit by necessity.** The planner should split it into multiple *tasks within one wave* (each producing a testable intermediate diff) but they all land in one commit because intermediate states do not compile. Alternatively, the planner can use feature-flagged scaffolding to land them as separate commits — but that adds complexity for no benefit. **Recommendation: single commit, multiple tasks under it with a single `commit:` step at the wave end.**
- **Commit 1 must precede commit 3** so the new `Cancelled` arm can call the same helper as `Error`/`Exhausted`. Otherwise commit 3 introduces a third copy of the duplicated block.
- **Commit 2 must precede commit 4.** `assert_eq!` on `Vec<AgentTurnEvent>` requires `PartialEq`.

---

## 5. Test Concurrency Recipe (Bounded-Channel Drain)

D-19 is explicit: scenarios #1, #2, #3, #5, #6, #7 use a live `mpsc::Sender`; the test driver **must drain concurrently with `run_turn_live`** or the bounded channel fills, every awaited `emit_agent_event::send` blocks, and `run_turn_live` deadlocks waiting for capacity.

### Canonical recipe

```rust
async fn drive_run_turn_live(
    loop_inst: &AgentLoop,
    request: AgentTurnRequest,
    capacity: usize,
) -> (AgentTurnResult, Vec<AgentTurnEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentTurnEvent>(capacity);

    let drain = async {
        let mut received = Vec::new();
        while let Some(event) = rx.recv().await {
            received.push(event);
        }
        received
    };

    let run = async move {
        let result = loop_inst.run_turn_live(request, tx).await;
        // tx is moved into run_turn_live; on completion it is dropped, closing rx.
        result
    };

    tokio::join!(run, drain)
}
```

**Why this works:** `run_turn_live` owns `tx`; it drops `tx` on return; the drop closes `rx`; `rx.recv()` returns `None`; the drain future completes. `tokio::join!` runs both concurrently on the same task.

**For tests with `tokio_stream::ReceiverStream`-backed fake LLMs (scenario #5):** still use the recipe above for the live sink, plus a separate `tokio::spawn` for the LLM event source as shown in §3.3.

**Channel capacity:** D-20 says 32 is plenty for non-overflow scenarios. For scenario #3 (full mirror, multiple events per round, multiple rounds), 32 is sufficient if the drain runs concurrently. **Do not** set capacity to a value smaller than the test's expected event count without concurrent drain — the test will hang.

**Receiver drop test (#4):** drop `rx` *before* awaiting `run_turn_live`. `run_turn_live` should still return `Completed`; every `emit_agent_event` `send` returns `Err(SendError)` and is silently swallowed.

---

## 6. Tokio mpsc Edge Cases

Phase 15 must distinguish **silent-swallow** errors (live-sink degraded, turn unaffected) from **propagate** errors (turn failure). The contract is in D-12, expanded here:

| Condition | Phase 15 behavior | Why |
|-----------|-------------------|-----|
| Receiver dropped (any send returns `Err(SendError)`) | Silently swallow (`let _ = sender.send(...).await;`). Turn continues, buffered `events` still populated. | Live sink is best-effort. Receiver may have drained early (UI closed, caller gave up). |
| Channel full, receiver still alive | `send` awaits — backpressure blocks the live caller until capacity. **Do not** `try_send`. | D-12 backpressure rule. Alpha policy. |
| Receiver capacity 0 | Compile-time panic — never construct. | `tokio::sync::mpsc::channel(0)` panics. |
| `cancel_flag` set during `emit_agent_event::send` await | The await is short; cancel-flag check happens after the await returns. The just-emitted event is preserved in both buffer and live receiver before cancel exit. | D-13 emission ordering; the partial that *caused* the cancel decision is always observed. |
| Multiple concurrent `Sender` clones | Phase 15 uses a single `Sender` per `run_turn_live` invocation. No clones. Phase 16 may clone for fan-out. | Out of scope. |
| Sender dropped explicitly | Drop on `run_turn_live` return is the normal close signal. Tests rely on this to terminate `rx.recv()` loops. | Clean termination. |
| Mid-stream cancel + live-sink active simultaneously | `consume_model_stream` checks cancel **after** processing each non-`Done` event. The most recent emission is delivered before cancel exit. | D-02 ordering. |

**Edge-case test coverage**:
- Scenario #4 covers receiver drop tolerance.
- Scenarios #1, #2, #3, #5, #6, #7 implicitly cover backpressure-await safety (channel capacity 32 + concurrent drain).
- No test covers explicit sender clone — out of scope.

---

## 7. Compilation Guardrails (Per-Commit `cargo check` Invariants)

The minimum changes that must land **together** in commit 3 to keep `cargo check -p lattice-agent` green:

```rust
// All of the following must be in the same commit, or the build breaks:

// (a) StreamOutcome variant
enum StreamOutcome {
    Done,
    Error(String),
    Cancelled,         // NEW — D-01
    Exhausted,
}

// (b) The match in run_turn_inner that dispatches on StreamOutcome
match stream_result.outcome {
    StreamOutcome::Error(msg) => { /* existing */ }
    StreamOutcome::Exhausted => { /* existing */ }
    StreamOutcome::Cancelled => { /* NEW arm — flush partial, return Cancelled */ }
    StreamOutcome::Done => { /* existing */ }
}

// (c) consume_model_stream signature widening
async fn consume_model_stream(
    &self,
    mut stream: lattice_llm::LlmStream,
    events: &mut Vec<AgentTurnEvent>,
    live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
    cancel_flag: Option<&Arc<AtomicBool>>,
) -> StreamResult { /* ... */ }

// (d) The ONE caller of consume_model_stream
let stream_result = self
    .consume_model_stream(stream, &mut all_events, live_events, request.cancel_flag.as_ref())
    .await;

// (e) run_turn_inner signature
async fn run_turn_inner(
    &self,
    request: &AgentTurnRequest,
    enabled: &EnabledTools,
    live_events: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
) -> AgentTurnResult { /* ... */ }

// (f) run_turn caller
self.run_turn_inner(&request, &enabled, None).await

// (g) New run_turn_live entrypoint
pub async fn run_turn_live(
    &self,
    request: AgentTurnRequest,
    events: tokio::sync::mpsc::Sender<AgentTurnEvent>,
) -> AgentTurnResult {
    // resolve_enabled_tools (same shape as run_turn)
    // apply_optional_timeout(&request, self.run_turn_inner(&request, &enabled, Some(&events)))
    // .await
}
```

**Anything that introduces async `.await` in `consume_model_stream` (the `emit_agent_event` calls) must land in the same commit** — half-way state with sync `events.push` for some events and async `emit_agent_event` for others is incoherent.

**Re-export check** (D-15): `crates/lattice-agent/src/lib.rs` already re-exports `pub use agent_loop::{AgentLoop, AgentTurnEvent, ...}`. Phase 15's new `run_turn_live` is a method on `AgentLoop` so no new re-export is needed. Verify no `pub use` line needs updating.

---

## 8. Open Questions / Risks

CONTEXT.md is locked; few open items remain.

1. **Checkpoint 1b coverage gap** (B1 sampling). Phase 15 has 7 scenarios; checkpoint 1b (post-`ModelStarted` emit, pre-`stream_chat`) has no dedicated test. Acceptable: the same code path runs on every successful round and a regression there fails every other test indirectly. PLAN.md should note this in `must_haves` as a known-and-accepted sampling gap.
2. **`tokio_stream` dev-dependency for scenario #5**. If the planner picks the `ReceiverStream` route, add `tokio-stream` to `[dev-dependencies]`. Alternative: implement `Stream` via `futures::stream::unfold` (already in deps via `futures`). **Recommendation**: prefer the no-new-dep path (`futures::stream::unfold` or a small `Stream` impl) to keep the dependency surface minimal in alpha.
3. **Channel capacity for tests**: D-20 says 32. Larger (e.g., 256) is also fine and never hurts; smaller (e.g., 4) is risky if drain is slightly delayed. Pick 32.
4. **Diagnostics on cancel** (D-CL CLAUDE'S DISCRETION): no `RunPhase` marker requested; do not add one.

---

## RESEARCH COMPLETE
