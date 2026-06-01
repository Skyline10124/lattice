# LANCET runtime-enforced surface

LATTICE agents execute tools through **LANCET**, the controlled execution and failure-learning subsystem. LANCET uses **CEP** (Controlled Execution Protocol) internally: JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Public surface (v1.3-alpha)

The documented public LANCET contract is split across **`lancet-protocol`** and
**`lancet-catalog`**:

| Component | Description |
|-----------|-------------|
| `lancet-catalog::CATALOG` | The 14-method CEP catalog: `workspace.read_file`, `workspace.grep`, `workspace.list_directory`, `workspace.find_files`, `workspace.write_file`, `workspace.patch`, `web.fetch`, `session.todo_write`, `command.run`, `envelope.declare`, `envelope.submit_intent`, `audit.query`, `audit.tail`, `audit.replay` |
| Method params/results | Typed `*Params` / `*Result` structs per catalog method in `lancet-catalog` |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` in `lancet-protocol` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types in `lancet-protocol` |
| `PolicyHook` / `PolicyDecision` | Pluggable policy hook interface in `lancet-protocol` |
| Audit/provenance schemas | `ProvGraphFragment` wire shapes in `lancet-protocol`; audit rows, filters, and attestation persistence in `lancet-audit` |

Internal implementation is split across `lancet-authority` (default policy, Guard L1, active envelope, effect-region semantics), `lancet-invariants` (invariant cards and Guard L2), and `lancet-native` (production handlers, transport lifecycle, verify/edit/shell/web integration).

**Not public surface:** `lancet-native` handler internals, private low-level executor helpers, authority/invariant implementation details, `lattice-tooling` shared types, and any `#[doc(hidden)]` re-exports. These are implementation details of the native LANCET server.

### Release gate and self-check

- **Offline release gate:** `scripts/release-gate-v1.3.sh` — runs `cargo fmt`, `cargo clippy`, targeted crate tests, dogfood suite, LANCET boundary check, and `lattice lancet doctor`. Exits 0 on a clean tree.
- **LANCET self-check:** `cargo run -p lattice-cli -- lancet doctor` — verifies the production registry fully registers the CATALOG, in-process transport smoke-tests, base envelope constructs, and mode policy parses cleanly.
- **Warn-mode report:** `docs/lancet-warn-report.md` — coverage report for all 14 catalog methods; the v1.3-alpha baseline is N=0 leaks and no `fix-now` triage rows.
- **Capability boundary walkthrough:** `docs/v1.3-alpha-walkthrough.md` — explains what Layer-1 e2e proves and what is deferred to beta.

If a checkout still has pre-cutover invariant cards, migrate them with `mv .cep .lancet`; new cards live under `.lancet/invariants/`.

### Mode semantics

`mode = "off"` under `[lancet]` skips the effect-region L2 check only. The CEP policy hook still runs; capability declarations are still required. Legacy direct tool calls are not re-enabled — there is no legacy tool executor path in the production stack.

## Provenance Audit

### Flat Store, Projected Graph

Audit rows live in SQLite (`cep_audit_entries` under `.lattice/cep_audit.db`). Each CEP call is one append-only row. **PROV-DM** structure (`Activity`, `Entity`, `Agent`, relations) is **not** stored as a graph; it is **projected** at read time into a `ProvGraphFragment` wire type (`audit.query`, `audit.replay`).

### Agent Triple

Rows carry `session_id`, `turn_id`, and nullable `model_id` / `runtime_version` columns populated from `CepAuditContext` at `start_call` so replay can reconstruct the Agent node from audit alone.

### Metadata Seeds

Handler and transport layers write compact `metadata_json` keys such as paths,
`content_hash`, and `envelope_id`. These fields are intentionally summary-only:
full request and response bodies are not stored in audit metadata.

Transport finalize merges catalog `required_capability` and effect-ledger `observed_capabilities` without changing the audit write lifecycle.

### Attestations

Per-turn **unsigned in-toto** statements are stored in `attestation_records` in the same audit database (not git-tracked files). Signing and key management are deferred beyond alpha; wire shapes remain forward-compatible.

### Time-Range Proxy

Monotonic `audit_id` range (`since_audit_id` / `until_audit_id`) is the
primary time-order proxy when wall-clock `started_at` bounds are omitted.
Optional `started_after` / `started_before` filter on the millis `started_at`
column.

### Read-only audit methods

`audit.query`, `audit.tail`, and `audit.replay` use
`ImpactKind::AuditObservation` and `Capability::ReadAudit`, granted on the turn
base envelope (session-scoped). They are excluded from default agent
enabled-tools; CLI and direct CEP clients opt into them explicitly.
