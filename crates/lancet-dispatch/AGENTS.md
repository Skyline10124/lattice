# AGENTS.md

Crate-local guidance for `lancet-dispatch`.

## Role

This crate owns the current generic CEP dispatch path: typed registration,
JSON-RPC transport, client/server channels, event sinks, call control,
cancellation, and server runtime plumbing. ADR 0004 absorbs this behavior into
`lancet-runtime` in #34 (Codeberg #12); the crate is migration infrastructure.

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
- Do not add a runtime plugin registry, dynamic Component loader, or new public
  abstraction intended to survive #34. Preserve current behavior while the
  generated closed-catalog runtime replaces this crate.

## Recent Issues

- FIXED #158: transport task/cancellation tracking uses a private per-request
  key rather than `InvocationId`; every terminal task path removes it, and
  response loss after the server marks durable `DispatchStarted` returns typed
  `OutcomeIndeterminate`. `EventSink` always overwrites payload identity with
  the host-bound invocation.
- CHANGED #70 (Codeberg #48): after cancellation the client call future
  resolves with the server-mapped structured outcome (the server observes the
  same watch and is the outcome authority); the client-synthesized
  `call_cancelled()` and its re-exports are deleted, and `start_call_raw` is
  `#[doc(hidden)] pub`. Drop-future `CancelGuard` semantics are unchanged.
- ADDED #27 (Codeberg #5) slice D: `CepClient::call_named<P, R>` —
  name-dynamic, type-static params, sends a raw JSON-RPC call, and
  deserializes the result without carrying a JSON Schema; Component
  params/result validation is done by generated descriptor validators on the
  server side. #34 absorbs this helper into `lancet-runtime`.
- FIXED P19-CR-01: cancellation senders were previously tracked by vector
  truncation; unordered completion could lose live handler cancel handles.
- FIXED P19-CR-02: `EventSink::emit` previously awaited on bounded channels and
  could deadlock ordinary calls that did not drain progress events.
- ADDED #27 slice C: `EventSink::emit_sync` — synchronous event emit for
  Component ToolHost imports that run inside synchronous wasmtime calls.
  Same best-effort semantics as `emit` (closed/full channel doesn't fail).
- WATCH: native and API live streaming depend on dispatch not blocking server
  progress when observers are slow or absent.
- WATCH: JSON-RPC wire-code mapping must stay consistent with
  `lancet-protocol::CepError`.

## Next Actions

- Keep cancellation keyed by the private per-request transport key and test
  shutdown with duplicate InvocationIds and out-of-order handler completion.
- Keep progress/event sends best-effort where backpressure would block call
  completion.
- If transport lifecycle changes, run native policy/evidence and API
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
