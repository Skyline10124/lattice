# AGENTS.md

Crate-local guidance for `lancet-cep`.

## Role

This crate is the public CEP facade. It frames the CEP interface by re-exporting
the protocol/catalog/dispatch contracts and by owning CEP-facing tool exposure
and base authority planning helpers.

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
- Keep tool schemas derived from `lancet-catalog`; never add a parallel hard-
  coded production schema list.
- Keep tool exposure separate from policy decisions: exposure aggregates
  catalog-declared requirements, while host lists and policy semantics come from
  the runtime policy layer.

## Recent Issues

- FIXED M-24-04: this crate separates `ToolExposure` from
  `BaseAuthorityPlan`; enabled method exposure now carries catalog-declared
  requirements instead of acting as the authority interface itself.
- FIXED L-llm-coupling: LANCET-facing tool definitions stay here while shared
  URL/DNS safety primitives live in the neutral `net-guard` leaf.

## Verification

- Local: `cargo test -p lancet-cep`.
- Runtime wiring changes should also run `cargo test -p lancet-api` and focused
  `lattice-api` CEP tests.
