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

- Session graph state belongs in `lattice-session` (git-backed, not SQLite), not here.
- Do not make live authority, policy, or effect decisions in this crate.
- Do not perform native execution or inspect workspace contents directly.
- Keep canonicalization deterministic; attestation hashes depend on it.

## Recent Issues

- ADDED #28: `PRAGMA user_version` carries `AUDIT_SCHEMA_VERSION`; a newer or
  old-shape database fails open with `AuditError::IncompatibleSchema` and is left
  untouched (read-only legacy import stays the migration path). Shape detection
  covers both written tables by column name (`cep_audit_entries` and
  `attestation_records`); matching names with different column types are still
  adopted. `merge_finalize_metadata` takes `ComponentCallEvidence` and writes
  `admission_evidence` / `observed_effects` / `denied_operations` separately;
  `component_cells` (4-segment `ctx:ext:scope:trust`, spec §6.3) is derived from
  the effect ledger and `observed_capabilities` is label-ordered.
  `build_in_toto_statement` picks v1/v2 per turn from row evidence,
  `rebuild_in_toto_statement` gives a stored `predicate_type` the last word, and
  `statement_bytes` canonicalizes v2 only — stored v1 bytes are never rewritten.
- ADDED #28 (labels): Component calls have their own vocabulary —
  `CallAuditMetadata::for_component_dispatch` (`cell_component` /
  `cell_admission`, policy-enforced) plus `SEMANTIC_STATUS_COMPONENT` and
  `SEMANTIC_STATUS_COMPONENT_DENIED`, which the `success`/`denied` verdict
  filters both name. The legacy catalog lookup must never label a Component
  call. A Cell-governed row has no legacy `ImpactKind`, so the projector marks
  its `ProvActivity::impact_kind` `impact_kind_attributed: false` instead of
  coercing the `unknown` column into a concrete kind.
- FIXED #38: `RejectionStage` vocabulary de-Uppered — `AdapterPreflight` →
  `PathGuard`, `BackendExecution` → `LocalIo`, `VerificationFailure` removed;
  `CallAuditMetadata.backend_invoked` and all routing metadata
  (`execution_backend`, `upper_workspace_id`, `adapter_preflight_invoked`,
  `backend_invoked`) are gone from durable audit rows. Mutation preflight
  evidence renamed `expected_hash_status` → `local_revision` (carries the
  observed Local Revision on stale-hash replans). Historical rows written
  before #38 keep the old labels; alpha-stage no-compat means no label
  migration — `rejection_stage` query filters match new writes only.
- FIXED issue #11: agent catalog audit CEP methods removed; operator reads live in
  `lancet-api::audit` and `lattice audit` (direct store, no CEP server).
- OPEN #17: operator read DTOs, pagination/filter behavior, and contract tests
  move into this crate while `lancet-api` remains a thin facade. They must not
  become Source Tools or generated catalog entries.
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
- Coordinate new stage/impact/Cell labels with the current `lancet-native` or
  target `lancet-runtime` lifecycle and `lattice-cli` audit output.

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
- Provenance/attestation changes: also run the active runtime provenance tests
  (`lancet-native` during migration, then `lancet-runtime`) and `lattice-api`
  audit turn provenance tests.
- Schema changes should include persistence, query, and replay coverage.
