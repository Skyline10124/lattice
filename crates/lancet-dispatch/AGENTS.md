# AGENTS.md

Crate-local guidance for `lancet-dispatch`.

## Role

This crate owns generic CEP dispatch: typed method registration, JSON-RPC
transport, client/server channels, event sinks, call control, cancellation, and
server runtime plumbing.

## Sub-Architecture

- `src/registry.rs` owns `MethodRegistry`, typed handler adapters, duplicate
  registration checks, cancellation helpers, and error-code mapping.
- `src/transport.rs` owns `CepTransport`, `CepClient`, `CepServer`,
  `ServerCall`, `CallControl`, and `ServerRuntime`.
- Registry code understands catalog methods but not native business logic.
- Transport code owns in-process request/response/event movement and shutdown.
- Event sinks are intentionally generic; native lifecycle maps them to audit and
  public events elsewhere.

## Boundaries

- No native shell/filesystem/web execution.
- No authority policy decisions, audit persistence, or effect-region semantics.
- Keep method-specific behavior in catalog/native crates.
- Preserve cancellation and event ordering when changing transport internals.
- Avoid adding dependencies that would make this crate a runtime policy layer.

## Recent Issues

- FIXED P19-CR-01: cancellation senders were previously tracked by vector
  truncation; unordered completion could lose live handler cancel handles.
- FIXED P19-CR-02: `EventSink::emit` previously awaited on bounded channels and
  could deadlock ordinary calls that did not drain progress events.
- WATCH: native and API live streaming depend on dispatch not blocking server
  progress when observers are slow or absent.
- WATCH: JSON-RPC wire-code mapping must stay consistent with
  `lancet-protocol::CepError`.

## Next Actions

- Keep cancellation keyed by call id and test shutdown with out-of-order handler
  completion.
- Keep progress/event sends best-effort where backpressure would block call
  completion.
- If transport lifecycle changes, run native policy/provenance and API
  round-trip tests, not only local unit tests.
- Add tests before changing error mapping, cancellation semantics, or event sink
  behavior.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` in this file when dispatch behavior
  changes or a new transport/cancellation risk is found.
- Open unresolved dispatch risks in root `FINDINGS.md`.
- Record landed dispatch fixes in root `FIX.md` with the verification commands
  used.
- Keep local issue bullets short; this file is for current navigation, while
  `FIX.md` is the permanent changelog.
- Keep this file near 80 lines by removing stale closed notes after they stop
  guiding edits.

## Verification

- Local: `cargo test -p lancet-dispatch`.
- Transport lifecycle changes: also run `cargo test -p lancet-native` and
  affected `lattice-api` streaming/round-trip tests.
- Cancellation changes need tests for cancel-before-start, cancel-during-call,
  shutdown, and unordered completion.
