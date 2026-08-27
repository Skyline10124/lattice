# AGENTS.md

Workspace-level architecture guidance for coding agents. This file intentionally
stays high level; for crate-local ownership, key files, and focused verification,
read `crates/<crate>/AGENTS.md`.

## Agent skills

### Issue tracker

Issues and PRDs are tracked in GitHub Issues for `Skyline10124/lattice`; external pull requests remain outside issue triage. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses the canonical `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix` labels. See `docs/agents/triage-labels.md`.

### Domain docs

This repository uses a single-context domain documentation layout. See `docs/agents/domain.md`.

## Platform Contract

LATTICE supports Linux hosts only for development, build, test, deployment,
and runtime operation. The `wasm32-unknown-unknown` Source Tool target is a
guest artifact format, not a second supported host platform.

- Do not add or retain macOS, Windows, BSD, generic-Unix, or other host
  compatibility branches, no-op fallbacks, non-Linux host target dependency
  sections, CI lanes, packaging, or documentation promises.
- Do not choose a weaker portable primitive or degraded behavior to preserve
  hypothetical future multi-platform support. Use the strongest appropriate
  Linux facility and fail closed when its contract cannot be met.
- Any future expansion beyond Linux requires an explicit architecture decision
  and a complete native design; it must not arrive incrementally through
  compatibility shims in Linux production paths.

## Architecture

LATTICE is a session-centric LLM runtime. Conversation state lives in a
git-style session graph, public turn execution is exposed through `lattice-api`,
and tool execution flows through LANCET. LANCET uses CEP (Controlled Execution
Protocol) as a JSON-RPC method protocol with direct policy gating, path
gating, path guards, effect checks, and an audit trail.

ADR 0003 is the accepted workspace target: structured LANCET Tools operate
directly on the Local Workspace. ADR 0004 is the accepted Tool target: each
built runtime has a closed catalog of embedded WASM Components whose source
manifests own their descriptors; the default distribution contains five Tools,
and operators may install precompiled Tool Bundles before rebuilding a new
closed runtime. The MSB/Upper path and `lattice-workspace` crate were removed
in GitHub #62; the central catalog authority, native handlers, and native/CEP/dispatch
split remain migration residue. Do not add projection, checkpoint, ambient Tool
split remain migration residue. Do not add workspace projection/checkpoint,
ambient Tool authority, runtime code loading, user-process, or
arbitrary-command capabilities while completing the migration. ADR 0005's
PendingTurn is a conversation recovery journal, not a workspace checkpoint.

ADR 0005 is the accepted durable Turn and invocation target. It introduces one
typed `InvocationId`, a data-only `lattice-conversation` contract, a generic
private-Git `lattice-store` kernel, durable PendingTurn reconciliation, and a
`lattice-api::TurnCoordinator`. Session, invocation/evidence/operator, and
workspace-history semantics remain three separate domains in
`lattice-session`, `lancet-audit`, and `lancet-runtime`; sharing Git mechanics
does not merge their repositories, locks, state machines, or failure domains.
Implementation is ordered by GitHub #141–#144 and #139-A/#139-B. #139-A was
approved on 2026-08-22 and #139-B may proceed with the production hard cut. Do
not add compatibility dual writes or legacy read/import paths. The correctness,
strict-reopen, bounded-read, and cold-read structural regressions remain
required.

The performance campaign is temporarily diagnostic-only. Its metric definitions
remain in `benchmarks/invocation-store/qualification-contract-v2.json`, and the
current decision record is
`docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`. The official
runner writes ignored local bundles below `target/invocation-store-bundles` and
uses `benchmark`; strict `qualify` is retained for the redesign tracked by
GitHub #153. Historical PASS/FAIL summaries remain evidence, not a prerequisite
for #139-B, and must not be rewritten or presented as a fresh approval campaign.

The workspace dependency graph is layered. Keep dependencies pointing from
higher layers to lower layers; do not introduce reverse dependencies. The graph
below describes the current transition state. ADR 0004 absorbs or retires the
central catalog, CEP, dispatch, and native surfaces through `lancet-runtime`
after Component Tool migration.

Every edge below is a real `workspace = true` dependency in that crate's
`[dependencies]`. Spell edges out rather than summarizing them as "+ lattice
runtime crates": a summarized edge cannot be checked against
`cargo metadata`, and the previous summary hid seven live edges.

```text
Layer 0 (leaf):  lancet-protocol    lattice-store
Layer 0.5:       net-guard                       (no workspace deps; see note)
                 lattice-conversation(->protocol)
                 lattice-llm(->net-guard,conversation)
Layer 1:         lancet-catalog(->protocol)
                 lattice-profile(->protocol)
                 lattice-session(->protocol,conversation,
                               lattice-store)
                 lattice-agent(->llm,conversation)
Layer 2:         lancet-dispatch(->protocol,catalog)
                 lancet-audit(->protocol,catalog,lattice-store)
                 lancet-authority(->protocol,catalog,net-guard)
Layer 2.5:       lancet-cep(->protocol,catalog,dispatch)
                 lancet-runtime(->protocol,dispatch,authority,net-guard,
                                lattice-store)
Layer 3:         lancet-native(->protocol,catalog,dispatch,audit,
                               authority,runtime)
Layer 3.5:       lancet-api(->protocol,catalog,dispatch,cep,audit,
                            authority,native,runtime)
Layer 4:         lattice-api(->lancet-api,lancet-protocol,lancet-catalog,
                             lancet-audit,lancet-authority,lancet-native,
                             lattice-agent,lattice-conversation,lattice-llm,
                             lattice-profile,lattice-session)
Layer 5:         lattice-tui(->lattice-api + presentation dependencies)
                 lattice-cli(->lattice-api,lattice-session,lattice-tui,
                              lancet-api + presentation dependencies)

Not in the graph: lattice-tool-prelude and lattice-system-harness have no
workspace-internal dependencies by contract. The prelude is a guest SDK
(`wit-bindgen` only); the harness must stay free of product crates so it can
only reach the product through the built binary (`cargo xtask
harness-boundary` enforces this, transitively).
```

ADR 0005 dependency deltas land in prerequisite order. The graph above records
the current `Cargo.toml` state; the list below separates the landed #139-A edge
from the remaining #139-B delta:

```text
# lattice-store landed in #143 with no workspace deps and appears in the graph above
# lattice-session -> lattice-store and lancet-runtime -> lattice-store landed
# in #143, and lattice-session -> lancet-protocol landed in #144; all are
# already in the graph above
# lancet-audit -> lattice-store landed in #139-A
# lattice-api -> lancet-audit,lattice-conversation landed with the #139-B
# program (Commit 1); both edges are already in the graph above
# lattice-api -> lancet-native remains through #139-B and is deleted by #34
```

There is no `lancet-audit -> lattice-session` edge, and none may be added.
`lancet-audit` owns the invocation ledger and returns audit-domain types only.
Projecting a pinned ledger view into
`lattice_session::pending::InvocationFactsSnapshot` is cross-domain composition:
the production projection lives in `lattice-api/src/turn/invocation_facts.rs`,
while the #139-A qualification projection remains in the non-production seam
(`crates/lancet-audit/tests/invocation_qualification.rs`, backed by a
dev-dependency). Putting either projection in the `lancet-audit` production
surface would make `lancet-api -> lancet-audit -> lattice-session` real and break LANCET's
independent embeddability. `lattice-session` must not depend on `lancet-audit`
either; applying a candidate revision is session-domain I/O
(`GitSessionGraphStore::execute_candidate_revision`) over a session-domain
request. Verify with `cargo tree -p lancet-api -e normal`.

`lattice-store` owns only repository creation/opening, canonical bytes, object
and ref CAS, Linux flock/fsync/rename durability, retention, and typed store
errors. It must not depend on or absorb session, runtime, provider, or audit
domains. All state is below `$XDG_STATE_HOME/lattice`: one global
`sessions.git`, plus project-keyed `invocations.git` and
`workspace-history.git`. `ProjectStoreKey` hashes the canonical Linux path bytes
of the full Local Workspace root; it is an address, not content/project identity.
Code must never hold two repository write locks simultaneously.

`lancet-runtime -> lancet-authority` arrived with #30, when the concrete Local
Workspace moved down into `lancet-runtime`: the visibility engine's decision
vocabulary (`ReadVisibilityPolicy` / `HiddenReason` / `ReadVisibility`) and the
System Path Exclusion predicate are policy *semantics* and stay in
`lancet-authority`, so the runtime consumes them rather than reimplementing them.
The edge points down (2.5 -> 2) and closes no cycle.

`net-guard`, `lattice-conversation`, and `lattice-llm` sit at 0.5 rather than 0
because `lattice-llm` depends on `net-guard` and (since #142)
`lattice-conversation`. `lancet-protocol` and `net-guard` are the crates with
no workspace-internal dependencies at all.
`lattice-api` and `lattice-cli` reach past the `lancet-api` facade into
individual LANCET crates. Those edges point downward, so they are not layering
violations, but they are load-bearing: `lattice-api` owns the profile→policy
translation and needs policy types (`lancet-authority`) and error/impact
mapping (`lancet-native`) that `lancet-api` does not re-export, and
`lattice-cli` drives operator audit commands through `lancet-api` directly.
Prefer naming a type through `lancet-api` when it re-exports it; do not add new
crates to these edge sets without a reason recorded here.

`cargo xtask lancet-boundary` now machine-checks the forbidden dependency and
legacy-surface set required by the hard cut, including the absence of
`lattice-agent -> lancet-native` and production test-only feature reachability.
GitHub #108 still tracks turning the complete positive edge diagram into gate
data; `cargo metadata` and this diagram remain the authority for edges not yet
encoded there.

## Crate Roles

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contracts: JSON-RPC frames, errors, policy hook types, effect types, and versioned Cell-evidence types. No I/O and no tokio. |
| `lancet-catalog` | Legacy central Tool-contract authority scheduled for removal by ADR 0004 after source manifests and generated descriptors take over. |
| `lancet-dispatch` | Current generic registry/transport surface scheduled for absorption into `lancet-runtime`. |
| `lancet-cep` | Current CEP/catalog projection facade scheduled for absorption or retirement after descriptor-generated projection lands. |
| `lancet-audit` | Production project-keyed Git `InvocationStore`, exact reservation/admission lifecycle, terminal material and Cell evidence, pinned views, and typed operator query/tail/replay DTOs. Its public surface is audit-domain only; `lattice-api` projects pinned ledger facts into Session reconciliation types. |
| `lancet-authority` | Policy semantics, capability matching, Guard L1 path/scope checks, effect ledger, and effect-region checks. |
| `lancet-native` | CEP server lifecycle, durable admission handoff, and the transitional Component/native dispatch split. The production native registry is empty after #32; no built-in has a native fallback. |
| `lancet-runtime` | Component engine, generated descriptor inventory, Tool Host enforcement, concrete Local Workspace, GET-only pinned network transport, Cell evidence, and private gix history. All five built-ins execute as Components after #32. |
| `lattice-tool-prelude` | ADR 0004 target guest SDK for source manifests, generated WIT bindings, descriptor helpers, and built-in Source Tools. |
| `lancet-api` | Unified public API. During migration it composes native CEP pieces; the target composes `lancet-runtime` without depending on LATTICE session/agent/profile layers. |
| `net-guard` | Neutral URL/DNS safety leaf: base-URL validation, public-address resolution, private/reserved IP checks, and rebinding-domain checks. Depends only on `url` and `std`. |
| `lattice-llm` | Provider adapters, SSE streaming, model routing, tool-call vocabulary, HTTP request pinning, logging, and (since #142) the pure-data provider replay/projection seam over `lattice-conversation`. |
| `lattice-conversation` | ADR 0005 §7 data-only provider-neutral conversation contract: ordered `NativeMessage`/`MessagePart` with `UnboundToolCall`/`BoundToolCall` typestates, ToolResult closure, typed `HeadTranscript`, semantic/replay digests, and per-attempt usage/interruption/rejection records. Depends only on `lancet-protocol`; no I/O, stores, runtimes, or ID generators. |
| `lattice-session` | Git-backed Session graph, immutable typed Turn commits, branch/HEAD state, full-Turn execution lock, and durable PendingTurn journal/reducer under `active-turn` and permanent `turns/<TurnId>` pins. Final Turn publication is reachable only through the journal transaction; direct commit helpers compile only under `test-utils`. |
| `lattice-store` | ADR 0005 domain-neutral private-Git/Linux durability kernel (landed in #143): XDG state topology, `ProjectStoreKey` derivation, typed create/open with `refs/lattice/meta` identity, canonical bytes, object/commit mechanics, ref CAS and multi-ref transactions, per-repository flock, fsync/rename durability barriers, atomic first-create, retention primitives, and per-location capability probes. No domain schemas and no workspace-internal dependencies by contract. |
| `lattice-profile` | Strict `agent.toml` schema, discovery, defaults, and LANCET profile sections. |
| `lattice-agent` | One-attempt `ModelStepDriver` over the injectable `LlmClient` seam (since #139-B Commit 4): frozen request assembly, protocol-ceiling metering, typed attempt outcomes. No loop, Tool execution, retry policy, or store authority. |
| `lattice-api` | Public Session/Turn facade and production `TurnCoordinator` saga. Translates profile policy, owns binding/reservation/dispatch/ResultDraft/terminal/reconciliation/final commit composition over `lancet-api`, `lancet-audit`, and `lattice-session`. |
| `lattice-tui` | Full-screen TUI shell over `lattice-api` sessions and `stream_turn` (ratatui/crossterm, alternate screen). Owns terminal lifecycle, input state, transcript/status rendering, and the forthcoming transcript card renderer (#116) as this crate's `render/` module; `lattice-cli` consumes it through the existing `lattice-cli -> lattice-tui` edge. |
| `lattice-cli` | Binary entry point and argument dispatch over API/session/model/audit commands plus the `lattice-tui` presentation crate. |

`lattice-store` landed in #143 and appears in the graph and table above; its
downstream edges (`lattice-session`, `lancet-runtime`) arrive with the #143
store migrations. `lattice-conversation` landed in #142.
## CEP And LANCET Contracts

- `lancet-protocol` is the bottom contract. Keep it data-only and runtime-free.
- The production native catalog is empty after #32. Source Tool manifests are
  the sole descriptor authority for all five built-ins; `lancet-catalog` remains
  only as transitional generic/scaffold machinery until ADR 0004 removes it.
- The ADR 0004 target makes each Source Tool manifest the sole Tool Descriptor
  authority. Build tooling generates WIT bindings, host metadata, LLM schemas,
  embedded Component artifacts, and `inventory` registrations from it. Missing,
  duplicate, unapproved, descriptor-drifted, or import-drifted registrations
  fail closed.
- A manifest's optional `[render]` section declares the presentation intent of
  its own fields, and `cargo xtask tool-codegen` projects it to
  `generated/render.json` (GitHub #113). That file is a projection, a sibling of
  `llm-projection.json`: it is never part of `ToolDescriptor`,
  `descriptor.json`, `descriptor-sha256`, or `expected-identity.json`, because
  manifest → descriptor → expected-identity → host composition is one
  digest-pinned identity chain and presentation must not be able to invalidate
  a Tool identity, an artifact attestation, or a composition pin. Annotations
  bind fields only — they select fields, partition by discriminant value, and
  declare a field's domain semantics; they never carry rows, columns,
  positions, or nesting, because the presentation primitives belong to the
  consumer. A Tool without `[render]` is still renderable and produces no
  `render.json`; that absence is a legal state, and `--check` fails on a stale
  one.
- The default distribution contains five embedded WASM Components:
  `observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`, and
  `session.todo_write`. Their manifests under `tools/*/manifest.toml` are the
  sole descriptor authorities. `lancet_catalog::catalog()` is empty after #32.
  A name present in both the Component and native vocabularies still fails the
  server run closed (`validate_no_component_native_collision`). A Tool
  manifest's required `[tool] distribution` field decides whether
  `cargo xtask tool-codegen` embeds it in the generated host composition
  (`crates/lancet-runtime/src/generated/host_composition.rs`) — that generated
  file is the only mechanism that links a Tool into a built runtime. Operators may install
  additional precompiled Tool Bundles and automatically rebuild a new
  versioned Linux host; a running process cannot load more code or change its
  closed catalog. Audit read/replay remains operator-only.
- Every Tool, including built-ins, executes as a WASM Component with no ambient
  WASI filesystem, network, or process authority. WIT Tool Host imports are the
  only capability surface. Native binaries, locally compiled third-party Cargo
  source, runtime plugins, user processes, `exec.command_run`, and
  post-mutation verify are not Tool capabilities.
- `fetch.web` reaches the network only through `lancet-runtime`'s async
  `network-fetch` import. The host enforces GET-only policy, DNS resolution and
  pinned connection, TLS verification, redirect/header/body limits, cancellation,
  the shared observation budget, and per-hop content-free network evidence.
- Mutating methods must pass authority policy, Guard L1, effect checks and audit
  recording. The concrete guarded Local Workspace is host-enforced: Components
  cannot bypass Local Revision checks, Mutation Preconditions, atomic
  installation, or history recording. The transitional native handler path is
  migration residue. Write-scope is a blacklist (deny list), not an allowlist
  envelope; there is no envelope to declare. System exclusions for project Git
  administration, LATTICE private state, and root escape are not
  profile-configurable.
- Workspace observation exposes tracked and non-ignored Local files by default.
  Explicit ignored-path inclusion and read denial live in
  `[lancet.workspace]`; system exclusions take precedence over both and the
  same visibility decision applies to read, list, grep, and find. Since #30 that
  decision is made in exactly one place — `lancet_runtime::host_workspace`, behind
  the `workspace-observe` Tool Host imports — and the `observe.workspace`
  Component reaches the filesystem only through them. A hidden path is reported
  as missing, with the `HiddenReason` audit label recorded as evidence and never
  returned to the guest.
- LANCET policy is configured through profile `[lancet]` sections: mode,
  project root, observation, network, effects, workspace policy, and external
  policy file pointers. Sandbox `[workspace]`, command, isolation, and verify
  configuration are retired surface; do not reintroduce them.
- Cell evidence is persisted directly in each terminal `InvocationStore` record
  under the typed `InvocationId`. SQLite evidence tables, `evidence_id`, numeric
  call/audit IDs, metadata pointers, and compatibility import paths are deleted;
  do not recreate them or add a second record shape beside
  `lancet_protocol::evidence`.
- Conversation state lives in global `sessions.git`; invocation lifecycle/evidence
  lives in project-keyed `invocations.git`; workspace history lives in its own
  project-keyed Git store. They have separate locks, refs, schemas, and failure
  domains. All stores open strictly; there are no SQLite or mutable sidecar
  production paths, dual writes, or legacy importers.
- Operator audit read (query/tail/replay) is implemented in `lancet-audit`
  and only re-exported by `lancet-api`; it is never an Agent Tool. Keep
  `lancet-audit` free of `lancet-native`/`lancet-dispatch`/`lancet-cep`/
  `lancet-runtime` deps — that absence is what makes "an operator read never
  composes a CEP server" a structural guarantee rather than an assertion.
- The legacy effect vocabulary (`ImpactKind`, `EffectScopeKind`,
  `OverlayBehavior`, `FailureSemantics`, `BaseAuthorityRequirement`) is deleted
  only when **all four** hold: (1) native methods migrated to Component Tools;
  (2) structured Cell evidence is produced only through the versioned evidence
  path, with legacy records kept read-only; (3) profile `[lancet.effects]` replaced by
  `[lancet.capabilities]` for every consumer; (4) `lancet-cep`/`lancet-catalog`
  retired per ADR 0004. Until then the `lancet-native` transport dispatch split
  is the single seam between the two vocabularies, and no new consumer of the
  legacy vocabulary may be added — a review rule, not an enforced one. Full
  record, including the recorded exception and the Component/native
  name-collision hazard: `docs/specs/2026-07-26-issue28-cell-admission-design.md`
  §11.
- Explicit `/rewind` reverses only LATTICE mutations recorded after the target
  Turn, preserves unrelated external changes, and conflicts on a changed
  mutation path. Store that history in a private bare gix repository, never the
  user's project `.git`; restore Git tree state and move workspace/session refs.
  Keep prior object IDs reachable through internal refs without compensation
  commits or additional user-facing command surface.
- That history landed in GitHub #63: `lancet_runtime::WorkspaceHistory` records
  every successfully installed Atomic Local Mutation, and a completed
  `TurnCommit` carries the `workspace_history_oid` the Turn arrived at. Do not
  add a second mutation-history store.
- Rewind itself landed in GitHub #64: `lattice_api::rewind_to_turn` resolves the
  target Turn in the current session, `lancet_api::rewind_workspace` plans the
  reversal and verifies every affected path's recorded post-state before any
  write, `lancet_native::rewind` restores parent-tree content through the same
  System Path Exclusion / safe resolution / atomic installation the mutation
  path uses, and only then do the private workspace-history ref and the Session
  History ref move. A same-path divergence is a structured conflict with nothing
  moved; a ref durability failure is never reported as success. The user-facing
  surface is exactly one command, `lattice session rewind`.
- Since #139-B Commit 3 Rewind audit records live on append-only Git chains,
  not SQLite rows: `lancet_runtime::rewind_audit` owns `RewindAuditRecord`, each
  typed Session's `refs/lattice/rewind-audit/sessions/<canonical-id>` first-parent
  chain inside `workspace-history.git`, and bounded completeness-aware reads. The exclusive
  `WorkspaceOperationLock` is acquired exactly once by
  `lattice_api::rewind_to_turn_with_barriers` (the RewindCoordinator) and passed
  down as `HeldWorkspaceOperation`; lower layers never reacquire it. A failed
  audit append returns the typed `RewindError::AuditPublicationFailed` partial
  state and is never reported as success.

## Turn Execution

1. `lattice-api::stream_turn` is the single live execution core.
2. `run_turn` drains the same stream through `RunTurnCollector`.
3. `turn/cep.rs` translates profile `[lancet]` policy into LANCET-native config
   rooted at the canonical Local Workspace and delegates runtime composition to
   `lancet_api::LancetRuntime::build`, which returns the bound `CepClient` and
   frozen Tool catalog.
4. `TurnCoordinator` and `turn/saga.rs` own the durable live path: one model
   attempt at a time, binding and reservation, sequential Tool dispatch, exact
   ResultDraft/terminal publication, typed closure, and final Turn commit.
5. During migration native handlers execute typed CEP methods. The ADR 0004
   target executes embedded Tool Components behind WIT Tool Host imports;
   host-side policy, Guard L1, effect, audit and concrete Local Workspace checks
   remain authoritative.
6. Turn outcomes are persisted as commits in the session graph.

Session commits never imply a Local Workspace snapshot. GitHub #63 records each
successfully installed Atomic Local Mutation in the private gix history and
stamps the resulting object id on the Turn commit; only the explicit `/rewind`
path (GitHub #64) restores from it.

ADR 0005's ordered target replaces the monolithic production loop only in
#139-B: `lattice-api::TurnCoordinator` owns the full-Turn session execution
lock, PendingTurn journal, binding plan, batch reservation, Tool dispatch,
ResultDraft, terminal finalization, reconciliation, and final Turn commit.
`lattice-agent` then drives one model step and owns no store, InvocationId
allocation, Tool execution, or Turn commit. Reopen only completes closure from
durable facts; it never resends a provider request or redispatches a Tool.
There is no Standalone/direct CEP product invocation contract, and a JSON-RPC
notification cannot create an invocation.

## Review Records

- `FIX.md` is the append-only changelog for applied fixes. Never delete or
  overwrite old entries.
- Add a `FIX.md` entry only after the fix has landed. Include issue id/severity,
  problem, fix summary, verification commands/results, and changed files when
  practical.
- `FINDINGS.md` tracks open, deferred, partial, or residual issues. Every open
  entry should include severity/id, location, problem, current status, and plan.
- When fixing a finding, record the applied fix in `FIX.md` and remove it from
  the open section of `FINDINGS.md` or move it to an obsolete/closed section.
- For partial fixes, record the completed work in `FIX.md` and keep the
  remaining risk in `FINDINGS.md` with an explicit residual status and plan.

## Repository Hygiene

- New Rust source files under `crates/**` must start with the workspace copyright
  and SPDX header:

  ```rust
  // Copyright 2026 Ast Rin
  // SPDX-License-Identifier: Apache-2.0
  ```

- Preserve existing copyright and SPDX headers when editing source files.
- Do not add or alter headers for generated, vendored, or external files unless
  the surrounding maintained source already uses that convention.
- Do not modify `.gitignore` in agent-driven changes. In particular, do not add
  ignore exceptions or whitelist rules to force ignored private/planning/build
  paths into commits. If a path appears to need tracking despite ignore rules,
  stop and ask a human maintainer to change the policy explicitly.
- Adding or removing a crate also updates the prose crate count in `README.md`,
  plus the README layer graph and crate table. Run `cargo test --workspace` —
  what GitHub CI runs — before pushing any change that touches the crate set.

## Ownership Rules

- Start host wire/effect vocabulary changes in `lancet-protocol`. Start Tool
  method/contract changes in the Source Tool manifest, then regenerate WIT,
  host metadata, projection, and inventory artifacts; never hand-edit a
  parallel central schema.
- Keep provider protocol details in `lattice-llm`; higher layers should consume
  typed events and errors.
- Keep shared URL/DNS safety primitives and neutral errors in `net-guard`;
  policy and HTTP client construction stay in their owning crates.
- The concrete Local Workspace lives in `lancet-runtime` (`src/workspace/`) since
  #30: Tool Host imports execute observation (`workspace-observe`) and mutation
  (`workspace-mutate` since #31), and a host import cannot reach up a layer into
  `lancet-native`. Rewind and the residual hashline apply engine still consume
  the same concrete type from there — one module, not a copy. Network execution
  also lives in `lancet-runtime` after #32; `lancet-native` only constructs the
  per-call host and owns the transitional server dispatch.
  Keep secure path resolution, bounded observation, Mutation Preconditions,
  atomic installation, and history behind one concrete host module; do not
  introduce a pluggable execution-backend seam. Reaching the module across a
  crate boundary is not such a seam — there is no trait behind it and nothing
  to substitute.
- Keep LANCET runtime composition and its public surface in `lancet-api`; it
  takes LANCET-native policy types and must not depend on LATTICE
  session/agent/profile crates. Callers that drive lower-level CEP tests must use
  `lancet_api::LancetServer`/`LancetRuntime` with explicit durable bindings; only
  `lattice-api` initiates production invocations.
- Keep public session and Turn API contracts in `lattice-api`; it owns
  profile→policy translation and the `TurnCoordinator` saga over PendingTurn,
  InvocationStore, and LANCET. `lattice-agent::ModelStepDriver` performs one
  provider attempt and owns no store, Tool dispatch, or Turn commit. Keep TUI
  terminal lifecycle, input state, and rendering in `lattice-tui`. Keep CLI
  output, non-interactive rendering, and argument parsing in `lattice-cli`.
  Exception: shared transcript rendering sinks into `lattice-tui` (as the
  `render/` module, #116), because `lattice-tui` cannot depend on `lattice-cli`
  and shared presentation can only move downward. This also closes the #116
  acceptance / #101 C5 boundary-drift note.
- `lattice-workspace` was deleted in GitHub #62. Do not reintroduce an Upper/MSB
  workspace boundary, snapshot lifecycle, or a pluggable execution-backend
  seam in any crate.
- Prefer crate-local `AGENTS.md` files for detailed navigation before editing.
