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

LATTICE agents execute tools through **LANCET**, the controlled execution and failure-learning subsystem. LANCET uses **CEP** (Controlled Execution Protocol) internally: JSON-RPC methods with capability envelopes, policy gating, path guards, and a flat append-only audit store.

## Public surface (v1.4-alpha boundary contract)

The documented public LANCET contract is framed by **`lancet-cep`**, which
re-exports the CEP protocol/catalog/dispatch interface and owns tool exposure
plus base-authority planning. The underlying contract remains split across
**`lancet-protocol`** and **`lancet-catalog`**:

| Component | Description |
| ----------- | ------------- |
| `lancet-cep` | Public CEP facade: protocol/catalog/dispatch re-exports, catalog-derived `ToolDefinition`, `ToolExposure`, descriptor-declared base authority aggregation, and explicit `BaseAuthorityPlan` construction |
| `lancet-catalog::CATALOG` | The **5-method** canonical agent CEP catalog plus descriptor metadata, including method-local base authority requirements: `observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`, `session.todo_write`. (`session.envelope`, `exec.command_run`, `mutate.batch`, post-mutation verify, and agent-visible audit CEP methods were removed - catalog and tests assert their absence.) |
| `lancet-api::audit` | Stable operator entry points for audit query/tail/replay over the project-keyed audit store (`query_audit`, `tail_audit`, `replay_audit`); a thin facade that delegates to `lancet-audit::operator`, which owns the DTOs, pagination, filters, and projection. Not agent CEP methods. |
| Method params/results | Typed `*Params` / `*Result` structs per catalog method in `lancet-catalog` |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` in `lancet-protocol` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types in `lancet-protocol` |
| `PolicyHook` / `PolicyDecision` | Pluggable policy hook interface in `lancet-protocol` |
| Audit/provenance schemas | `ProvGraphFragment` wire shapes in `lancet-protocol`; audit rows, filters, attestation persistence, operator read DTOs, and project-keyed store-path resolution in `lancet-audit` |

Internal implementation is split across `lancet-authority` (default policy, Guard L1, active envelope, effect-region semantics) and `lancet-native` (production handlers, transport lifecycle, edit/web integration).

**Not public surface:** `lancet-native` handler internals, private low-level executor helpers, authority implementation details, and any `#[doc(hidden)]` re-exports. These are implementation details of the native LANCET server.

Pre-v1.3.1 method names are removed. They are not accepted by the adapter or
native registry, and LLM-facing tool definitions are projected only from the 5
canonical agent catalog methods. Tool exposure is separate from base authority:
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
| `mutate.create` | Workspace mutation by pure file creation. Existing targets are rejected; missing parent directories are rejected. |
| `mutate.edit` | Workspace mutation of existing files through semantic replace/insert operations. The caller provides `expected_hash`; LANCET computes hashline anchors internally. |
| `fetch.web` | External observation through URL fetch policy. |
| `session.todo_write` | Session-state mutation for todo state. |

Audit query, tail, and turn replay are **not** catalog methods. Operators use
`lattice audit` (default: query/tail; `lattice audit replay --turn <id>` for
turn provenance reconstruction), backed by `lancet-api::{query_audit,
tail_audit, replay_audit}`, which delegate to `lancet-audit::operator`; see
**Provenance Audit** below. The operator DTOs never lived in `CATALOG`, and
since GitHub #39 they no longer live in `lancet-catalog` at all.

### Rollout gate and self-check

- **Alpha rollout gate:** `cargo xtask rollout --channel alpha` - rejects a
  tracked-dirty worktree, builds a locked release binary, runs
  Direct-Local/offline/dogfood/audit-replay/doctor evidence steps against it,
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

Effect-kind denial is enforced at checker time, after runtime effects are
observed. Configure it as `deny = ["workspace_mutation"]` under
`[lancet.effects]` or `[effects]` in an external policy file. This is stronger
than disabling a method prefix: effect-kind denial is keyed on observed impact
kind rather than method name, so it catches any side effect of a denied kind
regardless of which method produced it.

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
file `expected_hash` from `observe.workspace`; LANCET verifies that hash and
computes hashline anchors inside the native edit backend. The LLM-facing tool
surface does not accept hashline patch documents.

The internal hashline backend still forces edits to be based on the current file
state before commit. Audit metadata records changed files and line-level
add/remove effects. Agent-visible workspace mutation is limited to
`mutate.create` and `mutate.edit` (single-file methods only).

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

## Provenance Audit

### Flat Store, Projected Graph

Audit rows live in SQLite (`cep_audit_entries`). Production persistence uses the
**external project-keyed path** under the platform state directory
(`$XDG_STATE_HOME/lattice/projects/<sha256-canonical-root>/cep_audit.db` via
`audit_db_path_for_project`, defined in `lancet_audit::store_path` and
re-exported as `lancet_api::audit_db_path_for_project`). Each finalized CEP call is one
append-only row. **PROV-DM** structure (`Activity`, `Entity`, `Agent`, relations)
is **not** stored as a graph; it is **projected** at read time into a
`ProvGraphFragment` wire type (operator `query_audit` / `replay_audit`).

The database carries its schema version in `PRAGMA user_version`. A newer
version, or an unversioned database whose `cep_audit_entries` or
`attestation_records` shape predates the current columns, fails the open with an
actionable error naming the path and the missing table-qualified columns; the
file is never altered, so audit history is neither dropped nor rewritten. The
read-only legacy import path is unaffected. Failing with an action is the
deliberate choice over opening the old database read-only beside a fresh one:
one project root keeps one append-only history, and no reader joins two files
for a turn's replay or attestation range.

### Agent Triple

Rows carry `session_id`, `turn_id`, and nullable `model_id` / `runtime_version` columns populated from `CepAuditContext` at `start_call` so replay can reconstruct the Agent node from audit alone.

### Metadata Seeds

Handler and transport layers write compact `metadata_json` keys such as paths
and `content_hash`. These fields are intentionally summary-only:
full request and response bodies are not stored in audit metadata.

Native handlers also persist compact execution evidence:

- `cep_policy` records the policy decision, declared effect ceiling,
  capability-match summary, budget/network facts when available, and the
  post-policy authorized region summary.
- `cep_preflight` records method-local pre-execution facts. Command calls
  include declared/effective impact, effective effect kinds,
  RunCommand allowlist evidence, and WritePath region summaries for
  workspace-mutating commands. Mutation calls include target paths, out-of-scope
  paths, expected-hash status, verify configuration, and replan reason when
  known.
- `approval_request` is emitted only for `PolicyDecision::HumanReview`. It is
  approval-ready evidence for a future outer loop: method, reason, required
  capability, and retry hint. LANCET does not auto-approve
  or prompt interactively in the native handler.

Operator audit APIs and `replay_audit` project `cep_policy`,
`cep_preflight`, `approval_request`, and `effective_boundary` from flat row
metadata into PROV activity attributes. This keeps audit rows append-only while
making preflight and approval evidence visible to replay clients.

Transport finalize merges catalog `required_capability` and effect-ledger `observed_capabilities` without changing the audit write lifecycle.

Component Tool calls record why the call was authorized apart from what it did:
`admission_evidence` holds the approved/denied/boundary-excluded capability
names, `observed_effects` the actual Cell + `ObservedScope` + concrete target
entries, and `denied_operations` the per-operation refusals. All three are
versioned objects with seq-ordered inner arrays, so a reader checks `version`
before interpreting any of them. `component_cells` remains the 4-segment query key and is
derived from the effect ledger, so a multi-effect call cannot disagree with
itself. Operator audit APIs and `replay_audit` project all three into PROV
activity attributes.

### Attestations

Per-turn **unsigned in-toto** statements are stored in `attestation_records` in the same audit database (not git-tracked files). Signing and key management are deferred beyond alpha; wire shapes remain forward-compatible.

Predicate versions coexist. A turn is attested with the v2 predicate iff one of
its rows carries versioned evidence metadata, else v1; v2 statement bytes are
canonical JSON. On replay the stored `predicate_type` wins, so a stored v1
statement is never re-projected as v2 and a v2 statement compares byte for byte.

### Time-Range Proxy

Monotonic `audit_id` range (`since_audit_id` / `until_audit_id`) is the
primary time-order proxy when wall-clock `started_at` bounds are omitted.
Optional `started_after` / `started_before` filter on the millis `started_at`
column.

### Operator audit reads

Audit store reads are **host/operator only**. `lattice audit` calls
`lancet-api::{query_audit, tail_audit}` against the project-keyed store (no CEP
server). `replay_audit` rebuilds turn traces and optional in-toto statements from
stored rows. `CepAuditStore::query_rows` returns only **finalized** rows
(`ended_at` and `result_or_error` set). Agents do not receive `ReadAudit` or
audit methods in `[tools].enabled`.

Since GitHub #39 the `lancet-api` entry points are one-line delegations and the
behavior — request DTOs, the `AUDIT_TAIL_MAX_ROWS` / `AUDIT_TAIL_MAX_BATCHES`
pagination loop, filter construction, wire projection, and the replay
attestation comparison — lives in `lancet-audit::operator`, with the contract
tests in `lancet-audit::operator_tests`. "No CEP server" is enforced
structurally: `lancet-audit` depends on none of `lancet-native`,
`lancet-dispatch`, `lancet-cep`, or `lancet-runtime`, so the read path cannot
name a server, transport, or registry type. The tail cancellation contract
(L-11-01) and the synthetic-writer concurrency contract (L-11-02) are documented
in that module's doc comment.
