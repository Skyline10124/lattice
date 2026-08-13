# LANCET runtime-enforced surface

> **TRANSITION NOTICE - 2026-07-16 (updated 2026-07-17):** This document mostly
> describes the current v1.4-alpha migration implementation, not the accepted
> target architecture. ADR 0003 replaces MSB/Upper with one guarded Direct-Local
> Workspace path - **landed in #59-#62 (Codeberg #37-#40)**: production turns
> now operate directly on the canonical Local Workspace, and the MSB/Upper
> path, `[workspace]` profile section, and `lattice-workspace` crate are
> deleted. ADR 0004 replaces the permanent native/central-catalog boundary
> with a per-build closed catalog of embedded WASM Component Tools whose
> Source manifests own their descriptors.
> The default distribution still has the five methods shown below, but operators
> may add verified precompiled Tool Bundles before building another closed
> runtime; running processes cannot load code. Components receive no ambient
> WASI filesystem, network, or process authority and can act only through Tool
> Host imports. Until #27-#36 land, production still uses the
> `lancet-catalog`/CEP/dispatch and native-handler path documented below. Treat
> those surfaces as migration residue, not extension points. Command execution,
> post-mutation verify, agent-visible audit, and SQLite session storage remain
> retired. Final line-by-line synchronization is tracked by #36.
>
> ADR 0005 additionally fixes the durable invocation target. Provider Tool Call
> IDs remain provider replay metadata; one typed InvocationId eventually spans
> transcript binding, CEP execution, Host context, history, terminal Cell
> evidence, and operator output. GitHub #141–#144 establish the architecture,
> data contracts, generic private-Git kernel, and PendingTurn journal. #139-A
> then builds a non-production InvocationStore; only #139-B switches the live
> path and deletes the transitional SQLite audit/evidence identity. There is no
> dual write, legacy reader/import, Standalone invocation, or direct CEP product
> caller in that target.
> The JSON-RPC request object remains during migration; its canonical string
> `id` becomes the already-reserved InvocationId in #139-B. This is unrelated to
> the deleted `session.envelope` Tool.

LATTICE agents execute tools through **LANCET**, the controlled execution and failure-learning subsystem. LANCET uses **CEP** (Controlled Execution Protocol) internally: JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Public surface (v1.4-alpha boundary contract)

The documented public LANCET contract is framed by **`lancet-cep`**, which
re-exports the CEP protocol/catalog/dispatch interface and owns tool exposure
plus base-authority planning. The underlying contract remains split across
**`lancet-protocol`** and **`lancet-catalog`**:

| Component | Description |
| ----------- | ------------- |
| `lancet-cep` | Public CEP facade: protocol/catalog/dispatch re-exports, catalog-derived `ToolDefinition`, `ToolExposure`, descriptor-declared base authority aggregation, and explicit `BaseAuthorityPlan` construction |
| `lancet-catalog::CATALOG` | The **native** CEP catalog plus descriptor metadata for methods that still use typed native handlers. Since GitHub #31 it is `fetch.web` only. The Agent-visible five-Tool distribution (`observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`, `session.todo_write`) is closed composition: four embedded Components plus this one native method. (`session.envelope`, `exec.command_run`, `mutate.batch`, post-mutation verify, and agent-visible audit CEP methods were removed - catalog and tests assert their absence.) |
| `lancet-api::audit` | Stable operator entry points for audit query/tail/replay over the project-keyed audit store (`query_audit`, `tail_audit`, `replay_audit`); a thin facade that delegates to `lancet-audit::operator`, which owns typed DTOs, pagination, filters, and Cell-evidence materialization. Not agent CEP methods. |
| Method params/results | Typed `*Params` / `*Result` structs per catalog method in `lancet-catalog` |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` in `lancet-protocol` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types in `lancet-protocol` |
| `PolicyHook` / `PolicyDecision` | Pluggable policy hook interface in `lancet-protocol` |
| Audit/evidence schemas | Versioned Cell-evidence shapes in `lancet-protocol`; audit rows, filters, typed operator read DTOs, and project-keyed store-path resolution in `lancet-audit` |

Internal implementation is split across `lancet-authority` (default policy, Guard L1, active envelope, effect-region semantics) and `lancet-native` (production handlers, transport lifecycle, edit/web integration).

**Not public surface:** `lancet-native` handler internals, private low-level executor helpers, authority implementation details, and any `#[doc(hidden)]` re-exports. These are implementation details of the native LANCET server.

Pre-v1.3.1 method names are removed. They are not accepted by the adapter or
native registry, and LLM-facing tool definitions are projected from the closed
five-Tool distribution (native catalog + embedded Component descriptors). Tool exposure is separate from base authority:
enabled methods filter the model-visible catalog projection, while
catalog descriptors declare method-local base authority requirements.
`ToolExposure::base_authority()` aggregates those requirements, and
`BaseAuthorityPlan` materializes them with runtime policy inputs such as allowed
fetch hosts. The base envelope never grants workspace write or command scopes.
Projection APIs live in `lancet-cep`; `lancet-native` and `lancet-api` do not
carry compatibility projection shims.

### Effect-contract tool surface

The canonical method names are grouped by primary effect class:

| Method | Primary effect contract |
| -------- | ------------------------- |
| `observe.workspace` | Workspace observation. `mode=read_file` has single-path scope; `mode=grep`, `mode=list_directory`, and `mode=find_files` use root/directory observation scope. |
| `mutate.create` | Workspace mutation by pure file creation (embedded Component Tool since #31). Existing targets are rejected; missing parent directories are rejected. |
| `mutate.edit` | Workspace mutation of existing files through semantic replace/insert operations (embedded Component Tool since #31). The caller provides `expected_hash`; LANCET computes hashline anchors internally. |
| `fetch.web` | External observation through URL fetch policy. |
| `session.todo_write` | Session-state mutation for todo state. |

Audit query, tail, and turn replay are **not** catalog methods. Operators use
`lattice audit` (default: query/tail; `lattice audit replay --turn <id>` for a
turn's finalized call trace), backed by `lancet-api::{query_audit, tail_audit,
replay_audit}`, which delegate to `lancet-audit::operator`; see **Operator
Audit** below. The operator DTOs never lived in `CATALOG`, and
since GitHub #39 they no longer live in `lancet-catalog` at all.

### Rollout gate and self-check

- **Alpha rollout gate:** `cargo xtask rollout --channel alpha` - rejects a
  tracked-dirty worktree, builds a locked release binary, runs
  Direct-Local/offline/dogfood/release-operator-matrix/doctor evidence steps against it,
  and writes a versioned JSON record to `target/rollout/`. See
  [docs/TESTING.md](./TESTING.md).
- **LANCET self-check:** `lattice lancet doctor` (or `cargo run -p lattice-cli
  -- lancet doctor`) - verifies the production registry fully registers the
  CATALOG, in-process transport smoke-tests, base envelope constructs, and mode
  policy parses cleanly.
- **v1.4-alpha release record:** `docs/archive/releases/v1.4-alpha-release-record.md` - archived, unfilled evidence template for the retired host-isolation model.
- **Warn-mode report:** `docs/archive/releases/lancet-warn-report.md` - archived v1.3.1-alpha warn-mode baseline.
- **Capability boundary walkthrough:** `docs/archive/releases/v1.3-alpha-walkthrough.md` - archived v1.3-alpha Layer-1 walkthrough.

### Mode semantics

`mode = "off"` under `[lancet]` skips effect-region enforcement only. The CEP policy hook still runs; capability declarations are still required, and effect-kind denial remains a hard fail. Legacy direct tool calls are not re-enabled - there is no legacy tool executor path in the production stack.

Effect-kind denial is enforced in two layers, and the first one is a genuine
write barrier. Configure it as `deny = ["workspace_mutation"]` under
`[lancet.effects]` or `[effects]` in an external policy file.

1. **Pre-dispatch (declared kinds, prevents).** Before any handler runs, the
   transport compares the method's static `EffectDecl.kinds` from
   `lancet_catalog::catalog()` with the turn's denied kinds. A hit returns
   `EffectViolation` without entering the backend, so a denied `mutate.create`
   or `mutate.edit` never touches Local Workspace and a denied
   `observe.workspace` never reads it.
2. **Post-dispatch (observed kinds, defense in depth).** After a successful
   dispatch the same deny set is checked against the effect ledger, which
   catches a handler whose runtime effects diverge from its declaration. This
   layer reports a side effect that has **already** happened: per ADR 0003
   decision 8 a successfully installed Atomic Local Mutation is never rolled
   back, so an `EffectViolation` raised here means "denied effect landed and
   was reported", not "denied effect prevented".

Neither layer is tunable by `mode`: `warn` and `off` gate effect-*region*
checks only. Both layers key on the fixed native catalog, so they apply to
native methods only; Component Tool policy is `[lancet.capabilities]`
admission, which never names impact identity.

Because layer 1 keys on the declared kind, one deny entry covers every native
method declaring that kind, but it cannot express "deny only this path" — that
is `[lancet.workspace] denied_write_patterns`.

### Command physical-boundary configuration (retired)

Landlock/netns command isolation, the `[lancet.isolation.command]` profile
schema, and the Phase 29 native supervisor (unprivileged user namespaces,
minimal mount view, allowlist environment, fd cleanup) were removed - see
[docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed subsystems". `exec.command_run`
itself is retired (a raw CEP call returns JSON-RPC `-32601`), so there is no
command-isolation configuration surface. Git history retains the phase
narrative.

### Phase 30 overlay modes and artifact commit (retired)

The Phase 30 overlay/artifact-commit pipeline (diagnostic/artifact overlays,
candidate verify layers, lower-drift recheck, HashLine commit-baseline gate)
belonged to the retired `exec.command_run` command-isolation subsystem and was
removed with it - see [docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed
subsystems". The metadata keys it emitted (`overlay_status`,
`artifact_adjudication`, `commit_baseline_validation`, etc.) are no longer
produced. Git history retains the phase narrative.

## Mutation and command observation

### Semantic edits

`mutate.edit` edits one existing file with semantic operations such as
`replace_text`, `insert_before`, and `insert_after`. The caller supplies the
file `expected_hash` from `observe.workspace`; LANCET verifies that hash on the
`workspace-mutate` host before guest compilation and again at the final atomic
install operation. Semantic ops run in the guest (`tools/mutate-edit` +
`mutate-shared`); their validation/replan diagnostics return through a
non-installing host refusal, and the LLM-facing tool surface does not accept
hashline patch documents.

Edits are forced to be based on the current Local Revision before commit. Audit
metadata records MutationEvidence (before/after revision, precondition,
installation). Agent-visible workspace mutation is limited to `mutate.create`
and `mutate.edit` (single-file methods only).

### Create-only writes

`mutate.create` is create-only. It fails if the target already exists and does
not create missing parent directories. Existing-file edits must go through
`mutate.edit`.

### Command boundary metadata (retired)

The `effective_boundary` / `host_probe` metadata model, the
`physically_bounded` attribution rules, and the `exec.command_run` boundary
JSON documented here belonged to the retired command-isolation subsystem and
were removed with it - see [docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed
subsystems". Git history retains the metadata schema narrative.

## LANCET CLI inspection and self-check

- `lattice lancet status` reads the external project-keyed audit store via
  `audit_db_path_for_project` (no CEP server) and shows recent CEP calls plus
  the last guard verdict. `--json` emits the same data as JSON.
- `lattice lancet doctor` runs LANCET self-checks: the production registry
  fully registers the CATALOG, in-process transport smoke-tests, base envelope
  constructs, and mode policy parses cleanly. It exits nonzero if any check
  fails; `--json` emits a `{ ok, checks }` object.

### Live canary classification (retired)

The live Linux isolation canary lane (the ignored Rust canary suite, gated by a
live-isolation env flag) was removed - see
[docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed subsystems". Git history
retains the classification narrative.

## Phase 29 non-claims (retired)

Phase 29 command-boundary reporting and the applied-evidence model were part of
the retired `exec.command_run` command-isolation subsystem and were removed
with it - see [docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed subsystems".
Git history retains the non-claims narrative.

## Operator Audit

### Flat Store and Cell Evidence

Audit rows live in SQLite (`cep_audit_entries`). Production persistence uses the
**external project-keyed path** under the Linux XDG state directory
(`$XDG_STATE_HOME/lattice/projects/<sha256-canonical-root>/cep_audit.db` via
`audit_db_path_for_project`, defined in `lancet_audit::store_path` and
re-exported as `lancet_api::audit_db_path_for_project`). Each finalized CEP call
is one append-only row. The row carries `session_id`, `turn_id`, nullable
`model_id` / `runtime_version`, lifecycle labels, and a compact result summary.

The database carries its schema version in `PRAGMA user_version`. A newer
version, or an unversioned database whose required `cep_audit_entries` shape
cannot be adopted, fails the open with an actionable error without rewriting
audit history. The former turn-attestation table, `attestation_records`, is
retired and is dropped on store open; no attestation migration is retained.

Handler and transport layers may keep compact query keys in `metadata_json`,
including native `required_capability`, observed capability labels, and a
Component row's internal `evidence_id`. The field is not an operator response
surface and never serializes through audit query, tail, replay, or status.

Component Tool calls persist the existing versioned Cell-evidence records in
`evidence_calls`, `evidence_effects`, and `evidence_denials`. The records carry
why admission succeeded or failed, observed effects, and denied operations;
their versions are checked before a reader interprets them. A Component audit
row may point at those records through its internal `evidence_id`. A missing
pointer is valid for native and Component rows alike and exposes
`evidence: null`; the row remains visible. A present but dangling, malformed,
unsupported, corrupt, or row-mismatched pointer is an operator read error, not
a no-evidence value.

This SQLite shape is the complete transitional operator authority after #114,
not the ADR 0005 target. Its `audit_id`, numeric `call_id`, and `evidence_id`
are private/transitional storage values rather than invocation identity. #139-B
replaces them with the `lancet-audit` Git InvocationStore Adapter in one hard
cut; #139-A must not expose its parallel foundation as a second production
read path.

### Operator audit reads

Audit store reads are **host/operator only**. `lattice audit` calls
`lancet-api::{query_audit, tail_audit, replay_audit}` against the project-keyed
store, with no CEP server. `CepAuditStore::query_rows` returns only **finalized**
rows (`ended_at` and `result_or_error` set), ordered by `audit_id`. Agents do not
receive `ReadAudit` or audit methods in `[tools].enabled`.

`query_audit` emits `{ session_id, row_count, rows }`, where each row contains
the full finalized audit fact set plus `evidence`; its database query remains
capped at 10,000 rows and supports the inclusive `since` lower bound.
`tail_audit` emits compact rows and uses an exclusive `audit_id` cursor.
`replay_audit` returns every finalized row for the selected turn in audit-id
order, without inheriting the query cap; it does not rebuild a graph, statement,
source range, or attestation comparison. Status asks SQLite for
`audit_id DESC LIMIT 20`, then exposes those newest rows in that order. Query
and tail retain the session, turn, method, impact, capability, verdict, and
cursor filters.

Since GitHub #39 the `lancet-api` entry points are one-line delegations and the
behavior — request DTOs, the `AUDIT_TAIL_MAX_ROWS` / `AUDIT_TAIL_MAX_BATCHES`
pagination loop, filter construction, typed evidence materialization, and
contract tests — lives in `lancet-audit::operator`. "No CEP server" is enforced
structurally: `lancet-audit` depends on none of `lancet-native`,
`lancet-dispatch`, `lancet-cep`, or `lancet-runtime`, so the read path cannot
name a server, transport, or registry type. The tail cancellation contract
(L-11-01) and the synthetic-writer concurrency contract (L-11-02) are documented
in that module's doc comment.
