# AGENTS.md

Crate-local guidance for `lancet-cep`.

## Role

This crate is the current public CEP facade. It frames the migration interface
by re-exporting protocol/catalog/dispatch contracts and owning CEP-facing tool
exposure and base-authority planning helpers. ADR 0004 replaces hand-maintained
catalog projection with generated Source Tool descriptors and absorbs or retires
this facade through `lancet-runtime` in #34 (Codeberg #12).

## Sub-Architecture

- `src/lib.rs` curates public re-exports from `lancet-protocol`,
  `lancet-catalog`, and `lancet-dispatch`.
- `src/tool.rs` owns `ToolDefinition`, catalog projection, exact-name enabled
  method filtering, `ToolExposure`, and aggregation of descriptor-declared
  `BaseAuthorityRequirement`.
- `src/authority.rs` owns `BaseAuthorityPlan`, which builds
  `BaseExecutionEnvelope` from explicit base-authority inputs while rejecting
  workspace write or command authority.

## Boundaries

- Do not depend on `lancet-native`, `lancet-authority`, `lancet-audit`, or any
  `lattice-*` crate.
- Do not implement native execution, policy side effects, audit persistence, or
  provider-specific LLM behavior here.
- Until generated Source Tool projection becomes production, keep schemas
  derived from `lancet-catalog`; never add a parallel hard-coded schema list or
  a compatibility projection for the target runtime.
- Keep tool exposure separate from policy decisions: exposure aggregates
  catalog-declared requirements, while host lists and policy semantics come from
  the runtime policy layer.

## Recent Issues

- ADDED #31: `mutate.create` and `mutate.edit` left the native catalog with
  their Component migration. Catalog-only exposure now projects `fetch.web`
  alone; the public runtime unions generated Component descriptors elsewhere,
  and this facade must not add compatibility projections for migrated Tools.
- FIXED M-24-04: this crate separates `ToolExposure` from
  `BaseAuthorityPlan`; enabled method exposure now carries catalog-declared
  requirements instead of acting as the authority interface itself.
- FIXED L-llm-coupling: LANCET-facing tool definitions stay here while shared
  URL/DNS safety primitives live in the neutral `net-guard` leaf.
- MIGRATION: do not deepen `ToolDefinition`, `ToolExposure`, or
  `BaseAuthorityPlan` into permanent Tool authority. #27/#28 (Codeberg #5/#6)
  replace their Tool contract role; #34 removes the migration facade.

## Verification

- Local: `cargo test -p lancet-cep`.
- Runtime wiring changes should also run `cargo test -p lancet-api` and focused
  `lattice-api` CEP tests.
