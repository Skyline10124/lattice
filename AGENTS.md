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
authority, runtime code loading, user-process, or arbitrary-command
capabilities while completing the migration.
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
Layer 0 (leaf):  lancet-protocol   lattice-session
Layer 0.5:       net-guard                       (no workspace deps; see note)
                 lattice-llm(->net-guard)
Layer 1:         lancet-catalog(->protocol)
                 lattice-profile(->protocol)
                 lattice-agent(->llm,session)
Layer 2:         lancet-dispatch(->protocol,catalog)
                 lancet-audit(->protocol,catalog)
                 lancet-authority(->protocol,catalog,net-guard)
Layer 2.5:       lancet-cep(->protocol,catalog,dispatch)
                 lancet-runtime(->protocol,dispatch,authority)
Layer 3:         lancet-native(->protocol,catalog,dispatch,audit,
                               authority,net-guard,runtime)
Layer 3.5:       lancet-api(->protocol,catalog,dispatch,cep,audit,
                            authority,native,runtime)
Layer 4:         lattice-api(->lancet-api,lancet-protocol,lancet-catalog,
                             lancet-authority,lancet-native,
                             lattice-agent,lattice-llm,lattice-profile,
                             lattice-session)
Layer 5:         lattice-tui(->lattice-api + presentation dependencies)
                 lattice-cli(->lattice-api,lattice-session,lattice-tui,
                              lancet-api + presentation dependencies)

Not in the graph: lattice-tool-prelude and lattice-system-harness have no
workspace-internal dependencies by contract. The prelude is a guest SDK
(`wit-bindgen` only); the harness must stay free of product crates so it can
only reach the product through the built binary (`cargo xtask
harness-boundary` enforces this, transitively).
```

`lancet-runtime -> lancet-authority` arrived with #30, when the concrete Local
Workspace moved down into `lancet-runtime`: the visibility engine's decision
vocabulary (`ReadVisibilityPolicy` / `HiddenReason` / `ReadVisibility`) and the
System Path Exclusion predicate are policy *semantics* and stay in
`lancet-authority`, so the runtime consumes them rather than reimplementing them.
The edge points down (2.5 -> 2) and closes no cycle.

`net-guard` and `lattice-llm` sit at 0.5 rather than 0 because `lattice-llm`
depends on `net-guard`. Only `net-guard` is a true leaf of that pair;
`lancet-protocol` and `lattice-session` are the crates with no
workspace-internal dependencies at all.

`lattice-api` and `lattice-cli` reach past the `lancet-api` facade into
individual LANCET crates. Those edges point downward, so they are not layering
violations, but they are load-bearing: `lattice-api` owns the profile→policy
translation and needs policy types (`lancet-authority`) and error/impact
mapping (`lancet-native`) that `lancet-api` does not re-export, and
`lattice-cli` drives operator audit commands through `lancet-api` directly.
Prefer naming a type through `lancet-api` when it re-exports it; do not add new
crates to these edge sets without a reason recorded here.

Note that only one edge in this graph is machine-checked today
(`lattice-agent -> lancet-native`, by `cargo xtask lancet-boundary`). GitHub
#108 tracks turning this diagram into gate data so drift fails CI instead of
accumulating.

## Crate Roles

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contracts: JSON-RPC frames, errors, policy hook types, effect types, provenance wire types. No I/O and no tokio. |
| `lancet-catalog` | Legacy central Tool-contract authority scheduled for removal by ADR 0004 after source manifests and generated descriptors take over. |
| `lancet-dispatch` | Current generic registry/transport surface scheduled for absorption into `lancet-runtime`. |
| `lancet-cep` | Current CEP/catalog projection facade scheduled for absorption or retirement after descriptor-generated projection lands. |
| `lancet-audit` | CEP audit persistence, query filters, stage/impact labels, provenance projection, attestation persistence, and (since #112) the versioned Cell evidence tables that are the durable home of `AdmissionEvidence` / `ObservedEffectsRecord` / `DeniedOperationsRecord`. |
| `lancet-authority` | Policy semantics, capability matching, Guard L1 path/scope checks, effect ledger, and effect-region checks. |
| `lancet-native` | Current native handler/server surface for the method still native (`fetch.web`), plus the CEP server lifecycle and the Component/native dispatch split. ADR 0004 moves the remaining built-ins to Components and absorbs the rest into `lancet-runtime`. |
| `lancet-runtime` | Component engine, generated descriptor inventory, Tool Host enforcement, the concrete Local Workspace and its Policy-Visible visibility engine (moved down here in #30), audit/effect integration, and private gix history. The production Component path since #29; #31/#32 finish absorbing dispatch/transport. |
| `lattice-tool-prelude` | ADR 0004 target guest SDK for source manifests, generated WIT bindings, descriptor helpers, and built-in Source Tools. |
| `lancet-api` | Unified public API. During migration it composes native CEP pieces; the target composes `lancet-runtime` without depending on LATTICE session/agent/profile layers. |
| `net-guard` | Neutral URL/DNS safety leaf: base-URL validation, public-address resolution, private/reserved IP checks, and rebinding-domain checks. Depends only on `url` and `std`. |
| `lattice-llm` | Provider adapters, SSE streaming, model routing, tool-call vocabulary, HTTP request pinning, and logging. |
| `lattice-session` | Session graph contract, immutable turn commits, branch refs, HEAD state, and the git-backed graph store (`GitSessionGraphStore`). |
| `lattice-profile` | Strict `agent.toml` schema, discovery, defaults, and LANCET profile sections. |
| `lattice-agent` | Multi-turn `AgentLoop`, state machine, message assembly, and injectable LLM/tool traits. |
| `lattice-api` | Thin public facade for session creation/opening and turn execution. Translates profile `[lancet]` policy into LANCET-native config and wires the agent loop over `lancet-api`'s CEP runtime. |
| `lattice-tui` | Inline CLI chat REPL over `lattice-api` sessions and `stream_turn` via `rustyline` (no alternate screen, no ratatui/crossterm). Owns readline state, turn execution, and token/status rendering to stdout/stderr. |
| `lattice-cli` | Binary entry point and argument dispatch over API/session/model/audit commands plus the `lattice-tui` presentation crate. |

## CEP And LANCET Contracts

- `lancet-protocol` is the bottom contract. Keep it data-only and runtime-free.
- Until ADR 0004 lands, `lancet-catalog` and `lancet-cep` remain the production
  contract/projection path. Do not create a third parallel Tool schema during
  migration.
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
- The default distribution contains `observe.workspace`, `mutate.create`,
  `mutate.edit`, `fetch.web`, and `session.todo_write`. Four of those are embedded
  WASM Components whose sole descriptor authority is their source manifest:
  `session.todo_write` since GitHub #29 (`tools/todo-write/manifest.toml`),
  `observe.workspace` since GitHub #30 (`tools/observe-workspace/manifest.toml`),
  and `mutate.create` / `mutate.edit` since GitHub #31 (`tools/mutate-*/manifest.toml`).
  The remaining native catalog method is `fetch.web`, so `lancet_catalog::catalog()`
  is one entry, not five. Removing a migrated method from that catalog is not
  bookkeeping: a name present in both vocabularies fails the whole server run
  closed (`validate_no_component_native_collision`). A Tool
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
- Cell evidence is persisted in `lancet-audit`'s versioned evidence tables
  (`evidence_calls` / `evidence_effects` / `evidence_denials`), keyed by a
  store-generated `evidence_id` that a CEP audit row merely points at. That
  identity is deliberately not `audit_id`: ADR 0004 decision 8 deletes the audit
  store, and the evidence identity must outlive it. Do not put structured
  evidence back into `metadata_json`, and do not add a second record shape
  beside `lancet_protocol::evidence`.
- SQLite for CEP audit records belongs in `lancet-audit`; the conversation
  graph is git-backed (git refs under `refs/lattice/sessions/<id>/**`) and
  lives in `lattice-session`. SQLite session graph storage was retired.
- Operator audit read (query/tail/replay) is implemented in `lancet-audit`
  and only re-exported by `lancet-api`; it is never an Agent Tool. Keep
  `lancet-audit` free of `lancet-native`/`lancet-dispatch`/`lancet-cep`/
  `lancet-runtime` deps — that absence is what makes "an operator read never
  composes a CEP server" a structural guarantee rather than an assertion.
- The legacy effect vocabulary (`ImpactKind`, `EffectScopeKind`,
  `OverlayBehavior`, `FailureSemantics`, `BaseAuthorityRequirement`) is deleted
  only when **all four** hold: (1) native methods migrated to Component Tools;
  (2) audit rows and attestations produced only through the v2 evidence path,
  v1 kept read-only; (3) profile `[lancet.effects]` replaced by
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

## Turn Execution

1. `lattice-api::stream_turn` is the single live execution core.
2. `run_turn` drains the same stream through `RunTurnCollector`.
3. `turn/cep.rs` translates profile `[lancet]` policy into LANCET-native config
   rooted at the canonical Local Workspace and delegates runtime composition to
   `lancet_api::LancetRuntime::build` (native Local operations, server
   lifecycle, dispatch client, audit), then wraps the returned `CepClient` in a
   `CepExecutorAdapter`.
4. `AgentLoop` reads session state, assembles messages, calls the LLM through
   `LlmClient`, and sends requested tool calls through the injected executor.
5. During migration native handlers execute typed CEP methods. The ADR 0004
   target executes embedded Tool Components behind WIT Tool Host imports;
   host-side policy, Guard L1, effect, audit and concrete Local Workspace checks
   remain authoritative.
6. Turn outcomes are persisted as commits in the session graph.

Session commits never imply a Local Workspace snapshot. GitHub #63 records each
successfully installed Atomic Local Mutation in the private gix history and
stamps the resulting object id on the Turn commit; only the explicit `/rewind`
path (GitHub #64) restores from it.

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
  the same concrete type from there — one module, not a copy. Keep network and
  transport behavior in `lancet-native` until #32 absorbs them. At every stage
  keep secure path resolution, bounded observation, Mutation Preconditions,
  atomic installation, and history behind one concrete host module; do not
  introduce a pluggable execution-backend seam. Reaching the module across a
  crate boundary is not such a seam — there is no trait behind it and nothing
  to substitute.
- Keep LANCET runtime composition and its public surface in `lancet-api`; it
  takes LANCET-native policy types and must not depend on LATTICE
  session/agent/profile crates. Anything that drives CEP (CLI, tests, embedders)
  should spawn through `lancet_api::LancetServer`/`LancetRuntime`, not hand-roll
  the registry/transport/server dance.
- Keep public session and turn API contracts in `lattice-api`; it owns the
  profile→policy translation and the agent-loop wiring over `lancet-api`. Keep
  TUI terminal lifecycle, input state, and rendering in `lattice-tui`. Keep CLI
  output, non-interactive rendering, and argument parsing in `lattice-cli`.
- `lattice-workspace` was deleted in GitHub #62. Do not reintroduce an Upper/MSB
  workspace boundary, snapshot lifecycle, or a pluggable execution-backend
  seam in any crate.
- Prefer crate-local `AGENTS.md` files for detailed navigation before editing.
