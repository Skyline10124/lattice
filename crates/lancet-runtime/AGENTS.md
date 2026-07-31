# AGENTS.md

Crate-local guidance for `lancet-runtime`.

## Role

ADR 0004 Component engine, closed composition gate, Cell capability
admission, and ToolHost for LATTICE Source Tools (#27 (Codeberg #5)
slices C–D, #28 (Codeberg #6), #30). Owns the wasmtime engine, embedded inventory
collection, the fail-closed composition gate, the admission/authorization
authority, the ToolHost (control plus the `workspace-observe` domain imports),
the concrete Local Workspace module, the `call` execution path with
result/effect validation, and the `contains`/`descriptors`/`descriptor`
surface that `lancet-native` and `lancet-api` use for the D.3 dispatch split,
the D.4 LLM projection, and the per-call admission step.

## Sub-Architecture

- `src/composition.rs` owns `EmbeddedTool`, `ExpectedToolIdentity`,
  `CompositionError`, `ClosedCatalog`, and the `compose` gate. The gate
  verifies descriptor digest, artifact digest, ABI, imports, and channel
  adapters at startup, and refuses a catalog where one capability name maps
  to different cells (#28 C2). It cannot see `lancet-catalog`: a Component
  method name colliding with a native method would arm both vocabularies in
  one call (#28 design §11 hazard) — migration order keeps this unreachable,
  since a migrated method leaves the native catalog in the same patch that
  embeds its Component (#29, #30). The catalog is immutable after `compose`.
- `src/admission.rs` owns `admit_tool` (the fixed enabled → deny → boundary
  stage order), `AdmittedTool`, `AdmissionDenial` (which always carries its
  `AdmissionEvidence`), the per-operation `authorize_operation`, host-owned
  `resolve_scope`, and `validate_policy_against_catalog` (unknown deny name
  = startup error). This is the only authorization authority for Component
  Tools; `CepMode` never softens it.
- `src/runtime.rs` owns `ComponentRuntime`, `ComponentCallContext`,
  `StoreState`, the engine/linker/ticker construction, and the `call`
  method (params validation, pre-cancel short-circuit, instantiation,
  cancellation-aware guest-error mapping, result CellRef validation against
  the descriptor ceiling *and* the admitted set, result schema validation,
  host-computed context measure, Cell evidence recording). Call-scoped
  cancellation state is the sole cancellation authority (#70 (Codeberg #48)):
  pre-call check, guest logical-error path, and epoch abort all converge on
  one shared `CallCancelled` error. `call` takes `&self`
  (the epoch ticker is lazily spawned via `OnceLock`); an
  `Arc<ComponentRuntime>` can be shared across the projection (D.4) and
  server dispatch (D.3) paths.
- `src/history.rs` owns `WorkspaceHistory`, `WorkspaceHistoryBacking`,
  `MutationRecord`, `RecordedMutation`, and `HistorySnapshot` — the private
  durable history of successfully installed Atomic Local Mutations (#63). A
  private **bare** `gix` repository outside the Local Workspace and outside
  the user's project `.git`; Git blobs/trees/commits express before/after
  state, `refs/lattice/workspace-history/current` tracks the newest recorded
  mutation, and `refs/lattice/workspace-history/records/<oid>` pins every
  recorded commit so prior object ids stay reachable without time-based
  expiry. Recording is serialized behind a mutex so concurrent mutations
  chain in real execution order. Never a Git subprocess, never `git2`, never
  a parallel receipt/SQLite store.
- `src/bindings.rs` owns the `wasmtime::component::bindgen!` expansion for
  the Tool Host WIT world (`lattice:tool-host@0.1.0`).
- `src/host.rs` implements the `host-control` interface on `StoreState`
  (cancellation probe + structured events) and binds each effectful import
  to its cell: `emit-event ⇒ Cell{Mutate, None, <session-channel scope>}`
  with target `Session{turn_id}`. `cancelled` is a pure control probe and is
  never gated.
- `src/workspace/` owns the one concrete `LocalWorkspace` (#59/#60/#61,
  Codeberg #37/#38/#39; plus #63; moved down from `lancet-native` in #30):
  secure per-segment path resolution, Policy-Visible Workspace filtering
  (gix Git facts + authority read policy), bounded read/list/grep/find,
  content hashing (Local Revision), no-replace create, atomic edit install,
  private mutation-history recording (`with_history` / `record_mutation`),
  filesystem error normalization, and the Local operation vocabulary
  (`WorkspacePath`, `PathFacts`, `PathIntent`, requests). The async mutation
  primitives delegate to `*_blocking` synchronous cores so the sync host
  imports and the async native handlers share one implementation.
- `src/host_workspace.rs` implements the `workspace-observe` WIT imports on
  `WorkspaceObservationHost` (#30): the four observation operations
  (read/list/grep/find) behind one visibility decision. Every call parses
  the project-relative path, authorizes `alloc:read:workspace` through the
  same `authorize_operation` seam, charges the turn-scoped observation
  budget, applies System Path Exclusion/read-denial/ignored-path inclusion
  through `VisibilityEngine`, enforces `ObservationLimits` byte/line/result
  bounds host-side (recording `truncated` in the effect evidence), and
  reports hidden paths as missing. A host refusal is remembered so the
  structured `CepError` reaches the caller instead of the guest's string
  paraphrase.
- `src/limits.rs` owns `ExecutionLimits`, `RecordingLimits`, and
  `AbortReason`. Resource limits use `StoreLimits` + epoch interruption +
  fuel.
- `src/evidence.rs` owns `CellEvidenceLedger` — `Arc<Mutex>`-backed
  recorder for one call's admission evidence, observed effects, and denied
  operations, ordered by a shared `seq`. Clones share the underlying
  records so the caller can read `snapshot()`/`snapshot_keys()` after the
  call moves one clone into the wasmtime `Store` (D.3).
- `src/error_map.rs` maps wasmtime traps/limits/aborts to structured
  `CepError` codes (`ToolTrap`/`ToolTimeout`/`ToolResourceExhausted`/
  `CallCancelled`) and owns the shared `cancelled_error` used by every
  cancellation observation point (#70). No guest backtrace/host path/secret
  is exposed.
- `src/generated/host_composition.rs` is **generated** by `cargo xtask
  tool-codegen` from the Tool manifests that declare `distribution = "builtin"`
  (#29). It carries their `inventory` registrations (`include_bytes!` of the
  committed descriptor + Component) and the host-pinned `EXPECTED_COMPONENTS`
  identity list. This is the only mechanism that links a Tool into a built
  runtime: a Tool crate's own `generated/registration.rs` is not compiled
  anywhere. Since #29 it embeds `session.todo_write`, so `from_embedded()`
  composes a non-empty catalog in production; #30 added `observe.workspace`,
  and #31/#32 add the remaining three.
  `src/generated/noop_fixture.rs` remains the hand-maintained `test-utils`
  fixture.
- `src/test_helpers.rs` (`test-utils` feature) provides synthetic
  registration/identity construction for gate negative-case tests.

## Boundaries

- No `lancet-native`/`lancet-api`/LATTICE session/agent/profile dependency.
  `lancet-authority` is a lower layer and a legitimate dependency (#30 reads
  its read-visibility policy and system-exclusion matcher). The dependency
  graph has no cycle.
- No `wasmtime-wasi`/`wasi-common` dependency — no ambient WASI. Only
  versioned Tool Host WIT imports are linked.
- `WorkspaceHistory` never touches the Local Workspace or the user's project
  `.git`, never writes a compensation commit, and exposes no user command
  surface. Callers record only after an install has already succeeded.
- No runtime registration API: the catalog is immutable after `compose`.
  The public constructor (`from_embedded`) reads `inventory` + the
  generated `EXPECTED_COMPONENTS` only.
- `test-utils` feature exposes only the fixed no-op fixture constructor
  and synthetic test helpers. Production code never enables it.
- The `host-control` interface is control-only (cancellation + events).
  Domain operations arrive as handle-per-capability domain imports; #30 added
  the first one (`workspace-observe`), and each binds its own cell through
  the same `authorize_operation` seam.
- Every effectful Tool Host import is gated by its host-bound cell and
  concrete target. A denial rejects only that operation: the host facility
  is never invoked, the call continues, and a `DeniedOperation` is recorded
  instead of an effect. Path/host targets are refused outright until a
  domain channel can produce them.
- Admission runs per call, outside this crate (the transport), and is
  handed in through `ComponentCallContext::admitted`. The runtime never
  admits a tool itself, and no legacy effect type takes part in a decision
  here — `error_map.rs` only fills the legacy `EffectErrorData` wire shape
  (its `Vec<ImpactKind>` fields stay empty), which is a named error surface,
  never an authorization input.
- `call` is `&self`: the epoch ticker is lazily spawned on first call via
  `OnceLock` (requires a tokio runtime context; composition does not).
  `set_limits` remains `&mut self` (tests only).

## Verification

- Local: `cargo test -p lancet-runtime`.
- Observation changes: `cargo test -p lancet-runtime --lib host_workspace::`
  plus `LATTICE_BIN=$PWD/target/debug/lattice cargo test -p
  lattice-system-harness --test direct_observation`.
- History changes: `cargo test -p lancet-runtime --lib history::` plus
  `cargo test -p lattice-api --test workspace_history_acceptance`.
- Rewind changes: also `cargo test -p lattice-api --test rewind_acceptance`.
- Clippy: `cargo clippy -p lancet-runtime --all-targets -- -D warnings`.
- No-WASI: `cargo tree -p lancet-runtime | grep -E "wasmtime-wasi|wasi-common"`
  must be empty.
- Protocol: `cargo test -p lancet-protocol` (Tool execution error codes).

## Next Actions

- `WorkspaceHistory` records and plans; the Local Workspace writes stay
  outside it. #64 added `plan_rewind` / `rewind_with`: the walk from
  `current` back to a target identity, the per-path preflight/restore states
  derived from parent trees, and the guarded backwards move of `current`
  (deleting it for an empty target) under the same recording lock. The
  restore itself is the caller's closure — do not pull filesystem writes,
  a second history store, or an Agent-visible history command in here.
- Keep the baseline rule content-based: the parent tree must equal the
  before state the installer observed, since the Local Workspace is shared
  with the user and drifts.

## Record Maintenance

- Update this file in the same patch that changes the runtime's role or
  boundary contract.
- Mirror landed fixes in root `FIX.md` and open/residual items in root
  `FINDINGS.md`.
