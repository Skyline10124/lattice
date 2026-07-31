# AGENTS.md

Crate-local guidance for `lancet-audit`.

## Role

This crate owns CEP audit persistence and provenance projection: call audit
rows, query filters, stage labels, impact/capability labels, canonicalization,
in-toto attestation construction, and attestation persistence. Since GitHub #39 it also
owns the operator audit read path end to end — DTOs, pagination, filter
construction, wire projection, attestation comparison, and the project-keyed
store-path resolution behind it.

## Sub-Architecture

- `src/audit.rs` owns `CepAuditStore`, query filters, row model, semantic stage
  labels, impact labels, and call metadata, plus the evidence read/write methods
  (`record_call_evidence`, `call_evidence`, `call_evidence_for_turn`,
  `call_evidence_for_call`).
- `src/evidence.rs` (#112) owns the versioned Cell evidence tables —
  `evidence_calls` / `evidence_effects` / `evidence_denials` — `EvidenceId`
  (the stable per-call identity, **not** `audit_id`), `EvidenceOutcome`,
  `CallEvidence` / `StoredCallEvidence`, and `EvidenceError`. Cells are stored
  decomposed by column so the surface is queryable; `cell_scope` and `scope` are
  separate columns because `Cell::scope` and `ObservedScope::scope` are two
  protocol fields. Each record's `version` is validated on read
  (`UnsupportedRecordVersion`), and a corrupt enum label is reported
  (`UnreadableField`) rather than coerced. `src/evidence_tests.rs` holds the
  contract tests.
- `src/operator.rs` owns the operator read API: `AuditRequest`,
  `AuditQueryOutput`, `AuditTailOutput`, `AuditReplayRequest`,
  `AuditReplayResult`, `AUDIT_TAIL_MAX_ROWS` / `AUDIT_TAIL_MAX_BATCHES`,
  `OperatorAuditError`, and async `query_audit` / `tail_audit` / `replay_audit`.
  Its module doc carries the pagination, cancellation (L-11-01), replay-output,
  and concurrency (L-11-02) contracts.
- `src/operator_tests.rs` holds the operator contract tests (pagination, replay,
  attestation match, unfinalized-row exclusion, concurrent writer, legacy
  schema).
- `src/store_path.rs` owns `ExternalAuditPathError`, `audit_state_root`,
  `project_storage_key`, `audit_db_path_for_project`, and
  `audit_db_path_for_project_in`.
- `src/test_support.rs` (feature `test-support`) owns the single workspace-wide
  `ScopedXdgStateHome`; `lattice-api` and `lattice-cli` consume it as a
  dev-dependency instead of keeping crate-local copies.
- `src/provenance/projector.rs` projects audit records into PROV fragments.
- `src/provenance/attestation.rs` builds and stores turn attestation records.
- `src/provenance/canonicalize.rs` owns deterministic JSON canonicalization.
- `src/provenance/mod.rs` exports provenance helpers used by native/API tests.
- `src/audit_tests.rs` covers persistence and query behavior.
- SQLite used here is only for CEP audit and Cell evidence data.

## Boundaries

- Session graph state belongs in `lattice-session` (git-backed, not SQLite), not here.
- Do not make live authority, policy, or effect decisions in this crate.
- Do not perform native execution or inspect workspace contents directly.
- Keep canonicalization deterministic; attestation hashes depend on it.
- Never add a `lancet-native`, `lancet-dispatch`, `lancet-cep`, or
  `lancet-runtime` dependency. That absence is what makes "operator audit read
  never composes a CEP server" a structural crate-dependency guarantee instead
  of a source-string scan: server, transport, and registry types are simply not
  nameable here. Adding one silently reopens the door.
- Operator read DTOs are operator-facing, not Tool schema types. They must never
  gain catalog/schema derives, become Source Tools or generated catalog
  entries, or reappear as Agent catalog methods.

## Recent Issues

- ADDED #30: `evidence_effects` gained a `truncated INTEGER` column (nullable
  three-state: absent = unbounded observation, 0 = bounded but untruncated,
  1 = truncated — the host computes it, the guest cannot forge it).
  `EVIDENCE_SCHEMA_VERSION` moved 2 → 3
  (`EVIDENCE_SCHEMA_VERSION_PRE_TRUNCATION` names the old value). The column
  is **not** part of the shape guard that refuses mismatched files: a v2 file
  is adopted additively via `ALTER TABLE evidence_effects ADD COLUMN`, with a
  post-migration assertion that the column landed.
- ADDED #112: the versioned Cell evidence tables. `AUDIT_SCHEMA_VERSION` moved
  1 → 2 (`AUDIT_SCHEMA_VERSION_PRE_EVIDENCE` names the old value); a v1 file is
  adopted additively, and a file already claiming v2 whose evidence table shape
  disagrees is refused with an operator action and left untouched. Denials and
  failures land in the table too, labeled by `EvidenceOutcome`.
  `merge_finalize_metadata` no longer writes `admission_evidence` /
  `observed_effects` / `denied_operations` into `metadata_json` — it writes an
  `evidence_id` pointer plus the derived `component_cells` query key. PROV
  projection and attestation take an `EvidenceLookup` and resolve the records
  from the table (`project_rows_with_evidence`,
  `build_in_toto_statement_with_evidence`,
  `rebuild_in_toto_statement_with_evidence`); the no-lookup entry points remain
  for native-only rows, which never had Cell evidence.

- ADDED #28 (Codeberg #6): `PRAGMA user_version` carries `AUDIT_SCHEMA_VERSION`; a newer or
  old-shape database fails open with `AuditError::IncompatibleSchema` and is left
  untouched (read-only legacy import stays the migration path). Shape detection
  covers both written tables by column name (`cep_audit_entries` and
  `attestation_records`); matching names with different column types are still
  adopted. `REQUIRED_AUDIT_COLUMNS` naming the legacy `impact_kind` column is
  the one recorded exception to #28 §11's "no new consumer of the legacy
  vocabulary": adopting an unversioned legacy DB must name that DB's columns.
  It parses no `ImpactKind` value; the constant moves with the column.
  `merge_finalize_metadata` takes `ComponentCallEvidence` and writes
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
- FIXED GitHub #39 (= Codeberg #17): operator read DTOs, pagination/filter construction,
  wire projection, attestation comparison, contract tests, and the store-path
  helpers landed here as `src/operator.rs` / `src/operator_tests.rs` /
  `src/store_path.rs`; `lancet-api::audit` is now a thin delegating facade with
  byte-identical error messages, and `lancet-catalog/src/methods/audit.rs` with
  its six DTOs was deleted outright. These types are operator-only: never Source
  Tools, generated catalog entries, or Agent catalog methods. The same issue
  moved `ScopedXdgStateHome` here behind `test-support`, deleting the
  `lattice-api` / `lattice-cli` copies. New deps: `dirs`, `sha2`, optional
  `tempfile`. Residuals L-11-01 (tail cancellation) and L-11-02
  (synthetic-writer concurrency) are now crate-local — contracts in
  `src/operator.rs`, escalation conditions in root `FINDINGS.md`.
- ADDED #27 (Codeberg #5) slice D: `merge_finalize_metadata` gained a
  `component_cells: &[String]` parameter. Component Tool calls record
  4-segment Cell audit keys (`ctx:ext:scope:trust`) as a `component_cells`
  JSON array; native handler calls pass an empty slice (no behavior change).
  Audit records cells, not capability names (spec §6.3).
- FIXED #60 (Codeberg #38): `RejectionStage` vocabulary de-Uppered —
  `AdapterPreflight` → `PathGuard`, `BackendExecution` → `LocalIo`,
  `VerificationFailure` removed; `CallAuditMetadata.backend_invoked` and all
  routing metadata (`execution_backend`, `upper_workspace_id`,
  `adapter_preflight_invoked`, `backend_invoked`) are gone from durable audit
  rows. Mutation preflight evidence renamed `expected_hash_status` →
  `local_revision` (carries the observed Local Revision on stale-hash
  replans). Historical rows written before #60 keep the old labels;
  alpha-stage no-compat means no label migration — `rejection_stage` query
  filters match new writes only.
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
- FIXED issue #33 (Codeberg #11): agent catalog audit CEP methods removed;
  operator reads are reached through `lancet-api::audit` and `lattice audit`
  (direct store, no CEP server) and, since #39, implemented here in
  `src/operator.rs`.
- WATCH: provenance isomorphism tests in `lancet-native` and audit turn tests in
  `lattice-api` are the practical consumers of this crate's contract.
- WATCH: call stage and impact labels are shared language between native
  lifecycle, audit queries, CLI rendering, and provenance.
- No open crate-local audit *storage* finding is listed in `FINDINGS.md`.

## Next Actions

- Keep audit storage, provenance projection, attestation serialization, and the
  operator read path here; runtime orchestration belongs in `lancet-api`. Do not
  push pagination, filter parsing, wire projection, or attestation comparison
  back up into the facade.
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

- Local: `cargo test -p lancet-audit` (add `--features test-support` when the
  change touches `src/test_support.rs` or its downstream consumers).
- Provenance/attestation changes: also run the active runtime provenance tests
  (`lancet-native` during migration, then `lancet-runtime`) and `lattice-api`
  audit turn provenance tests.
- Schema changes should include persistence, query, and replay coverage.
