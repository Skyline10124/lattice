# Issue #24 Slice 0 — Direct-Local harness foundations

## Goal

Make `lattice-system-harness` capable of deterministic, lossless, bounded
Direct-Local scenarios before adding product assertions. This slice changes no
LATTICE product contract and preserves the `LATTICE_BIN`-only boundary.

## Decisions

- A timeout/cancel is a command outcome, not an I/O error: retain stdout,
  stderr, exit/signal information, and process-group cleanup evidence.
- Valid future `TurnEvent` variants remain tolerated; malformed nonblank JSONL
  is retained and fails attested scenarios.
- Fake-provider races use explicit request gates and bounded waits, never sleeps
  as synchronization.
- Secret scanning is recursive and fail-closed. Redaction happens before an
  artifact write; a post-write sentinel hit fails the harness test.
- Whole-scenario budgets cap every command automatically with
  `min(step_timeout, remaining_total)`.

## Code shape

```rust
pub enum Termination {
    Exited(Option<i32>),
    TimedOut(Duration),
    Interrupted,
}

pub struct CommandOutcome {
    pub output: Output,
    pub termination: Termination,
}

pub struct RunningCommand { /* child, pgid, drain handles */ }

impl RunningCommand {
    pub fn interrupt(&mut self) -> io::Result<()>;
    pub fn wait(self, timeout: Duration) -> Result<CommandOutcome, DriverError>;
}
```

`SessionSendDriver::send` returns one lossless execution value containing raw
command output, parsed events, and malformed-line diagnostics. The integration
test-local `raw_send` helper is then removed.

```rust
pub struct ResponseGate { /* Condvar-backed, cloneable */ }

let (server, gate) = FakeLlmServer::start_gated(script)?;
server.wait_for_requests(1, Duration::from_secs(2))?;
// mutate host state deterministically
gate.release();
```

A `ScenarioEvidence` recorder owns the pre-scenario workspace snapshot, command
outcomes, fake-provider captures, secret sentinels, and failure directory.
`finish()` writes redacted evidence only on failure and recursively verifies the
persisted bundle.

## TDD sequence

1. RED/GREEN: timed-out child returns partial stdout/stderr and no live process
   group.
2. RED/GREEN: controlled interrupt reaches the whole child process group and is
   reaped.
3. RED/GREEN: malformed JSONL is retained separately from unknown valid events.
4. RED/GREEN: fake-provider gate waits/releases deterministically and teardown
   joins gated handler threads.
5. RED/GREEN: recursive scanner reports nested byte-level sentinel hits and I/O
   failures.
6. RED/GREEN: evidence writing clears stale scenario directories, redacts before
   write, then fails on any post-write hit.
7. RED/GREEN: remaining total budget shortens a later command timeout.
8. Refactor existing three Direct-Local skeletons onto the new interfaces.

## Files

- `crates/lattice-system-harness/src/{driver,session_send,fake_llm,artifacts,assert,scenario}.rs`
- `crates/lattice-system-harness/src/lib.rs`
- `crates/lattice-system-harness/tests/direct_local_turn.rs`
- `crates/lattice-system-harness/AGENTS.md`

## Verification

```bash
cargo +1.96.0 test -p lattice-system-harness --lib
cargo +1.96.0 xtask harness-boundary
cargo +1.96.0 build -p lattice-cli --bin lattice
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_local_turn
```
