# AGENTS.md

Crate-local guidance for `lancet-cep`.

## Role

This crate is the transitional public CEP facade. It re-exports
`lancet-protocol`, `lancet-catalog`, and `lancet-dispatch`, and keeps generic
Tool definition/exposure and legacy base-authority helpers until ADR 0004
GitHub issue 34 removes or absorbs them.

The production native catalog is empty after #32. Runtime built-in definitions
come from generated Component descriptors, not catalog projection.

## Sub-architecture

- `src/lib.rs` curates protocol/catalog/dispatch re-exports.
- `src/tool.rs` owns generic `ToolDefinition`, exact-name exposure filtering, and
  projection from any residual native catalog entries. Production projection of
  the empty catalog is intentionally empty.
- `src/authority.rs` owns the residual `BaseAuthorityPlan` used by generic native
  and scaffold paths. Component authority comes from Cell admission and Tool Host
  checks.

## Boundaries

- Do not depend on `lancet-native`, `lancet-runtime`, `lancet-authority`,
  `lancet-audit`, or any `lattice-*` crate.
- Do not add built-in schemas or compatibility definitions here. Start Tool
  contracts in Source manifests and use generated descriptors.
- Keep projection separate from execution policy and provider name encoding.
- Do not deepen this migration facade into permanent Tool authority.

## Verification

- Local: `cargo test -p lancet-cep`.
- Runtime wiring changes: run `cargo test -p lancet-api` and focused
  `lattice-api` CEP tests.

## Record maintenance

Update this file when the remaining facade is removed or its consumers change.
Record landed fixes in root `FIX.md`; keep only open residuals in `FINDINGS.md`.
