# AGENTS.md

Crate-local guidance for `lancet-audit`.

## Role

This crate owns CEP audit persistence, query filters, stage/impact labels,
typed operator query/tail/replay DTOs, project-keyed store-path resolution, and
the versioned Cell evidence tables. The evidence tables are the durable home of
`AdmissionEvidence`, `ObservedEffectsRecord`, and `DeniedOperationsRecord`.

## Sub-Architecture

- `src/audit.rs` owns `CepAuditStore`, query filters, row model, semantic-stage
  labels, impact labels, call metadata, and Cell-evidence read/write entry
  points.
- `src/evidence.rs` owns `evidence_calls`, `evidence_effects`, and
  `evidence_denials`; current `EvidenceId` is the durable SQLite lookup key for
  those three records and is never an `audit_id`. It is not ADR 0005's public
  invocation identity and is deleted with the SQLite path in #139-B. Record
  versions are validated on read.
- `src/operator.rs` owns `AuditRequest`, `AuditQueryOutput`,
  `AuditTailOutput`, `AuditReplayRequest`, `AuditReplayResult`,
  `OperatorAuditRow`, `OperatorCellEvidence`, the tail limits, and async
  `query_audit` / `tail_audit` / `replay_audit`.
- `src/operator_tests.rs` pins pagination, finalized-row exclusion, native /
  Component / mixed evidence behavior, pointerless Component visibility,
  unbounded turn replay, fixed JSON DTO fixtures, corrupt evidence failures,
  concurrency, and legacy-schema compatibility.
- `src/store_path.rs` owns external audit-path resolution.
- `src/test_support.rs` (feature `test-support`) owns the workspace-wide
  `ScopedXdgStateHome` test guard.
- `src/audit_tests.rs` and `src/evidence_tests.rs` cover store persistence and
  evidence schema behavior.

## Boundaries

- Session graph state belongs in `lattice-session` (git-backed), not SQLite.
- Do not make live authority, policy, or effect decisions here.
- Do not execute native handlers or inspect workspace contents directly.
- Never add a `lancet-native`, `lancet-dispatch`, `lancet-cep`, or
  `lancet-runtime` dependency. Their absence makes "operator audit reads never
  compose a CEP server" structural: server, transport, and registry types are
  not nameable in this crate.
- Operator DTOs are operator-facing, never Tool schemas, Source Tools,
  generated catalog entries, or Agent catalog methods.
- `metadata_json` is an internal pointer/query-key carrier. It must not cross
  an operator DTO boundary. Missing evidence is valid only for a row with no
  pointer; a malformed, dangling, unsupported, corrupt, or mismatched pointer
  is a read error.
- Every operator read observes finalized rows only and preserves `audit_id`
  order. Query is capped; replay reads the complete selected turn; tail cursors
  are exclusive. Bounded recent-call reads apply descending order and limit in
  SQLite before row materialization.
- Turn/audit provenance projection and turn attestation storage were retired by
  GitHub #114. Do not recreate them. Artifact identity and bundle provenance
  are separate contracts and remain intact.
- `audit_id`, numeric `call_id`, and `evidence_id` are transitional SQLite
  storage values, not ADR 0005 invocation identity. #139-A adds a real but
  non-production Git InvocationStore using the #142 typed InvocationId; the
  existing #114 SQLite operator Adapter remains authoritative until #139-B
  atomically switches it and deletes the SQLite identity/path. Never dual-write,
  import legacy rows, or expose two production Adapters.
- `lattice-store` supplies only generic Git mechanics. Reservation,
  DispatchStarted, terminal Cell evidence, query/tail/replay semantics, and the
  single append-only invocation ledger remain owned here. Operator reads must
  stay usable without composing CEP/runtime.

## Recent Issues

- ADDED #30/#31: evidence tables gained visibility and mutation facts through
  additive, versioned schema upgrades.
- ADDED #112: versioned Cell evidence became the durable record for Component
  calls; `metadata_json` keeps only an `evidence_id` pointer and query keys.
- FIXED #39: operator reads moved here from `lancet-api`, with a thin facade
  preserving public paths and error text.
- FIXED #114: replaced provenance fragments and in-toto attestations with a
  full finalized-row adapter. Any row without a pointer uses `evidence: null`;
  a readable pointer attaches typed Cell evidence. Old `attestation_records`
  is dropped when the store opens.

## Verification

- Local: `cargo test -p lancet-audit`.
- Schema changes: include persistence, query, tail, replay, and evidence-error
  coverage.
- Public-read changes: also run `cargo test -p lancet-api`, the Direct-Local
  `direct_operator` target, and the feature-gated `release_operator` scenario.
