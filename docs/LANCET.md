# LANCET runtime-enforced surface

LATTICE agents execute tools through **CEP** (Controlled Execution Protocol): JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Public surface (v1.3-alpha)

The documented public contract for LANCET is the surface exported from **`cep-protocol`**:

| Component | Description |
|-----------|-------------|
| `CATALOG` | The 14-method CEP catalog: `workspace.read_file`, `workspace.grep`, `workspace.list_directory`, `workspace.find_files`, `workspace.write_file`, `workspace.patch`, `web.fetch`, `session.todo_write`, `command.run`, `envelope.declare`, `envelope.submit_intent`, `audit.query`, `audit.tail`, `audit.replay` |
| Method params/results | Typed `*Params` / `*Result` structs per catalog method |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types |
| `PolicyHook` / `PolicyDecision` | Pluggable policy hook interface |
| Audit/provenance schemas | `ProvGraphFragment`, `AttestationRecord` wire shapes |

**Not public surface:** `cep-executor` handler internals, private low-level executor helpers, `lattice-tooling` shared types, and any `#[doc(hidden)]` re-exports. These are implementation details of the native CEP server.

### Release gate and self-check

- **Offline release gate:** `scripts/release-gate-v1.3.sh` — runs `cargo fmt`, `cargo clippy`, targeted crate tests, dogfood suite, CEP boundary check, and `lattice lancet doctor`. Exits 0 on a clean tree.
- **CEP self-check:** `cargo run -p lattice-cli -- lancet doctor` — verifies the production registry fully registers the CATALOG, in-process transport smoke-tests, base envelope constructs, and policy/`cep_mode` parses cleanly.
- **Warn-mode report:** `docs/cep-warn-report.md` — DOG-02 coverage report for all 14 catalog methods; the v1.3-alpha baseline is N=0 leaks and no `fix-now` triage rows.
- **Capability boundary walkthrough:** `docs/v1.3-alpha-walkthrough.md` — explains what Layer-1 e2e proves and what is deferred to beta.

### cep_mode semantics

`cep_mode = "off"` skips the effect-region L2 check only. The CEP policy hook still runs; capability declarations are still required. Legacy direct tool calls are not re-enabled — there is no legacy tool executor path in the production stack.

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
