# LANCET runtime-enforced surface

LATTICE agents execute tools through **CEP** (Controlled Execution Protocol): JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Provenance audit (Phase 26)

### Flat store, projected graph (D-01)

Audit rows live in SQLite (`cep_audit_entries` under `.lattice/cep_audit.db`). Each CEP call is one append-only row. **PROV-DM** structure (`Activity`, `Entity`, `Agent`, relations) is **not** stored as a graph; it is **projected** at read time into a `ProvGraphFragment` wire type (`audit.query`, `audit.replay`).

### Agent triple (D-04)

Rows carry `session_id`, `turn_id`, and nullable `model_id` / `runtime_version` columns populated from `CepAuditContext` at `start_call` so replay can reconstruct the Agent node from audit alone.

### Metadata seeds (D-03)

Handler and transport layers write compact `metadata_json` keys (paths, `content_hash`, `envelope_id`, etc.). The per-method contract is documented in:

`.planning/phases/26-provenance-audit-hardening/26-METADATA-SEED-SPEC.md`

Transport finalize merges catalog `required_capability` and effect-ledger `observed_capabilities` without changing the audit write lifecycle.

### Attestations (D-09–D-12)

Per-turn **unsigned in-toto** statements are stored in `attestation_records` in the same audit database (not git-tracked files). Signing and key management are deferred beyond alpha; wire shapes remain forward-compatible.

### Time-range proxy (W3)

For PROV-02 filtering in alpha, monotonic `audit_id` range (`since_audit_id` / `until_audit_id`) is the primary time-order proxy when wall-clock `started_at` bounds are omitted. Optional `started_after` / `started_before` filter on the millis `started_at` column.

### Read-only audit methods

`audit.query`, `audit.tail`, and `audit.replay` use `ImpactKind::AuditObservation` and `Capability::ReadAudit`, granted on the turn base envelope (session-scoped). They are excluded from default agent enabled-tools; CLI and direct CEP clients consume them in later phase plans.
