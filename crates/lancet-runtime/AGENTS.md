# AGENTS.md

Crate-local guidance for `lancet-runtime`.

## Role

ADR 0004's sole Component execution path. This crate owns the Wasmtime engine,
generated closed catalog and provider projection, reservation-bound call
controls/events, Cell capability admission, Tool Host enforcement for all five
built-ins, the concrete Local Workspace, pinned GET-only network execution,
Cell evidence, and Component result validation.

## Sub-Architecture

- `src/composition.rs` owns `EmbeddedTool`, `ExpectedToolIdentity`,
  `CompositionError`, `ClosedCatalog`, and the `compose` gate. The gate
  verifies descriptor digest, artifact digest, ABI, imports, and channel
  adapters at startup, and rejects conflicting capability-to-Cell mappings.
  The catalog is immutable after `compose`.
- `src/admission.rs` owns `admit_tool` (the fixed enabled → deny → boundary
  stage order), `AdmittedTool`, `AdmissionDenial` (which always carries its
  `AdmissionEvidence`), the per-operation `authorize_operation`, host-owned
  `resolve_scope`, and `validate_policy_against_catalog` (unknown deny name
  = startup error). This is the only authorization authority for Component
  Tools; admission is always enforced.
- `src/catalog.rs` projects provider-visible Tool definitions directly from the
  immutable generated catalog and rejects unknown enabled names.
- `src/control.rs` owns call-scoped cancellation, ordered host-bound Tool events,
  and execution receipts carrying typed Cell evidence.
- `src/execution.rs` owns `RuntimeExecution`, the sole per-Turn composition of
  Component runtime, admitted call context, hosts, budgets, Local Workspace, and
  WorkspaceHistory. It validates exact durable bindings before every call.
- `src/runtime.rs` owns `ComponentRuntime`, `ComponentCallContext`,
  `StoreState`, the engine/linker/ticker construction, and the `call`
  method (params validation, pre-cancel short-circuit, instantiation,
  cancellation-aware guest-error mapping, result CellRef validation against
  the descriptor ceiling *and* the admitted set, result schema validation,
  host-computed context measure, Cell evidence recording). Call-scoped
  cancellation state is the sole cancellation authority (#70 (Codeberg #48)):
  pre-call check, guest logical-error path, and epoch abort all converge on
  one shared `CallCancelled` error. `call` takes `&self`, launches Wasmtime on a
  blocking worker, and drives async imports through the composed catalog's bounded
  process-level Tokio runtime (two async workers, at most eight blocking DNS tasks).
  The process-level epoch ticker is lazy and the `Arc<ComponentRuntime>` remains
  shareable across catalog projection and `RuntimeExecution`.
- `src/history.rs` owns `WorkspaceHistory`, `WorkspaceHistoryBacking`,
  `MutationRecord`, `RecordedMutation`, and `HistorySnapshot` — the private
  durable history of successfully installed Atomic Local Mutations (#63),
  built on the `lattice-store` kernel since #143. A project-scoped store
  outside the Local Workspace and outside the user's project `.git`; Git
  blobs/trees/commits express before/after state and
  `refs/lattice/workspace-history/current` tracks the newest recorded
  mutation. Normal recording writes no per-record refs (everything it appends
  is an ancestor of `current`); a Rewind retains the tip it abandons through
  the kernel's `refs/lattice/retained/<oid>` pin, which is what keeps prior
  object ids resolvable without time-based expiry. Recording and rewinding
  serialize on the kernel's repository write lock. Never a Git subprocess,
  never `git2`, never a parallel receipt/SQLite store.
- `src/bindings.rs` owns the `wasmtime::component::bindgen!` expansion for
  the Tool Host WIT world (`lattice:tool-host@0.1.0`).
- `src/host.rs` implements the `host-control` interface on `StoreState`
  (cancellation probe + structured events) and binds each effectful import
  to its cell: `emit-event ⇒ Cell{Mutate, None, <session-channel scope>}`
  with target `Session{turn_id}`. `cancelled` is a pure control probe and is
  never gated.
- `src/workspace/` owns the one concrete `LocalWorkspace`: secure per-segment
  resolution, Policy-Visible Workspace filtering, bounded read/list/grep/find,
  content hashing, no-replace create, atomic edit install, private mutation
  history recording, error normalization, and the Local operation vocabulary.
  Synchronous cores serve Wasmtime Tool Host imports without nesting a Tokio
  runtime.
- `src/host_workspace.rs` implements the `workspace-observe` WIT imports on
  `WorkspaceObservationHost` (#30): the four observation operations
  (read/list/grep/find) behind one visibility decision. Every call parses
  the project-relative path, authorizes `alloc:read:workspace` through the
  same `authorize_operation` seam, charges the turn-scoped observation
  budget, applies System Path Exclusion/read-denial/ignored-path inclusion
  through `VisibilityEngine`, enforces `ObservationPolicyConfig` byte/line/result
  bounds host-side, and reports hidden paths as missing. Read imports expose
  one host-rendered window so budget/evidence match the exact visible content;
  the Component owns only the outer union result document.
  Effect evidence records both host-computed truncation and content-free
  visibility decisions. A host refusal is remembered so the structured
  `CepError` reaches the caller instead of the guest's string paraphrase.
- `src/host_mutate.rs` implements the `workspace-mutate` WIT imports on
  `WorkspaceMutationHost` (#31): `read-for-edit`, `create-if-absent`,
  `replace-with-expected`, and non-installing `refuse-edit`. Every call re-resolves the path, authorizes
  `mutate:write:workspace`, enforces the single `MAX_WORKSPACE_WRITE_BYTES`
  cap (#120), applies System Path Exclusion / write-deny / Mutation
  Preconditions, installs atomically (or refuses before any Local bytes
  change), appends private history after a successful install, and records
  MutationEvidence. If the atomic name installation succeeds but the following
  parent-directory fsync fails, the Host keeps the installed receipt, appends
  history and installed evidence, then returns `ExecutionFailed`; it never
  relabels changed bytes as an uninstalled refusal. Early refusals (parse / guard / size) call
  `record_refusal` so the typed CEP code outranks the guest `Err(String)`.
- `src/host_network.rs` implements the async `network-fetch` import (#32). It
  validates policy before I/O, resolves and pins each connection, verifies TLS
  with platform roots, follows at most five revalidated redirects, strips caller
  headers on cross-origin redirects, disables compression, caps headers and the
  final shaped body, and shares the Turn observation budget/deadline/cancellation.
  Per-hop evidence strips userinfo/query/fragment, distinguishes host-received bytes
  from bytes delivered to the guest, and records every post-I/O failure as an
  `ObservedEffect`; `DeniedOperation` remains pre-I/O only.
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
  and #31 added `mutate.create` / `mutate.edit`; #32 added `fetch.web`. The
  production composition now contains all five built-ins.
  `src/generated/noop_fixture.rs` remains the hand-maintained `test-utils`
  fixture.
- `src/test_helpers.rs` (`test-utils` feature) provides synthetic
  registration/identity construction for gate negative-case tests.

## Boundaries

- No `lancet-api` or LATTICE session/agent/profile dependency.
  `lancet-authority` is a lower layer and a legitimate dependency for network,
  read-visibility, Guard L1, and System Path Exclusion policy.
- No `wasmtime-wasi`/`wasi-common` dependency — no ambient WASI. Only
  versioned Tool Host WIT imports are linked.
- `WorkspaceHistory` never touches the Local Workspace or the user's project
  `.git`, never writes a compensation commit, and exposes no user command
  surface. Callers record only after an install has already succeeded.
- ADR 0005/#143 moved WorkspaceHistory's repository/ref/lock/fsync mechanics
  to `lattice-store`, not its mutation/rewind semantics. This crate owns the
  typed history handle and Local mutation integration and must not hand-roll
  durability primitives. Never merge workspace history with
  sessions/invocations or hold its repository write lock together with another
  repository lock.
- The history store is project-scoped
  (`projects/<ProjectStoreKey>/workspace-history.git`) and strictly opened:
  `WorkspaceHistory::open` fails closed on a missing store, and `create` is
  reserved for the `session new` composition path.
- The per-project `WorkspaceOperationLock`
  (`locks/projects/<ProjectStoreKey>/workspace.lock`) is reached through
  `WorkspaceHistory::lock_workspace_operation` /
  `LocalWorkspace::lock_operation`. Observation takes it shared; mutation
  holds it exclusive from the precondition check through atomic install and
  history publication; rewind holds it across plan/restore/ref move. The
  kernel repository write lock is strictly narrower and is taken underneath
  it, never the reverse, and a shared hold is never upgraded.
- No runtime registration API: the catalog is immutable after `compose`.
  The public constructor (`from_embedded`) reads `inventory` + the
  generated `EXPECTED_COMPONENTS` only.
- `test-utils` feature exposes only the fixed no-op fixture constructor
  and synthetic test helpers. Production code never enables it.
  Domain operations use handle-per-capability imports: `workspace-observe`,
  `workspace-mutate`, and async `network-fetch`. Each binds its own Cell through
  `authorize_operation`.
- Every effectful Tool Host import is gated by its host-bound cell and
  concrete target. A denial rejects only that operation: the host facility
  is never invoked, the call continues, and a `DeniedOperation` is recorded
  instead of an effect. Path targets require workspace scopes; host targets
  require network scopes. Other target kinds fail closed.
- `RuntimeExecution` performs admission per call after exact durable-binding and
  reservation checks, then passes the resulting `AdmittedTool` through
  `ComponentCallContext`. No legacy effect vocabulary participates in a decision.
- `call` is `&self`: the epoch ticker is lazily spawned on first call via
  `OnceLock` (requires a tokio runtime context; composition does not).
  `set_limits` remains `&mut self` (tests only).

## Verification

- Local: `cargo test -p lancet-runtime`.
- Observation changes: `cargo test -p lancet-runtime --lib host_workspace::`
  plus `LATTICE_BIN=$PWD/target/debug/lattice cargo test -p
  lattice-system-harness --test direct_observation`.
- Network changes: `cargo test -p lancet-runtime --lib host_network::` plus the
  relevant `lattice-system-harness` Tool round trips.
- Mutation changes: `cargo test -p lancet-runtime --lib host_mutate::` plus
  `LATTICE_BIN=$PWD/target/debug/lattice cargo test -p lattice-system-harness
  --test direct_mutation --test direct_failures --test direct_rewind
  --test direct_workspace_history`.
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
  (deleting it for an empty target) under the kernel's repository write lock,
  after retaining the abandoned tip. The
  restore itself is the caller's closure — do not pull filesystem writes,
  a second history store, or an Agent-visible history command in here.
- Keep the baseline rule content-based: the parent tree must equal the
  before state the installer observed, since the Local Workspace is shared
  with the user and drifts.
- Keep the operation-lock order intact when touching observation or mutation:
  `WorkspaceOperationLock` first, at most one repository write lock under it,
  no upgrade from shared. A new mutation entry point must take the exclusive
  hold before its precondition read, not after.

## Record Maintenance

- Update this file in the same patch that changes the runtime's role or
  boundary contract.
- Mirror landed fixes in root `FIX.md` and open/residual items in root
  `FINDINGS.md`.
