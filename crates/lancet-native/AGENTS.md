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
- During Direct-Local migration keep one concrete guarded Local Workspace here.
  Do not preserve `ExecutionBackend` or another pluggable host seam; #12 moves
  the concrete host module into `lancet-runtime` after Tool migration.

## Recent Issues

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
