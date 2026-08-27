# LANCET runtime-enforced surface

> **CURRENT BOUNDARY - 2026-08-23:** ADR 0003's guarded Direct-Local Workspace
> path is production; the MSB/Upper path, `[workspace]` profile section, and
> `lattice-workspace` crate are deleted. ADR 0004's manifest-owned Component
> composition is production for all five built-ins. `fetch.web` joined the
> embedded Component catalog in #32 and reaches the network only through the
> async `network-fetch` Tool Host import. The production native catalog is empty.
> Components have no ambient WASI filesystem, network, or process authority.
>
> ADR 0005's durable invocation hard cut is also production. One typed
> `InvocationId` spans binding, CEP execution, Host context, history, terminal
> Cell evidence, events, cancellation, and operator output. `lattice-api`'s
> `TurnCoordinator` is the sole product invocation owner; lower LANCET APIs exist
> for composition/internal tests only and require an exact durable binding. The
> project-keyed Git `InvocationStore` is invocation/evidence/operator authority.
> SQLite audit/evidence stores, numeric call/audit/evidence identities, dual
> writes, imports, fallback readers, Standalone invocation, and direct CEP product
> callers are deleted. Provider Tool Call IDs remain optional replay metadata.
> The JSON-RPC request `id` is the already-reserved `InvocationId`; inbound
> notifications cannot create or dispatch an invocation.

LATTICE executes Tools through **LANCET**, its controlled execution subsystem.
CEP (Controlled Execution Protocol) is the typed JSON-RPC method protocol behind
the runtime; admission combines exact durable binding state, capability/policy
gates, path guards, effect checks, and the authoritative append-only Git
InvocationStore ledger.

## Public surface (v1.4-alpha boundary contract)

The documented public CEP contract is framed by `lancet-cep`, which re-exports
the transitional protocol/catalog/dispatch types. Runtime Tool definitions come
from the closed generated Component inventory; Source Tool manifests own every
built-in descriptor.

| Component | Description |
| ----------- | ------------- |
| `lancet-cep` | Transitional CEP facade: protocol/catalog/dispatch re-exports plus generic `ToolDefinition` and exposure helpers |
| `lancet-catalog::catalog()` | Empty production native catalog after #32; generic method traits and dev-only scaffold entries remain until #34 |
| `lancet-api::audit` | Stable operator query/tail/replay entry points over project-keyed `invocations.git`; a thin facade over `lancet-audit::operator` typed DTOs, pinned views, OID cursors, filters, and Cell-evidence projection. Not Agent Tools. |
| Tool params/results | Manifest-generated descriptor schemas and guest types for built-ins; residual typed native traits remain generic migration machinery |
| `CepError` / `CepErrorCode` | Typed error families with `Retriability` in `lancet-protocol` |
| `ExecutionEnvelope` / `Capability` / `Scope` | CEP capability, scope, and impact envelope types in `lancet-protocol` |
| `PolicyHook` / `PolicyDecision` | Pluggable external policy hook interface in `lancet-protocol` |
| Invocation/evidence schemas | Typed bindings and versioned Cell-evidence shapes in `lancet-protocol`; authoritative records, pinned operator DTOs, and project store opening in `lancet-audit` |

Internal implementation is split across `lancet-authority` (policy/guard/effect semantics), `lancet-runtime` (Component execution and Tool Host authority), and `lancet-native` (CEP server lifecycle and transitional routing).

**Not public:** transitional `lancet-native` server internals, low-level executor helpers, authority implementation details, and `#[doc(hidden)]` re-exports.

Pre-v1.3.1 method names are removed. They are not accepted by the runtime or
runtime registry. LLM-facing definitions come from the closed generated
five-Component inventory. Tool exposure is separate from base authority:
Enabled methods filter the model-visible generated definitions. Cell admission
and per-operation Tool Host authorization decide execution authority. Legacy
`ToolExposure::base_authority()` and `BaseAuthorityPlan` remain only for the
transitional generic/native machinery; they grant no production built-in.

### Effect-contract tool surface

The canonical method names are grouped by primary effect class:

| Method | Primary effect contract |
| -------- | ------------------------- |
| `observe.workspace` | Workspace observation. `mode=read_file` has single-path scope; `mode=grep`, `mode=list_directory`, and `mode=find_files` use root/directory observation scope. |
| `mutate.create` | Workspace mutation by pure file creation (embedded Component Tool since #31). Existing targets are rejected; missing parent directories are rejected. |
| `mutate.edit` | Workspace mutation of existing files through semantic replace/insert operations (embedded Component Tool since #31). The caller provides `expected_hash`; LANCET computes hashline anchors internally. |
| `fetch.web` | External observation through a GET-only embedded Component. The host revalidates every URL/redirect, pins DNS results to connections, verifies TLS, enforces bounds and the shared observation budget, and records content-free per-hop evidence. |
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

`mode = "off"` under `[lancet]` skips legacy effect-region enforcement only.
Component admission, workspace policy, and network policy remain host-enforced.

The production native catalog is empty after #32. A non-empty
`[lancet.effects] deny` therefore fails runtime construction instead of becoming
a silent no-op. Migrate impact-kind denies to manifest capability names under
`[lancet.capabilities] deny`, for example `external_observation` →
`network.fetch`, `workspace_observation` → `workspace.observation`, and
`workspace_mutation` → `workspace.mutation`. Legacy `ImpactKind` checks remain
only for generic native/scaffold dispatch until #34 removes that vocabulary.

`fetch.web` also requires `max_boundary = "ex"` and a non-empty
`[lancet.network] allowed_hosts`. `allow_public_https` enables public HTTPS but
does not grant any host; localhost additionally requires `allow_localhost = true`.
Every redirect is checked against the same rules.

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

- `lattice lancet status` opens the authoritative project-keyed
  `invocations.git` directly (no CEP server), pins one ledger view, and shows its
  `ledger_oid`, newest 20 current invocation records, and last lifecycle verdict.
  `--json` emits the same typed data.
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

## Operator Invocation Ledger

### Git store and Cell evidence

Every accepted production invocation is keyed by one typed `InvocationId` in the
project's authoritative Git ledger:

```text
$XDG_STATE_HOME/lattice/projects/<ProjectStoreKey>/invocations.git
```

The store owns exact ordered batch reservation, durable `DispatchStarted`, atomic
terminal-state plus versioned `CellEvidenceTriple` publication, an ordered
high-water witness, and pinned linear-ancestry views. Current records include
`Reserved` and `DispatchStarted`, not only terminal calls. Terminal material is
represented by outcome-independent digest/length/availability metadata; evidence
contains admission, observed-effect, and denial facts without Tool content.

Arguments, result/fetched content, provider payload or opaque replay state,
credentials, provider IDs as correlation identity, and internal metadata blobs are
not operator fields. SQLite tables, `metadata_json`, pointer IDs, numeric
call/audit/evidence IDs, schema adoption, import, dual read/write, and fallback
opening are deleted. Old disposable development state is rebuilt explicitly; see
[DEVELOPMENT.md](./DEVELOPMENT.md#manual-development-state-rebuild).

### Query, tail, replay, and status

Operator reads are host-only. `lattice audit` delegates to
`lancet-api::{query_audit, tail_audit, replay_audit}` and never composes a CEP
server or takes a project operation lock. Agents receive no audit Tool.

- Query pins the requested `--at <ledger-oid>` view, or the current tip, and
  returns at most 1,000 current records filtered by typed Session/Turn identity,
  method, and lifecycle verdict.
- Tail uses `--since <ledger-oid>` as an exclusive reachable commit cursor, returns
  lifecycle transitions in commit order, and never splits one atomic reservation
  commit at the page boundary.
- Replay pins one view and returns every invocation for the typed Session/Turn in
  structural model-step/part order.
- Status pins one view and returns that `ledger_oid`, the newest 20 current records,
  and the latest lifecycle verdict.

The owning behavior and contract tests live in `lancet-audit::operator`;
`lancet-api` is a thin error-mapping facade. Structural dependencies keep
`lancet-audit` free of `lancet-native`, `lancet-dispatch`, `lancet-cep`, and
`lancet-runtime`, so the read path cannot name a server, transport, or registry
type.
