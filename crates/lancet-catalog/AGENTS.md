# AGENTS.md

Crate-local guidance for `lancet-catalog`.

## Role

This crate owns the typed CEP method catalog: method identifiers, params and
results, descriptors, required capabilities, schema metadata, and the static
production catalog used by native handlers and tool projection.

## Sub-Architecture

- `src/method.rs` defines `CepMethod`, `CatalogMethod`, and
  `MethodDescriptor`.
- `src/catalog.rs` builds the production `CATALOG` and optional scaffold
  catalog behind `inspect-scaffold`.
- `src/methods/workspace.rs` owns observe and mutate workspace contracts.
- `src/methods/command.rs` owns command execution contracts.
- `src/methods/external.rs` owns `fetch.web` contracts.
- `src/methods/session.rs` owns session state helper contracts.
- `src/methods/envelope.rs` owns `session.envelope` authority contracts.
- `src/methods/audit.rs` owns audit query/replay contracts.
- `src/methods/scaffold.rs` is feature-gated inspection scaffolding only.

## Boundaries

- Depend only on protocol plus serialization/schema support.
- `CATALOG` is the authoritative production method set.
- Keep the production catalog aligned with the root 9-method CEP contract.
- Do not define LLM-facing tool schemas here; `lancet-native` projects them.
- Keep method params/results, descriptors, and capabilities in sync.

## Recent Issues

- FIXED P19: catalog sealing compile-fail coverage was corrected during CEP
  core review.
- WATCH M-24-04: `tools.enabled` in `lattice-api` currently affects both LLM
  tool exposure and base authority derivation; catalog metadata should not
  become an implicit policy layer.
- WATCH: dotted method names stay canonical in CEP catalog; provider-specific
  name encoding belongs in `lattice-llm` wire adapters.
- WATCH: optional scaffold methods must remain feature-gated and outside the
  production 9-method catalog.

## Next Actions

- For any new or changed method, update params/results, descriptor metadata,
  required capabilities, native handlers, tool projection tests, and API mapping.
- If tool exposure/authority splitting happens for M-24-04, keep catalog
  descriptors policy-neutral and move authority derivation out of exposure code.
- Add regression tests when descriptor schemas, capability requirements, or
  method names change.
- Keep scaffold additions behind `inspect-scaffold` and verify production
  catalog length remains unchanged unless the root contract is intentionally
  updated.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` whenever catalog semantics,
  descriptor metadata, or method count changes.
- Record open/deferred/residual catalog issues in root `FINDINGS.md`.
- Record landed catalog fixes in root `FIX.md` with verification and touched
  files.
- Do not let this file become a full changelog; keep local bullets current and
  leave durable history in `FIX.md`.
- Keep this file near 80 lines by pruning stale closed bullets.

## Verification

- Local: `cargo test -p lancet-catalog`.
- Method contract changes: also run affected `lancet-native` and `lattice-api`
  tests.
- Production catalog changes: assert catalog membership, descriptor schema, and
  tool projection behavior.
