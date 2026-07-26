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

```text
Layer 0 (leaf):  lancet-protocol   net-guard   lattice-llm   lattice-session
Layer 1:         lancet-catalog(->protocol)
                 lattice-profile(->protocol)
                 lattice-agent(->llm,session)
Layer 2:         lancet-dispatch(->protocol,catalog)
                 lancet-audit(->protocol,catalog)
                 lancet-authority(->protocol,catalog,net-guard)
Layer 2.5:       lancet-cep(->protocol,catalog,dispatch)
                 lancet-runtime(->protocol,dispatch)
Layer 3:         lancet-native(->protocol,catalog,dispatch,audit,
                               authority,net-guard,runtime)
Layer 3.5:       lancet-api(->cep,protocol,catalog,dispatch,audit,
                            authority,native,runtime)
Layer 4:         lattice-api(-> lancet-api + lattice runtime crates)
Layer 5:         lattice-tui(-> api + presentation dependencies)
                 lattice-cli(-> api,tui + presentation dependencies)
```

## Crate Roles

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contracts: JSON-RPC frames, errors, policy hook types, effect types, provenance wire types. No I/O and no tokio. |
| `lancet-catalog` | Legacy central Tool-contract authority scheduled for removal by ADR 0004 after source manifests and generated descriptors take over. |
| `lancet-dispatch` | Current generic registry/transport surface scheduled for absorption into `lancet-runtime`. |
| `lancet-cep` | Current CEP/catalog projection facade scheduled for absorption or retirement after descriptor-generated projection lands. |
| `lancet-audit` | CEP audit persistence, query filters, stage/impact labels, provenance projection, and attestation persistence. |
| `lancet-authority` | Policy semantics, capability matching, Guard L1 path/scope checks, effect ledger, and effect-region checks. |
| `lancet-native` | Current native handler/server surface. ADR 0004 moves built-ins to Components and absorbs the remaining concrete host responsibilities into `lancet-runtime`. |
| `lancet-runtime` | ADR 0004 target: Component engine, generated descriptor inventory, dispatch/transport, Tool Host enforcement, concrete Local Workspace, native network/session channels, audit/effect integration, and private gix history. Not yet the production path. |
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
- The default distribution contains `observe.workspace`, `mutate.create`,
  `mutate.edit`, `fetch.web`, and `session.todo_write`. Operators may install
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
  same visibility decision applies to read, list, grep, and find.
- LANCET policy is configured through profile `[lancet]` sections: mode,
  project root, observation, network, effects, workspace policy, and external
  policy file pointers. Sandbox `[workspace]`, command, isolation, and verify
  configuration are retired surface; do not reintroduce them.
- SQLite for CEP audit records belongs in `lancet-audit`; the conversation
  graph is git-backed (git refs under `refs/lattice/sessions/<id>/**`) and
  lives in `lattice-session`. SQLite session graph storage was retired.
- Explicit `/rewind` reverses only LATTICE mutations recorded after the target
  Turn, preserves unrelated external changes, and conflicts on a changed
  mutation path. Store that history in a private bare gix repository, never the
  user's project `.git`; restore Git tree state and move workspace/session refs.
  Keep prior object IDs reachable through internal refs without compensation
  commits or additional user-facing command surface.
- That history landed in GitHub #63: `lancet_runtime::WorkspaceHistory` records
  every successfully installed Atomic Local Mutation, and a completed
  `TurnCommit` carries the `workspace_history_oid` the Turn arrived at. #64
  builds Rewind on top of it; do not add a second mutation-history store.

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
- During Direct-Local migration keep concrete Local filesystem, network,
  mutation, and transport behavior in `lancet-native`. ADR 0004 then absorbs
  those host responsibilities into `lancet-runtime`. At every stage keep secure
  path resolution, bounded observation, Mutation Preconditions, atomic
  installation, and history behind one concrete host module; do not preserve a
  pluggable execution-backend seam.
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
