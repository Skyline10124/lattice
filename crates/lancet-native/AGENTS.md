# AGENTS.md

Crate-local guidance for `lancet-native`.

## Role

This crate owns the production native CEP implementation: method handlers,
native server lifecycle, policy/guard/audit integration, verification, edit
application, shell/web helpers, and effect-contract enforcement.

## Sub-Architecture

- `src/handlers/` owns production CEP method handlers.
- `src/server/` owns registry setup, transport lifecycle, and durable call
  metadata.
- `src/enforcement/` owns Guard L1 bridging, isolation probing, and verify
  command support.
- `src/edit/` owns patch parsing, hash checks, application, diagnostics, and
  patch write limits.
- `src/executor/` owns crate-private shell/web/render helpers and output limits.
- `src/effect_contract_tests/` exercises runtime effect contracts.

## Boundaries

- Keep low-level executor helpers crate-private behind native handlers.
- Tool projection belongs in `lancet-cep`; do not add native compatibility
  projection shims.
- Mutations must pass active envelope, Guard L1, effect checks, audit recording,
  and optional verify.
- Do not expose direct native executors to `lattice-agent`.

## Recent Issues

- FIXED M-24-02: command workspace snapshots and artifact lower rechecks run in
  the blocking pool with entry/depth limits.
- FIXED M-24-03: `ToolLimits` was removed; shell timeouts are explicit and
  patch write size is owned by `PatchApplyLimits`.
- FIXED L-30/L-31: cleanup metadata records all cleanup failures, and positive
  commit-baseline hunk summaries are recorded when evidence exists.
- PARTIAL M-05: web fetch rebinding defense uses request-time pinned IP plus a
  maintained rebinding suffix denylist.
- WATCH: tool schemas must continue to come from `lancet-cep` catalog
  projection only.
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
