# AGENTS.md

Crate-local guidance for `lancet-catalog`.

## Role

This crate is transitional ADR 0004 machinery for generic CEP method traits and
dev-only scaffold fixtures. Its production `catalog()` is empty after GitHub #32.
All five built-in Tool contracts now come only from Source Tool manifests and
generated descriptors.

## Sub-architecture

- `src/method.rs` defines the residual generic `CepMethod`, `CatalogMethod`,
  `MethodDescriptor`, and legacy authority/effect vocabulary.
- `src/catalog.rs` returns the empty production catalog. Optional inspection
  scaffold methods remain behind `inspect-scaffold`.
- `src/test_method.rs` provides the dev-only `TestEcho` fixture. It never enters
  the production catalog.
- `src/methods/` contains only generic module wiring and feature-gated scaffold
  support. Built-in workspace, session, and external contract modules are gone.

## Boundaries

- Do not add production Tools here. Start in `tools/<tool>/manifest.toml` and run
  `cargo xtask tool-codegen`.
- Do not restore built-in constants, schemas, descriptors, native projection
  shims, or compatibility reads. The manifest is the sole contract authority.
- Depend only on protocol plus serialization/schema support.
- Keep operator audit contracts in `lancet-audit`; operator audit is not a Tool.
- Keep provider name encoding in `lattice-llm`.
- Keep scaffold methods feature-gated and outside `catalog()`.
- The empty catalog is intentional migration state, not an invitation to remove
  collision checks or generic legacy tests before ADR 0004 removes their users.

## Verification

- Local: `cargo test -p lancet-catalog`.
- Catalog consumers: `cargo test -p lancet-cep -p lancet-authority`.
- Boundary: `cargo xtask lancet-boundary`.

## Record maintenance

Update this file when generic catalog machinery is removed or its remaining
consumers change. Record landed fixes in root `FIX.md`; keep only open residuals
in `FINDINGS.md`.
