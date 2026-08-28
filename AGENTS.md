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
git-style session graph, public Turn execution is exposed through `lattice-api`,
and Tool execution flows through LANCET's closed, reservation-gated WASM
Component runtime with host-enforced Cell capabilities, path guards, evidence,
and an audit trail.

ADR 0003 is the accepted workspace architecture: structured LANCET Tools
operate directly on the Local Workspace. ADR 0004 is the accepted Tool
architecture: every built runtime has a closed catalog of embedded WASM
Components whose Source Tool manifests own their descriptors. The default
distribution contains five Tools; operators may install precompiled Tool
Bundles only by rebuilding a new closed runtime. There is one generated
Component execution path in `lancet-runtime`; the central catalog, generic
dispatch, CEP facade, and native server crates are retired. Keep runtime code
loading, user processes, arbitrary commands, ambient Tool authority, workspace
projection, and checkpoints out of the architecture. ADR 0005's PendingTurn is
a conversation recovery journal, not a workspace checkpoint.

ADR 0005 is the accepted durable Turn and invocation target. It introduces one
typed `InvocationId`, a data-only `lattice-conversation` contract, a generic
private-Git `lattice-store` kernel, durable PendingTurn reconciliation, and a
`lattice-api::TurnCoordinator`. Session, invocation/evidence/operator, and
workspace-history semantics remain three separate domains in
`lattice-session`, `lancet-audit`, and `lancet-runtime`; sharing Git mechanics
does not merge their repositories, locks, state machines, or failure domains.
The #141–#144 and #139-A/#139-B program is implemented; this repository uses
the production hard cut with no compatibility dual writes or legacy import
paths. Preserve the correctness, strict-reopen, bounded-read, and cold-read
structural regressions.

The performance campaign is temporarily diagnostic-only. Its metric definitions
remain in `benchmarks/invocation-store/qualification-contract-v2.json`, and the
current decision record is
`docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`. The official
runner writes ignored local bundles below `target/invocation-store-bundles` and
uses `benchmark`; strict `qualify` is retained for the redesign tracked by
GitHub #153. Historical PASS/FAIL summaries remain evidence, not a prerequisite
for #139-B, and must not be rewritten or presented as a fresh approval campaign.

The workspace dependency graph is layered. Keep dependencies pointing from
higher layers to lower layers; do not introduce reverse dependencies. Every
edge below is a real normal `workspace = true` dependency; verify changes with
`cargo metadata`.

```text
Layer 0 (leaf):  lancet-protocol    lattice-store    net-guard
Layer 1:         lattice-conversation(->protocol)
                 lattice-profile(->protocol)
                 lancet-authority(->protocol,net-guard)
                 lancet-audit(->protocol,lattice-store)
Layer 2:         lattice-llm(->net-guard,conversation)
                 lattice-session(->protocol,conversation,lattice-store)
                 lancet-runtime(->protocol,authority,net-guard,lattice-store)
Layer 3:         lattice-agent(->llm,conversation)
                 lancet-api(->protocol,audit,authority,runtime)
Layer 4:         lattice-api(->lancet-api,protocol,audit,authority,agent,
                             conversation,llm,profile,session)
Layer 5:         lattice-tui(->lattice-api)
                 lattice-cli(->lancet-api,lattice-api,session,tui)

Outside the product graph: lattice-tool-prelude and the Source Tool packages,
mutate-shared, xtask, and lattice-system-harness. The harness must stay free of
product crates so it reaches the product only through the built binary;
`cargo xtask harness-boundary` enforces that transitively.
```

There is no `lancet-audit -> lattice-session` edge, and none may be added.
`lancet-audit` owns the invocation ledger and returns audit-domain types only.
The production projection of a pinned ledger view into
`lattice_session::pending::InvocationFactsSnapshot` lives in
`lattice-api/src/turn/invocation_facts.rs`; the qualification-only projection
lives in `crates/lancet-audit/tests/invocation_qualification.rs`.
`lattice-session` must not depend on `lancet-audit`.

`lattice-store` owns repository creation/opening, canonical bytes, object/ref
CAS, Linux flock/fsync/rename durability, retention, and typed store errors.
It must not absorb session, runtime, provider, or audit schemas. State below
`$XDG_STATE_HOME/lattice` consists of global `sessions.git` plus project-keyed
`invocations.git` and `workspace-history.git`. `ProjectStoreKey` hashes the
canonical Linux path bytes of the full Local Workspace root. Code must never
hold two repository write locks simultaneously.

`lattice-api` reaches through `lancet-api` to `lancet-audit` and
`lancet-authority` because it owns pinned-facts projection and profile-to-policy
translation. `lattice-cli` reaches `lancet-api` for operator audit commands.
Prefer a `lancet-api` re-export when one exists; record any new edge here.

`cargo xtask lancet-boundary` checks the surviving dependency boundaries and
production/test feature separation. Use one-time `cargo metadata`, `cargo tree`,
and source-reference review as deletion evidence; do not add tombstone gates for
retired crates or APIs.
## Crate Roles

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure wire/data contracts: durable identities/bindings, JSON-RPC frames, typed errors, Cell descriptors/scopes, and versioned evidence. No I/O or async runtime. |
| `lancet-audit` | Production project-keyed Git `InvocationStore`, exact reservation/admission lifecycle, terminal material and Cell evidence, pinned views, and typed operator query/tail/replay DTOs. Its public surface is audit-domain only; `lattice-api` projects pinned ledger facts into Session reconciliation types. |
| `lancet-authority` | Cell capability-admission support, Guard L1 path checks, network policy, workspace visibility policy, and System Path Exclusion semantics. |
| `lancet-runtime` | Sole generated-Component execution path: closed catalog/provider projection, reservation-bound call controls and ordered events, Tool Host enforcement, concrete Local Workspace, GET-only pinned network transport, Cell evidence, and private Git history. |
| `lattice-tool-prelude` | Guest SDK for source manifests, generated WIT bindings, descriptor helpers, and built-in Source Tools. |
| `lancet-api` | Thin LANCET facade over `lancet-runtime` Turn composition and `lancet-audit` operator reads. It has no LATTICE session/agent/profile dependencies. |
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

## LANCET Contracts

- `lancet-protocol` is the bottom contract. Keep it data-only and runtime-free.
- Source Tool manifests are the sole Tool Descriptor authority. Build tooling
  generates WIT bindings, host metadata, LLM schemas, embedded Component
  artifacts, and inventory registrations. Missing, duplicate, unapproved,
  descriptor-drifted, or import-drifted registrations fail closed.
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
  sole descriptor authorities. A Tool
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
  `network-fetch` import. The host enforces GET-only policy, complete special-address
  rejection, DNS resolution and pinned connection, TLS verification, redirect/header/body
  limits, cancellation, and the shared observation budget. Per-hop evidence excludes
  userinfo/query/fragment, distinguishes received bytes from guest-delivered bytes, and
  records post-I/O failures as effects rather than pre-I/O denials.
- Mutating Tools must pass Cell capability admission, Guard L1, and audit
  recording. The concrete Local Workspace is host-enforced: Components cannot
  bypass Local Revision checks, Mutation Preconditions, atomic installation, or
  history recording. Write-scope is a blacklist (deny list), not an allowlist
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
- LANCET policy is configured through profile `[lancet]` sections: project
  root, observation, network, capabilities, workspace policy, and external
  policy file pointers. Retired execution modes and effect profiles are rejected.
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
  `lancet-audit` free of `lancet-runtime` dependencies; that absence makes
  "an operator read never composes a Tool runtime" a structural guarantee.
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
- Rewind resolves the target Turn in `lattice-api`, plans the reversal through
  `lancet-api`, and restores parent-tree content in `lancet-runtime` through the
  same System Path Exclusion, safe resolution, and atomic installation used by
  mutation. Only after successful restore do the private workspace-history ref
  and Session History ref move. The user-facing surface is `lattice session
  rewind`.
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
3. `turn/cep.rs` translates profile `[lancet]` policy into runtime policy rooted
   at the canonical Local Workspace and calls `lancet_api::LancetRuntime::build`,
   which returns the closed generated Tool catalog and ordered Tool-event stream.
4. `TurnCoordinator` and `turn/saga.rs` own the durable live path: one model
   attempt at a time, binding and reservation, sequential Component Tool
   execution, exact ResultDraft/terminal publication, typed closure, and final
   Turn commit.
5. Every Tool executes as an embedded Component behind Tool Host imports; host
   capability admission, Guard L1, evidence, and Local Workspace checks remain
   authoritative.
6. Turn outcomes are persisted as commits in the session graph.

Session commits never imply a Local Workspace snapshot. GitHub #63 records each
successfully installed Atomic Local Mutation in the private gix history and
stamps the resulting object id on the Turn commit; only the explicit `/rewind`
path (GitHub #64) restores from it.

`lattice-api::TurnCoordinator` owns the full-Turn session execution lock,
PendingTurn journal, binding plan, batch reservation, Tool execution,
ResultDraft, terminal finalization, reconciliation, and final Turn commit.
`lattice-agent` drives one model step and owns no store, InvocationId allocation,
Tool execution, or Turn commit. Reopen only completes closure from durable
facts; it never resends a provider request or redispatches a Tool. There is no
standalone/direct product invocation contract.

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
- `INFO.md` captures non-blocking informational review notes. Move stale entries
  to an obsolete section when referenced files or subsystems are retired.

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

- Start shared wire/evidence vocabulary changes in `lancet-protocol`. Start Tool
  contract changes in the Source Tool manifest, then regenerate WIT, host
  metadata, provider projection, and inventory artifacts.
- Keep provider protocol details in `lattice-llm`; higher layers should consume
  typed events and errors.
- Keep shared URL/DNS safety primitives and neutral errors in `net-guard`;
  policy and HTTP client construction stay in their owning crates.
- Keep the concrete Local Workspace in `lancet-runtime/src/workspace/`. Tool Host
  imports and Rewind share it; network execution lives in the sibling runtime
  host. Keep secure path resolution, bounded observation, Mutation Preconditions,
  atomic installation, and history behind this one concrete module. It has no
  pluggable execution-backend trait.
- Keep LANCET runtime composition and its public facade in `lancet-api`. It must
  not depend on LATTICE session/agent/profile crates. Only `lattice-api` initiates
  production invocations; lower-level tests use `LancetRuntime` with explicit
  durable bindings.
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
