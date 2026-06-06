# AGENTS.md

Crate-local guidance for `lancet-audit`.

## Role

This crate owns CEP audit persistence and provenance projection: call audit
rows, query filters, stage labels, impact/capability labels, canonicalization,
in-toto attestation construction, and attestation persistence.

## Sub-Architecture

- `src/audit.rs` owns `CepAuditStore`, query filters, row model, semantic stage
  labels, impact labels, and call metadata.
- `src/provenance/projector.rs` projects audit records into PROV fragments.
- `src/provenance/attestation.rs` builds and stores turn attestation records.
- `src/provenance/canonicalize.rs` owns deterministic JSON canonicalization.
- `src/provenance/mod.rs` exports provenance helpers used by native/API tests.
- `src/audit_tests.rs` covers persistence and query behavior.
- SQLite used here is only for CEP audit data.

## Boundaries

- Session graph SQLite belongs in `lattice-session`, not here.
- Do not make live authority, policy, or effect decisions in this crate.
- Do not perform native execution or inspect workspace contents directly.
- Keep canonicalization deterministic; attestation hashes depend on it.

## Recent Issues

- FIXED M-24-05: attestation persistence wiring moved into `lancet-api`;
  `lattice-api` consumes it through the public LANCET runtime surface.
- WATCH: provenance isomorphism tests in `lancet-native` and audit turn tests in
  `lattice-api` are the practical consumers of this crate's contract.
- WATCH: call stage and impact labels are shared language between native
  lifecycle, audit queries, CLI rendering, and provenance.
- No open crate-local audit storage finding is currently listed in
  `FINDINGS.md`.

## Next Actions

- Keep audit storage, provenance projection, and attestation serialization here;
  runtime orchestration belongs in `lancet-api`.
- Add migration or compatibility tests before changing audit row shape or
  attestation serialization.
- Keep provenance projection and canonicalization changes paired with
  integration tests that compare stable statements/hashes.
- Coordinate new stage/impact labels with `lancet-native` lifecycle and
  `lattice-cli` audit output.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when audit schema, labels,
  provenance projection, or attestation behavior changes.
- Put unresolved audit/provenance risks in root `FINDINGS.md`.
- Put landed audit/provenance fixes in root `FIX.md` with verification results.
- If a cross-crate issue mainly affects wiring, mention it here only when it
  changes this crate's next edit path.
- Keep this file near 80 lines; prune stale closed notes after `FIX.md` records
  them.

## Verification

- Local: `cargo test -p lancet-audit`.
- Provenance/attestation changes: also run `lancet-native` provenance tests and
  `lattice-api` audit turn provenance tests.
- Schema changes should include persistence, query, and replay coverage.
