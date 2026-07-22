# Issue #24 Slice 5 — Provider failures, public cancellation, and deadline

## Goal

Give `session send` real public cooperative cancellation/deadline behavior and
black-box attest terminal JSONL, durable session state, audit state, and process
cleanup.

## Approved public controls

The user approved documented public controls rather than harness-only kills.

### Deadline

Add an optional strict profile field:

```toml
[llm]
timeout_secs = 1
```

```rust
pub struct LlmSection {
    pub timeout_secs: Option<u64>,
    pub retry: LlmRetrySection,
}
```

Validate nonzero and a documented upper bound in `lattice-profile`, then map it
to `AgentTurnRequest.timeout`. The default remains `None`; the CLI's existing
300-second observer idle timeout remains a separate last-resort guard.

### SIGINT cancellation

`session send` creates an `Arc<AtomicBool>`, installs a scoped Tokio SIGINT
listener, and passes the flag through `RunTurnRequest`. Abort the listener after
the stream drains so it cannot affect later commands.

The Agent loop must race a stalled `stream.next()` against both deadline and
cooperative cancellation; checking the flag only after a provider event is not
enough.

## Fake provider modes

Add bounded scripts for HTTP error, stall-until-release, delayed chunk,
disconnect after chunks, and malformed SSE. Handler threads observe shutdown so
failed tests do not leak.

## Scenarios

- unreachable/non-2xx startup with retry disabled;
- disconnect before output;
- disconnect after a partial token;
- malformed SSE;
- SIGINT while the provider is stalled;
- one-second profile deadline while stalled;
- successful mutation followed by provider failure retains Local bytes;
- no scenario automatically retries.

Expected cancellation sequence:

```text
turn_started ... turn_cancelled -> turn_committing ->
turn_committed(status=cancelled, disposition=aborted/not_applicable) ; exit 1
```

Expected deadline/failure sequence:

```text
turn_started ... turn_failed -> turn_committing ->
turn_committed(status=error) ; exit 1
```

Session log/status and audit query must agree with the terminal event. A harness
step timeout remains a test failure/cleanup mechanism and must never be reported
as product cancellation/deadline coverage.

## Files

- `crates/lattice-profile/src/{schema,load}.rs` + AGENTS/tests
- `crates/lattice-api/src/turn/mapping.rs`
- `crates/lattice-cli/src/commands/{run,session}.rs` + tests/AGENTS
- `crates/lattice-agent/src/agent_loop/mod.rs` + focused cancellation tests
- harness fake provider/driver and `tests/direct_failures.rs`
- `FIX.md` after behavior lands

## Verification

```bash
cargo +1.96.0 test -p lattice-profile
cargo +1.96.0 test -p lattice-agent cancel
cargo +1.96.0 test -p lattice-api cancelled
cargo +1.96.0 test -p lattice-cli
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_failures -- --test-threads=1
```
