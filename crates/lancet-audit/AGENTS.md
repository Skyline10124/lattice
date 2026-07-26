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

- ADDED #64: `rewind_records` table plus `RewindAuditRecord`,
  `insert_rewind_record`, and `rewind_records_for_session`. One durable row
  per explicit Rewind attempt — success, conflict, restore failure, or ref
  failure — carrying the target Turn, both history identities, the reversed
  steps, per-path preflight revisions, restored paths, and conflicts.
  Content policy is unchanged: paths, hashes, and object ids only, never
  file bytes. `init_schema` creates the table with `IF NOT EXISTS`, so a
  pre-#64 database gains it on first open (covered by a compatibility test).
- ADDED #5 slice D: `merge_finalize_metadata` gained a `component_cells: &[String]`
  parameter. Component Tool calls record 4-segment Cell audit keys
  (`ctx:ext:scope:trust`) as a `component_cells` JSON array; native handler
  calls pass an empty slice (no behavior change). Audit records cells, not
  capability names (spec §6.3).
- FIXED #38: `RejectionStage` vocabulary de-Uppered — `AdapterPreflight` →
  `PathGuard`, `BackendExecution` → `LocalIo`, `VerificationFailure` removed;
  `CallAuditMetadata.backend_invoked` and all routing metadata
  (`execution_backend`, `upper_workspace_id`, `adapter_preflight_invoked`,
  `backend_invoked`) are gone from durable audit rows. Mutation preflight
  evidence renamed `expected_hash_status` → `local_revision` (carries the
  observed Local Revision on stale-hash replans). Historical rows written
  before #38 keep the old labels; alpha-stage no-compat means no label
  migration — `rejection_stage` query filters match new writes only.
- FIXED M-24-05: attestation persistence wiring moved into `lancet-api`;
  `lattice-api` consumes it through the public LANCET runtime surface.
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
