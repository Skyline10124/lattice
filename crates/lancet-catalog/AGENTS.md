# AGENTS.md

Crate-local guidance for `lancet-catalog`.

## Role

This crate owns the current typed CEP method catalog used by native handlers and
tool projection. It is a migration authority, not the ADR 0004 target: Source
Tool manifests and generated descriptors replace it after Component migration,
and #34 (Codeberg #12) removes the remaining central catalog surface.

## Sub-Architecture

- `src/method.rs` defines `CepMethod`, `CatalogMethod`, `MethodDescriptor`, and
  `BaseAuthorityRequirement`.
- `src/catalog.rs` builds the production `CATALOG` and optional scaffold
  catalog behind `inspect-scaffold`.
- `src/methods/workspace.rs` owns the mutate workspace contracts.
  `observe.workspace` left in #30 — it is an embedded Component Tool whose
  contract authority is `tools/observe-workspace/manifest.toml`.
- `src/methods/external.rs` owns `fetch.web` contracts.
- `src/methods/session.rs` owns session state helper contracts.
- `src/methods/scaffold.rs` is feature-gated inspection scaffolding only.
- There is no operator audit module here. `methods/audit.rs` was deleted in GitHub #39;
  operator audit DTOs live in `lancet-audit::operator`.

## Boundaries

- Depend only on protocol plus serialization/schema support.
- `CATALOG` remains authoritative for the current production path only. Do not
  create a third schema beside it and the ADR 0004 Source Tool manifest work.
- `CATALOG` is the **native** method set, three entries since #30 migrated
  `observe.workspace` to an embedded Component Tool (#29 did the same for
  `session.todo_write`). After #31 the native catalog is `fetch.web` only; the shipped distribution is still five Tools; the
  migrated two's descriptor authority is their `tools/*/manifest.toml`, and
  re-adding them here would be the parallel schema ADR 0004 forbids (and
  would arm both authorization vocabularies for one call name — #28 design
  §11). Do not treat the set as a permanent global limit; closed runtime
  composition belongs to generated registrations in the target
  architecture.
- `src/test_method.rs` (`test-method` feature, dev-only) holds `TestEcho`, the
  synthetic typed method downstream tests register when they need any
  `CepMethod` shape. It implements `CatalogMethod` (sealing lives here, so the
  impl is legal only in this crate) but is deliberately absent from `catalog()`.
  `session.todo_write` used to double as that fixture; #29 removed the type.
- Do not define LLM-facing tool schemas here; `lancet-cep` projects them from
  descriptors and `lancet-native` exposes the native path.
- Keep method params/results, descriptors, and capabilities in sync.
- Keep base authority requirements method-local and policy-neutral; catalog may
  say that `fetch.web` needs fetch authority, but not which hosts are allowed.
- Do not add operator-only contracts here. #39 moved the audit DTOs to
  `lancet-audit`; operator audit is not a Tool and does not come back.

## Recent Issues

- FIXED #39: `src/methods/audit.rs` and its `methods/mod.rs` wiring were
  deleted, removing all six operator audit DTOs (`AuditQueryParams`,
  `AuditQueryResult`, `AuditTailParams`, `AuditTailResult`, `AuditReplayParams`,
  `AuditReplayResult`) from the central migration surface. None became a Tool
  descriptor, generated inventory entry, Agent catalog method, or Component —
  operator audit now owns its own DTOs in `lancet-audit`. This shrinks what #34
  has to delete when the central catalog authority goes away.
- FIXED P19: catalog sealing compile-fail coverage was corrected during CEP
  core review.
- FIXED M-24-04: `MethodDescriptor::base_authority` now declares method-local
  base authority requirements consumed by `lancet-cep::ToolExposure`; policy
  inputs such as allowed hosts stay outside catalog.
- FIXED issue #33 (Codeberg #11): production `CATALOG` is 5 agent methods;
  audit reads are operator-only and were never registered in `CATALOG` (their
  DTOs left this crate in #39). (The count has since shrunk with migration:
  4 after #29, 3 after #30 — Component Tools carry their own descriptors.)
- WATCH: dotted method names stay canonical in CEP catalog; provider-specific
  name encoding belongs in `lattice-llm` wire adapters.
- WATCH: optional scaffold methods must remain feature-gated and outside the
  production native catalog.

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
