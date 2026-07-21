# AGENTS.md

Crate-local guidance for `lancet-runtime`.

## Role

ADR 0004 Component engine, closed composition gate, and minimal ToolHost
for LATTICE Source Tools (#5 slice C). Owns the wasmtime engine, embedded
inventory collection, the fail-closed composition gate, the control-only
ToolHost, and the `call` execution path with result/effect validation.

## Sub-Architecture

- `src/composition.rs` owns `EmbeddedTool`, `ExpectedToolIdentity`,
  `CompositionError`, `ClosedCatalog`, and the `compose` gate. The gate
  verifies descriptor digest, artifact digest, ABI, imports, and channel
  adapters at startup. The catalog is immutable after `compose`.
- `src/runtime.rs` owns `ComponentRuntime`, `ComponentCallContext`,
  `StoreState`, the engine/linker/ticker construction, and the `call`
  method (params validation, instantiation, result CellRef validation,
  result schema validation, Cell evidence recording).
- `src/bindings.rs` owns the `wasmtime::component::bindgen!` expansion for
  the Tool Host WIT world (`lattice:tool-host@0.1.0`).
- `src/host.rs` implements the `host-control` interface on `StoreState`
  (cancellation probe + structured events).
- `src/limits.rs` owns `ExecutionLimits`, `RecordingLimits`, and
  `AbortReason`. Resource limits use `StoreLimits` + epoch interruption +
  fuel.
- `src/evidence.rs` owns `CellEvidenceLedger` — in-memory recorder for
  one call's `(Cell, Trust)` pairs.
- `src/error_map.rs` maps wasmtime traps/limits/aborts to structured
  `CepError` codes (`ToolTrap`/`ToolTimeout`/`ToolResourceExhausted`/
  `CallCancelled`). No guest backtrace/host path/secret is exposed.
- `src/generated/` owns the production `EXPECTED_COMPONENTS` list (empty
  until #7–#10) and the `test-utils` no-op fixture.
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
  No workspace/network/session domain operation before #6 capability
  admission.
- `call` is `&mut self` because the epoch ticker is lazily spawned on
  first call (requires a tokio runtime context; composition does not).

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
