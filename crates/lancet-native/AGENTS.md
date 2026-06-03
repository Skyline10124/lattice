# AGENTS.md

Crate-local guidance for `lancet-native`.

## Role

This crate owns the production native CEP implementation: method handlers,
native server lifecycle, policy/guard/audit integration, verification, edit
application, shell/web helpers, impact analysis, and catalog-to-tool projection.

## Sub-Architecture

- `src/handlers/` owns production CEP method handlers.
- `src/server/` owns registry setup, transport lifecycle, and durable call
  metadata.
- `src/enforcement/` owns Guard L1 bridging, isolation probing, and verify
  command support.
- `src/edit/` owns patch parsing, hash checks, application, diagnostics, and
  patch write limits.
- `src/executor/` owns crate-private shell/web/render helpers and output limits.
- `src/impact/` owns impact analysis layers.
- `src/projection/` owns catalog-to-LLM tool definition projection.
- `src/effect_contract_tests/` exercises runtime effect contracts.

## Boundaries

- Keep low-level executor helpers crate-private behind native handlers.
- `to_tool_definitions` is the public tool projection path; no parallel
  hard-coded tool schema surface.
- Mutations must pass active envelope, Guard L1, effect checks, audit recording,
  optional invariant checks, and optional verify.
- Do not expose direct native executors to `lattice-agent`.

## Recent Issues

- OPEN M-24-01: `host_pattern` naming implies wildcard host matching but native
  effect-region checks use exact equality.
- OPEN M-24-02: command workspace snapshots synchronously traverse project roots
  and can block tokio workers on large repositories.
- OPEN M-24-03: crate-private `ToolLimits` remains as a hidden native limit
  shape after CEP policy split; only patch write size is meaningfully active.
- PARTIAL M-05: web fetch rebinding defense uses request-time pinned IP plus a
  maintained rebinding suffix denylist.
- WATCH: tool schemas must continue to come from catalog projection only.

## Next Actions

- Resolve M-24-01 with either explicit exact-host naming or wildcard semantics
  across protocol, authority, native, and tests.
- Move workspace snapshot work behind `spawn_blocking` and add depth/file-count
  limits or a cheaper mutation detector.
- Replace `ToolLimits` with a focused patch apply limit or a CEP mutation policy
  field; avoid second command/network/read policy defaults.
- Keep web fetch security centered on request-time DNS pinning; treat denylist
  additions as defense in depth.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` whenever native handlers,
  enforcement, executor helpers, edit logic, or projection behavior changes.
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
