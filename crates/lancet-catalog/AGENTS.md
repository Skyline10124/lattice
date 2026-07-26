# AGENTS.md

Crate-local guidance for `lancet-catalog`.

## Role

This crate owns the current typed CEP method catalog used by native handlers and
tool projection. It is a migration authority, not the ADR 0004 target: Source
Tool manifests and generated descriptors replace it after Component migration,
and #12 removes the remaining central catalog surface.

## Sub-Architecture

- `src/method.rs` defines `CepMethod`, `CatalogMethod`, `MethodDescriptor`, and
  `BaseAuthorityRequirement`.
- `src/catalog.rs` builds the production `CATALOG` and optional scaffold
  catalog behind `inspect-scaffold`.
- `src/methods/workspace.rs` owns observe and mutate workspace contracts.
- `src/methods/external.rs` owns `fetch.web` contracts.
- `src/methods/session.rs` owns session state helper contracts.
- `src/methods/audit.rs` owns operator audit query/tail/replay DTOs (not agent catalog methods).
- `src/methods/scaffold.rs` is feature-gated inspection scaffolding only.

## Boundaries

- Depend only on protocol plus serialization/schema support.
- `CATALOG` remains authoritative for the current production path only. Do not
  create a third schema beside it and the ADR 0004 Source Tool manifest work.
- Keep the current default catalog aligned with its five built-ins, but do not
  treat that set as a permanent global limit; closed runtime composition belongs
  to generated registrations in the target architecture.
- Do not define LLM-facing tool schemas here; `lancet-cep` projects them from
  descriptors and `lancet-native` exposes the native path.
- Keep method params/results, descriptors, and capabilities in sync.
- Keep base authority requirements method-local and policy-neutral; catalog may
  say that `fetch.web` needs fetch authority, but not which hosts are allowed.
- Do not add operator-only contracts here. #17 moves existing audit DTOs to
  `lancet-audit`; operator audit is not a Tool.

## Recent Issues

- FIXED P19: catalog sealing compile-fail coverage was corrected during CEP
  core review.
- FIXED M-24-04: `MethodDescriptor::base_authority` now declares method-local
  base authority requirements consumed by `lancet-cep::ToolExposure`; policy
  inputs such as allowed hosts stay outside catalog.
- FIXED issue #11: production `CATALOG` is 5 agent methods; audit DTOs in
  `methods/audit.rs` are operator-only (not registered in `CATALOG`).
- WATCH: dotted method names stay canonical in CEP catalog; provider-specific
  name encoding belongs in `lattice-llm` wire adapters.
- WATCH: optional scaffold methods must remain feature-gated and outside the
  production 5-method agent catalog.

## Next Actions

- Do not add new production Tools by extending this crate. Start ADR 0004 Tool
  contract work in its Source manifest and generated artifacts.
- For maintenance of a current built-in before migration, update params/results,
  descriptor metadata, native handlers, projection tests, and API mapping as one
  temporary-path change.
- Add regression tests when descriptor schemas, capability requirements, or
  method names/base-authority requirements change.
- Keep scaffold additions behind `inspect-scaffold` and outside the current
  production catalog.

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
- Production catalog changes: assert catalog membership, descriptor schema,
  base-authority requirements, and tool projection behavior.
