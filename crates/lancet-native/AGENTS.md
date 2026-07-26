# AGENTS.md

Crate-local guidance for `lancet-native`.

## Role

This crate owns the current production native CEP implementation: method
handlers, server lifecycle, policy/guard/audit integration, edit application,
web helpers, and effect-contract enforcement. ADR 0004 migrates every Tool to a
WASM Component and absorbs the remaining concrete host responsibilities into
`lancet-runtime`; do not treat this crate as a permanent Tool extension point.

## Sub-Architecture

- `src/handlers/` owns production CEP method handlers.
- `src/server/` owns registry setup, transport lifecycle, and durable call
  metadata.
- `src/enforcement/` owns Guard L1 bridging and preflight metadata.
- `src/edit/` owns patch parsing, hash checks, application, diagnostics, and
  patch write limits.
- `src/workspace/` owns the one concrete `LocalWorkspace` (#37/#38/#39): secure
  per-segment path resolution, Policy-Visible Workspace filtering (gix Git
  facts + authority read policy), bounded read/list/grep/find, content hashing
  (Local Revision), no-replace create, atomic edit install, filesystem error
  normalization, and the Local operation vocabulary (`WorkspacePath`,
  `PathFacts`, `PathIntent`, requests). ADR 0004 moves this into
  `lancet-runtime`.
- `src/executor/` owns crate-private web/render helpers and output limits.
- `src/effect_contract_tests/` exercises runtime effect contracts.

## Boundaries

- Keep low-level executor helpers crate-private behind native handlers.
- Tool projection belongs in `lancet-cep`; do not add native compatibility
  projection shims.
- Mutations must pass active envelope, Guard L1, effect checks, and audit
  recording. Post-mutation verify is retired; do not reintroduce it.
- Do not expose direct native executors to `lattice-agent`.
- Do not add new native Tool implementations or a native Tool ABI. New Tool
  contracts start in Source manifests; Components receive authority only through
  Tool Host imports.
- The concrete guarded Local Workspace is the single workspace path. The
  `ExecutionBackend`/routing seam was deleted in #38 — do not reintroduce a
  pluggable workspace implementation, routing enum, fallback, or parallel
  path. #12 moves the concrete host module into `lancet-runtime` after Tool
  migration.

## Recent Issues

- ADDED #28: the Component branch of `transport.rs` runs `admit_tool` against
  `cep_context.policy.capability_admission` before `rt.call`. A denial answers
  `CapabilityNotGranted` and still finalizes the audit row with its
  `AdmissionEvidence` — a Component call never finalizes with absent metadata.
  An admitted call carries the admitted set into `ComponentCallContext`, and
  finalize merges admission evidence, observed effects, and denied operations.
  `ServerRunConfig::validate_capability_admission` makes an unknown deny name a
  startup error at `run_native`. Admission is unconditional: `CepMode` is not
  consulted. The legacy pre-dispatch deny and all native handlers are untouched.
- FIXED #48: call-scoped cancellation is audited from the structured code,
  not a message string — `finalize_audit_for_dispatch` records
  `AuditOutcome::Cancelled` on `CepErrorCode::CallCancelled` (the old
  exact-message arm was unreachable dead code), and `is_incomplete_dispatch`
  matches the code only. Component and (future) native cancellations share
  one classification authority.
- ADDED #5 slice D: `ServerRunConfig` carries `Option<Arc<ComponentRuntime>>`.
  `handle_request` branches Component vs native dispatch after audit start
  and before finalize, sharing one durable audit path. Component calls
  produce Cell evidence (`component_cells` in audit metadata); native calls
  produce effect-ledger evidence as before. Production `from_embedded()`
  yields an empty Component catalog so all 5 native methods are unchanged.
- FIXED #38: `ExecutionBackend`/`ExecutionRouting`/`UpperContext`/
  `RoutingInvocationState`/`NullExecutionBackend` and all spy backends
  deleted; handlers and the edit engine call the concrete `LocalWorkspace`
  directly; audit routing metadata replaced with method-local evidence
  (guarded path, `local_revision`, post-install content hash,
  `PathGuard`/`LocalIo` rejection stages); narrow one-shot
  `fail_next_install` fault injection is the only test seam.
- FIXED #39 follow-up: ignored ancestors reachable only for an exact included
  descendant are navigable but not emitted by list/find; successful
  read/list/grep/find audit metadata now records content-free visibility rule
  categories without hidden paths, content, counts, or hashes.
- FIXED M-24-02: command workspace snapshots and artifact lower rechecks run in
  the blocking pool with entry/depth limits.
- FIXED M-24-03: `ToolLimits` was removed; shell timeouts are explicit and
  patch write size is owned by `PatchApplyLimits`.
- FIXED L-30/L-31: cleanup metadata records all cleanup failures, and positive
  commit-baseline hunk summaries are recorded when evidence exists.
- PARTIAL M-05: web fetch rebinding defense uses request-time pinned IP plus a
  maintained rebinding suffix denylist.
- WATCH: current schemas must continue to come from `lancet-cep` projection until
  generated descriptors replace it; never hand-write a native or Component
  compatibility schema.
- CLOSED 31-CR-01/WR-02: artifact commit delete paths reject symlink parents,
  and commit-baseline anchor files are read from the captured lower baseline.

## Next Actions

- If future fetch capabilities need wildcard hosts, implement semantics across
  protocol, authority, native, and tests instead of using pattern wording alone.
- If mutation write budgets become profile-managed, lift `PatchApplyLimits` into
  CEP mutation policy rather than reviving generic native tool limits.
- Keep web fetch security centered on request-time DNS pinning; treat denylist
  additions as defense in depth.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` whenever native handlers,
  enforcement, executor helpers, edit logic, or impact behavior changes.
- Keep open/deferred/residual native issues in root `FINDINGS.md`.
- Add landed native fixes to root `FIX.md` with verification and changed files.
- For partial fixes, update this file and leave explicit residual risk in
  `FINDINGS.md`.
- Keep this file near 80 lines by pruning stale closed issue notes.

## Verification

- Local: `cargo test -p lancet-native`.
- Handler/lifecycle changes: also run affected `lattice-api` round-trip,
  policy, provenance, and release tests.
- Edit/security changes need focused regression tests for path, symlink, scope,
  verify, and effect-region behavior.
