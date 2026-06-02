# LANCET runtime-enforced surface

LATTICE agents execute tools through **LANCET**, the controlled execution and failure-learning subsystem. LANCET uses **CEP** (Controlled Execution Protocol) internally: JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Public surface (v1.3.1-alpha)

The documented public LANCET contract is split across **`lancet-protocol`** and
**`lancet-catalog`**:

| Component | Description |
|-----------|-------------|
| `lancet-catalog::CATALOG` | The 9-method canonical CEP catalog: `observe.workspace`, `mutate.create`, `mutate.patch`, `exec.command_run`, `fetch.web`, `session.todo_write`, `session.envelope`, `observe.audit`, `observe.audit_replay` |
| Method params/results | Typed `*Params` / `*Result` structs per catalog method in `lancet-catalog` |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` in `lancet-protocol` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types in `lancet-protocol` |
| `PolicyHook` / `PolicyDecision` | Pluggable policy hook interface in `lancet-protocol` |
| Audit/provenance schemas | `ProvGraphFragment` wire shapes in `lancet-protocol`; audit rows, filters, and attestation persistence in `lancet-audit` |

Internal implementation is split across `lancet-authority` (default policy, Guard L1, active envelope, effect-region semantics), `lancet-invariants` (invariant cards and Guard L2), and `lancet-native` (production handlers, transport lifecycle, verify/edit/shell/web integration).

**Not public surface:** `lancet-native` handler internals, private low-level executor helpers, authority/invariant implementation details, `lattice-tooling` shared types, and any `#[doc(hidden)]` re-exports. These are implementation details of the native LANCET server.

Pre-v1.3.1 method names are removed. They are not accepted by the adapter or
native registry, and LLM-facing tool definitions are projected only from the 9
canonical catalog methods.

### Effect-contract tool surface

The canonical method names are grouped by primary effect class:

| Method | Primary effect contract |
|--------|-------------------------|
| `observe.workspace` | Workspace observation. `mode=read_file` has single-path scope; `mode=grep`, `mode=list_directory`, and `mode=find_files` use root/directory observation scope. |
| `mutate.create` | Workspace mutation by pure file creation. Existing targets are rejected; missing parent directories are rejected. |
| `mutate.patch` | Workspace mutation of existing files through hashline-anchored patch hunks. |
| `exec.command_run` | Command execution, plus optional observed workspace mutation diff when `effective_impact = workspace_mutation`. |
| `fetch.web` | External observation through URL fetch policy. |
| `session.todo_write` | Session-state mutation for todo state. |
| `session.envelope` | Session-state mutation for explicit envelope declaration or derived intent. |
| `observe.audit` | Audit observation for query/tail mode. |
| `observe.audit_replay` | Audit observation for turn replay and attestation projection. |

### Release gate and self-check

- **Offline release gate:** `scripts/release-gate-v1.3.sh` — runs `cargo fmt`, `cargo clippy`, targeted crate tests, dogfood suite, LANCET boundary check, and `lattice lancet doctor`. Exits 0 on a clean tree.
- **LANCET self-check:** `cargo run -p lattice-cli -- lancet doctor` — verifies the production registry fully registers the CATALOG, in-process transport smoke-tests, base envelope constructs, and mode policy parses cleanly.
- **Warn-mode report:** `docs/lancet-warn-report.md` — coverage report for the canonical effect-contract surface; the baseline is N=0 leaks and no `fix-now` triage rows.
- **Capability boundary walkthrough:** `docs/v1.3-alpha-walkthrough.md` — explains what Layer-1 e2e proves and what is deferred to beta.

If a checkout still has pre-cutover invariant cards, migrate them with `mv .cep .lancet`; new cards live under `.lancet/invariants/`.

### Mode semantics

`mode = "off"` under `[lancet]` skips the effect-region L2 check only. The CEP policy hook still runs; capability declarations are still required, and effect-kind denial remains a hard fail. Legacy direct tool calls are not re-enabled — there is no legacy tool executor path in the production stack.

Effect-kind denial is enforced at checker time, after runtime effects are
observed. Configure it as `deny = ["workspace_mutation"]` under
`[lancet.effects]` or `[effects]` in an external policy file. This is stronger
than disabling a method prefix: a command whose primary method is
`exec.command_run` can still produce a `WorkspaceMutation` side effect, and that
side effect is caught only by effect-kind denial.

## Mutation and command observation

### Hashline patches

`mutate.patch` requires hashline anchors for context and removed lines:
`<prefix>blake3:<4 hex><TAB><line text>`, where `<prefix>` is either a space
for context or `-` for removal. The short anchor is the first 4 hex digits of
BLAKE3 over the before-state 1-based line number and line text. Added lines
remain plain `+text`.

This forces edits to be based on the current file state: a patch cannot remove
or anchor a line unless it supplies that line's current line anchor. Full-file
replacement is allowed when every removed line is hashline-anchored. Patch audit
metadata records changed files and line-level add/remove effects.

### Create-only writes

`mutate.create` is create-only. It fails if the target already exists and does
not create missing parent directories. Existing-file edits must go through
`mutate.patch`.

### Command opacity boundary

`exec.command_run` is a partial observation boundary, not a sound effect gate for
every dimension of the child process.

Observed:

- `CommandExecution` for every command invocation.
- `WorkspaceMutation` pre/post diff only when the policy classifies the command
  as `effective_impact = workspace_mutation`.

Unobserved:

- Network effects.
- Tree-out reads and writes.
- Workspace read set.
- Workspace writes for diagnostic commands, because diagnostic mode skips the
  mutation diff.

Audit metadata is derived from `effective_impact`:

- Workspace-mutation commands record `effect_observation = "partial"`,
  `observed_effects = ["command_execution", "workspace_mutation_diff"]`, and
  unobserved dimensions `network`, `tree_out_read`, `tree_out_write`, and
  `workspace_read_set`.
- Diagnostic commands record `effect_observation = "partial"`,
  `observed_effects = ["command_execution"]`, and additionally list
  `tree_in_write` as unobserved.

Command allowlists are admission control only. They constrain which executable
argv prefixes may run; they do not describe everything a permitted binary can do
after it starts. OS-level sandboxing remains future work.

## Provenance Audit

### Flat Store, Projected Graph

Audit rows live in SQLite (`cep_audit_entries` under `.lattice/cep_audit.db`). Each CEP call is one append-only row. **PROV-DM** structure (`Activity`, `Entity`, `Agent`, relations) is **not** stored as a graph; it is **projected** at read time into a `ProvGraphFragment` wire type (`observe.audit`, `observe.audit_replay`).

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

`observe.audit` and `observe.audit_replay` use
`ImpactKind::AuditObservation` and `Capability::ReadAudit`, granted on the turn
base envelope (session-scoped). They are excluded from default agent
enabled-tools; CLI and direct CEP clients opt into them explicitly.
