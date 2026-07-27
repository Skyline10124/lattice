# AGENTS.md

Crate-local guidance for `lancet-runtime`.

## Role

ADR 0004 Component engine, closed composition gate, and minimal ToolHost
for LATTICE Source Tools (#27 (Codeberg #5) slices C–D). Owns the wasmtime
engine, embedded inventory collection, the fail-closed composition gate,
the control-only ToolHost, the `call` execution path with result/effect
validation, and the `contains`/`descriptors` surface that `lancet-native`
and `lancet-api` use for the D.3 dispatch split and D.4 LLM projection.

## Sub-Architecture

- `src/composition.rs` owns `EmbeddedTool`, `ExpectedToolIdentity`,
  `CompositionError`, `ClosedCatalog`, and the `compose` gate. The gate
  verifies descriptor digest, artifact digest, ABI, imports, and channel
  adapters at startup. The catalog is immutable after `compose`.
- `src/runtime.rs` owns `ComponentRuntime`, `ComponentCallContext`,
  `StoreState`, the engine/linker/ticker construction, and the `call`
  method (params validation, pre-cancel short-circuit, instantiation,
  cancellation-aware guest-error mapping, result CellRef validation,
  result schema validation, Cell evidence recording). Call-scoped
  cancellation state is the sole cancellation authority (#70 (Codeberg #48)):
  pre-call check, guest logical-error path, and epoch abort all converge on
  one shared `CallCancelled` error. `call` takes `&self`
  (the epoch ticker is lazily spawned via `OnceLock`); an
  `Arc<ComponentRuntime>` can be shared across the projection (D.4) and
  server dispatch (D.3) paths.
- `src/bindings.rs` owns the `wasmtime::component::bindgen!` expansion for
  the Tool Host WIT world (`lattice:tool-host@0.1.0`).
- `src/host.rs` implements the `host-control` interface on `StoreState`
  (cancellation probe + structured events).
- `src/limits.rs` owns `ExecutionLimits`, `RecordingLimits`, and
  `AbortReason`. Resource limits use `StoreLimits` + epoch interruption +
  fuel.
- `src/evidence.rs` owns `CellEvidenceLedger` — `Arc<Mutex>`-backed
  recorder for one call's `(Cell, Trust)` pairs. Clones share the
  underlying records so the caller can read `snapshot_keys()` after the
  call moves one clone into the wasmtime `Store` (D.3).
- `src/error_map.rs` maps wasmtime traps/limits/aborts to structured
  `CepError` codes (`ToolTrap`/`ToolTimeout`/`ToolResourceExhausted`/
  `CallCancelled`) and owns the shared `cancelled_error` used by every
  cancellation observation point (#70). No guest backtrace/host path/secret
  is exposed.
- `src/generated/` owns the production `EXPECTED_COMPONENTS` list (empty
  until #29–#32 (Codeberg #7–#10)) and the `test-utils` no-op fixture.
- `src/test_helpers.rs` (`test-utils` feature) provides synthetic
  registration/identity construction for gate negative-case tests.

## Boundaries

- No `lancet-native`/`lancet-api`/LATTICE session/agent/profile dependency.
  The dependency graph has no cycle.
- No `wasmtime-wasi`/`wasi-common` dependency — no ambient WASI. Only
  versioned Tool Host WIT imports are linked.
- No runtime registration API: the catalog is immutable after `compose`.
  The public constructor (`from_embedded`) reads `inventory` + the
  generated `EXPECTED_COMPONENTS` only.
- `test-utils` feature exposes only the fixed no-op fixture constructor
  and synthetic test helpers. Production code never enables it.
- The `host-control` interface is control-only (cancellation + events).
  No workspace/network/session domain operation before #28 (Codeberg #6)
  capability admission.
- `call` is `&self`: the epoch ticker is lazily spawned on first call via
  `OnceLock` (requires a tokio runtime context; composition does not).
  `set_limits` remains `&mut self` (tests only).

## Verification

- Local: `cargo test -p lancet-runtime`.
- Clippy: `cargo clippy -p lancet-runtime --all-targets -- -D warnings`.
- No-WASI: `cargo tree -p lancet-runtime | grep -E "wasmtime-wasi|wasi-common"`
  must be empty.
- Protocol: `cargo test -p lancet-protocol` (Tool execution error codes).

## Record Maintenance

- Update this file in the same patch that changes the runtime's role or
  boundary contract.
- Mirror landed fixes in root `FIX.md` and open/residual items in root
  `FINDINGS.md`.
