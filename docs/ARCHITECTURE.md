<!-- generated-by: gsd-doc-writer -->
# LATTICE Architecture

> This document is generated from repository verification. Claims that could not
> be directly confirmed from the repo are marked with `<!-- VERIFY: ... -->`.

## 1. System overview

LATTICE is a session-centric LLM runtime written in Rust. Conversation state
lives in a **git-backed session graph**: every turn outcome is persisted as an
immutable commit under `refs/lattice/sessions/**`, so history is durable and
auditable. Public turn execution is exposed through `lattice-api`, whose single
live core is `stream_turn`. Tool execution flows through **LANCET**, a
separately-embeddable runtime that exposes the **Controlled Execution Protocol
(CEP)** — a typed JSON-RPC method protocol with direct policy gating, path
guards (Guard L1), effect checks, and an audit trail. The architecturally
significant property is the strict layering: the workspace is a layered Cargo
workspace where dependencies only ever point from higher layers to lower layers,
with no reverse dependencies, so the LANCET CEP runtime (the `lancet-*` crates)
can be embedded without pulling in the LATTICE session/agent/profile layers.

The production execution path is **Direct-Local** (ADR 0003): structured
LANCET Tools observe and mutate the canonical Local Workspace directly behind
host-enforced policy, Guard L1 path checks, effect checks, and audit. There is
no sandbox VM, workspace projection, or per-turn provisioning — the
MSB/Upper path and the `lattice-workspace` crate were removed in
#62 (Codeberg #40). The session graph store is git-backed
(`GitSessionGraphStore` over the project's `git2` object database); the
previous SQLite graph store has been removed.
## 2. Component diagram

The workspace has 19 crates grouped into layers. Arrows point from a higher
layer toward the lower-layer crates it depends on. The diagram below shows the
runtime crates; `lattice-tool-prelude` (guest SDK) and
`lattice-system-harness` (black-box tests) sit outside it by contract.
`AGENTS.md` carries the authoritative edge list.
```mermaid
graph TD
    %% Layer 5 — presentation / entry
    CLI["lattice-cli"]
    TUI["lattice-tui"]

    %% Layer 4 — LATTICE public facade
    LAPI["lattice-api"]

    %% Layer 3.5 — LANCET unified API
    NAPI["lancet-api"]

    %% Layer 3 — native handlers
    NATIVE["lancet-native"]

    %% Layer 2.5 — CEP facade
    CEP["lancet-cep"]

    %% Layer 2 — dispatch / audit / authority
    DISPATCH["lancet-dispatch"]
    AUDIT["lancet-audit"]
    AUTHORITY["lancet-authority"]

    %% Layer 1 — catalog / profile / agent
    CATALOG["lancet-catalog"]
    PROFILE["lattice-profile"]
    AGENT["lattice-agent"]

    %% Layer 0 — leaves
    PROTOCOL["lancet-protocol"]
    NETGUARD["net-guard"]
    LLM["lattice-llm"]
    SESSION["lattice-session"]
    %% Layer 5 -> 4
    CLI --> LAPI
    CLI --> TUI
    TUI --> LAPI

    %% Layer 4 -> LANCET (3.5) and LATTICE runtime crates
    LAPI --> NAPI
    LAPI --> AGENT
    LAPI --> PROFILE
    LAPI --> SESSION
    LAPI --> LLM

    %% Layer 3.5 -> 3 + 2 + 1 + 0
    NAPI --> CEP
    NAPI --> NATIVE
    NAPI --> DISPATCH
    NAPI --> AUDIT
    NAPI --> AUTHORITY
    NAPI --> CATALOG
    NAPI --> PROTOCOL

    %% Layer 3 -> 2 + 1 + 0
    NATIVE --> DISPATCH
    NATIVE --> AUDIT
    NATIVE --> AUTHORITY
    NATIVE --> CATALOG
    NATIVE --> PROTOCOL
    NATIVE --> NETGUARD
    %% Layer 2.5 -> 2 + 1 + 0
    CEP --> DISPATCH
    CEP --> CATALOG
    CEP --> PROTOCOL

    %% Layer 2 -> 1 + 0
    DISPATCH --> CATALOG
    DISPATCH --> PROTOCOL
    AUDIT --> CATALOG
    AUDIT --> PROTOCOL
    AUTHORITY --> CATALOG
    AUTHORITY --> PROTOCOL
    AUTHORITY --> NETGUARD

    %% Layer 1 -> 0
    CATALOG --> PROTOCOL
    PROFILE --> PROTOCOL
    AGENT --> LLM
    AGENT --> SESSION
    LLM --> NETGUARD
```

Layer model (verified against each crate's `Cargo.toml` `[dependencies]`):

| Layer | Crate | Depends on |
|---|---|---|
| 0 (leaf) | `lancet-protocol` | — |
| 0 (leaf) | `net-guard` | — |
| 0 (leaf) | `lattice-llm` | `net-guard` |
| 0 (leaf) | `lattice-session` | — |
| 1 | `lancet-catalog` | `lancet-protocol` |
| 1 | `lattice-profile` | `lancet-protocol` |
| 1 | `lattice-agent` | `lattice-llm`, `lattice-session` |
| 2 | `lancet-dispatch` | `lancet-protocol`, `lancet-catalog` |
| 2 | `lancet-audit` | `lancet-protocol`, `lancet-catalog` |
| 2 | `lancet-authority` | `lancet-protocol`, `lancet-catalog`, `net-guard` |
| 2.5 | `lancet-cep` | `lancet-protocol`, `lancet-catalog`, `lancet-dispatch` |
| 3 | `lancet-native` | `lancet-protocol`, `lancet-catalog`, `lancet-dispatch`, `lancet-audit`, `lancet-authority`, `net-guard` |
| 3.5 | `lancet-api` | `lancet-cep`, `lancet-protocol`, `lancet-catalog`, `lancet-dispatch`, `lancet-audit`, `lancet-authority`, `lancet-native` |
| 4 | `lattice-api` | `lancet-api` + `lattice-agent`, `lattice-profile`, `lattice-session`, `lattice-llm` |
| 5 | `lattice-tui` | `lattice-api` |
| 5 | `lattice-cli` | `lattice-api`, `lattice-tui` |

## 3. Data flow

A single turn, from user keystroke to committed session graph:

1. **User invokes the runtime.**
   - `lattice-cli` parses arguments and, for an interactive session, calls
     `lattice_tui::run`; the TUI is a `rustyline` inline REPL (no alternate
     screen) that reads lines and executes turns through
     `lattice_api::stream_turn`.
     - The TUI spawns a Ctrl-C watcher for the duration of each turn and sets
       `RunTurnRequest::cancel_flag` to cooperatively cancel.
   - Non-interactive paths call `lattice_api::stream_turn` (or `run_turn`)
     directly.

2. **`lattice-api::stream_turn` is the single live execution core.**
   - `stream_turn` (in `crates/lattice-api/src/turn/execute.rs`) builds the
     live turn stream. `run_turn` is a thin wrapper that drains the same stream
     through `RunTurnCollector` (`crates/lattice-api/src/turn/collector.rs`)
     and returns a `RunTurnResult`.

3. **`turn/cep.rs` assembles the LANCET runtime and the executor adapter.**
   - `turn/cep.rs` (`crates/lattice-api/src/turn/cep.rs`) is the
     LATTICE↔LANCET seam. It translates the profile `[lancet]` policy sections
     into LANCET-native config types and delegates runtime composition to
     `lancet_api::LancetRuntime::build` (`crates/lancet-api/src/runtime.rs`),
     which spawns the native CEP server and returns a `CepClient`.
   - The returned `CepClient` is wrapped in a `CepExecutorAdapter`, which
     implements `lattice_agent::ToolCallExecutor` and maps LLM-shape tool
     calls into typed CEP method invocations.
   - The runtime is rooted at the canonical Local Workspace (the project
     root). No sandbox VM, lease, or per-turn workspace provisioning exists;
     the #62 cleanup removed the Upper lifecycle (`prepare_upper`,
     `UpperLease`, `UpperHandle`) with the `lattice-workspace` crate.
4. **`AgentLoop` assembles messages from session state.**
   - `AgentLoop` (`crates/lattice-agent/src/agent_loop/mod.rs`) reads the
     session graph state, assembles the LLM message history
     (`agent_loop/messages.rs`), and drives the turn state machine with
     deadline/cancellation handling.

5. **`LlmClient` streams the model response.**
   - The injected `LlmClient` trait (`crates/lattice-agent/src/agent_loop/
     traits.rs`) is satisfied by a concrete provider adapter from
     `lattice-llm`, which streams `LlmEvent`s back (SSE parsing in
     `lattice-llm/src/sse.rs`).

6. **Tool calls are dispatched via `CepClient`.**
   - When the model emits tool calls, `AgentLoop` sends each through the
     `ToolCallExecutor` (the `CepExecutorAdapter`), which resolves the dotted
     method name against the catalog and calls the typed method on
     `CepClient`.
   - `CepClient` (`crates/lancet-dispatch/src/transport.rs`, re-exported by
     `lancet-native` and `lancet-api`) carries the typed JSON-RPC call over an
     in-process transport to the native CEP server.

7. **Native handlers execute behind policy / Guard L1 / effect / audit.**
   - Native handlers in `lancet-native` execute the typed CEP method
     (`observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`,
     `session.todo_write`) behind policy decisions (`lancet-authority`),
     Guard L1 path/scope checks, effect-region checks, and audit recording
     (`lancet-audit`).
   - Filesystem observation and mutation run directly on the guarded Local
     Workspace (the concrete host module in `lancet-native`), using typed
     workspace-relative paths — never shell strings. Mutation preconditions,
     atomic installation, and policy-visible reads are host-enforced.
8. **Turn outcome is committed to the git session graph.**
   - The turn outcome (messages, tool results, usage) is persisted as an
     immutable commit by `GitSessionGraphStore`
     (`crates/lattice-session/src/git_graph/mod.rs`) implementing the
     `SessionGraphStore` trait (`crates/lattice-session/src/graph.rs`).

9. **Events stream back to the caller.**
   - `TurnEvent`s stream live from `stream_turn` back to the caller (CLI/TUI),
     where tokens render to stdout and tool/status lines render to stderr.

## 4. Key abstractions

All file paths verified by reading the source.

| Abstraction | Kind | Location | Notes |
|---|---|---|---|
| `SessionGraphStore` | trait | `crates/lattice-session/src/graph.rs:390` | Backend-neutral session graph contract: commits, branch refs, HEAD, diffs. |
| `GitSessionGraphStore` | struct | `crates/lattice-session/src/git_graph/mod.rs:37` | Git-backed store over the project's `git2` object database; commits live under `refs/lattice/sessions/**`, mutable metadata in sidecar files. Replaces the removed SQLite store. |
| `AgentLoop` | struct | `crates/lattice-agent/src/agent_loop/mod.rs:99` | Multi-turn loop: turn state machine, message assembly, tool rounds, cancellation, deadlines. `AgentLoopConfig` at line 79. |
| `LlmClient` | trait | `crates/lattice-agent/src/agent_loop/traits.rs:53` | Injectable LLM client for the agent loop (chat + streaming). Concrete adapters live in `lattice-llm`; `ToolCallExecutor` (line 44) is the sibling tool-execution trait. |
| `ToolCallExecutor` | trait | `crates/lattice-agent/src/agent_loop/traits.rs:44` | Injectable tool executor; production impl is `CepExecutorAdapter`. |
| `LancetRuntime` | struct | `crates/lancet-api/src/runtime.rs:112` | Fully-composed LANCET runtime: spawns the native CEP server and hands back a `CepClient` + audit + tool definitions. Built via `LancetRuntime::build` from `LancetRuntimeConfig`. |
| `CepClient` | struct | `crates/lancet-dispatch/src/transport.rs:76` | Typed CEP JSON-RPC client over an in-process transport. Re-exported by `lancet-native` (`src/lib.rs`) and `lancet-api`. |
| `CepExecutorAdapter` | struct | `crates/lattice-api/src/turn/cep.rs:75` | Maps LLM-shape `ToolCall`s into typed CEP method invocations via `CepClient`; implements `ToolCallExecutor`. Holds catalog/enabled name sets. |
| `CATALOG` | static | `crates/lancet-catalog/src/catalog.rs:13` | `LazyLock<[MethodDescriptor; 5]>` — the authoritative production agent catalog (exactly 5 methods, see below). |
| `LocalWorkspace` | module | `crates/lancet-native/src/workspace/` | Concrete guarded Local Workspace host: secure path resolution, bounded observation, Mutation Preconditions, atomic installation. Not a pluggable seam (ADR 0003). |
| `CepError` | struct | `crates/lancet-protocol/src/error.rs:216` | Typed CEP error with `data` + `Retriability`. Wire codes in reserved JSON-RPC range `-32000..-32099`. |
| `CepErrorCode` | enum | `crates/lancet-protocol/src/error.rs:46` | Closed enum of CEP error codes, segmented by family (Validation/Permission/Execution/State/Guard/Effect). |
| `stream_turn` | fn | `crates/lattice-api/src/turn/execute.rs:259` | `pub async fn stream_turn(...) -> Result<TurnEventStream>` — the single live execution core. |
| `run_turn` | fn | `crates/lattice-api/src/turn/execute.rs:132` | `pub async fn run_turn(...) -> Result<RunTurnResult>` — drains `stream_turn` via `RunTurnCollector`. |
| `RunTurnCollector` | struct | `crates/lattice-api/src/turn/collector.rs:26` | Drains the live `TurnEventStream` into a `RunTurnResult`. |

### CEP catalog — exactly 5 agent-visible methods

The `CATALOG` static declares exactly these methods (verified at
`crates/lancet-catalog/src/catalog.rs`):

1. `observe.workspace`
2. `mutate.create`
3. `mutate.edit`
4. `fetch.web`
5. `session.todo_write`

**Operator audit (not in `CATALOG`):** query, tail, and turn replay read the
project-keyed CEP audit SQLite store through `lancet-api::{query_audit,
tail_audit, replay_audit}` (`lattice audit` CLI). Agents cannot invoke audit
reads as CEP tools.

Since GitHub #39 those three `lancet-api` entry points are one-line delegations: the
DTOs, pagination loop, filter construction, wire projection, attestation
comparison, and contract tests live in `lancet-audit`
(`src/operator.rs`, `src/operator_tests.rs`), and the external project-keyed
path helpers in `lancet-audit/src/store_path.rs` (re-exported by
`lancet-api::config`). The public `lancet_api::*` paths are unchanged. Because
`lancet-audit` sits at layer 2 and depends on none of `lancet-native`,
`lancet-dispatch`, `lancet-cep`, or `lancet-runtime`, "an operator audit read
never composes a CEP server" is a **structural crate-dependency guarantee** —
server, transport, and registry types are not nameable from the read path — not
a source-string-scan assertion. The operator DTOs that previously lived in
`lancet-catalog/src/methods/audit.rs` were deleted with that module; none became
a Tool descriptor, generated catalog entry, or Component.

There is **no** `session.envelope` method. It was removed from the catalog;
`crates/lattice-api/src/turn/cep.rs:1315-1323` contains a test
(`catalog_exposure_omits_unknown_envelope_method`) asserting that enabling
`session.envelope` fails because it is no longer a catalog method.

There is **no** `exec.command_run` method. It and post-mutation verify were
permanently retired from the CEP layer (issue #26); command execution is not an
agent-visible capability. `crates/lancet-catalog/src/catalog.rs` contains a
regression test (`catalog_retires_exec_command_run`) asserting its absence, and
`crates/lancet-api/src/runtime.rs` contains a production-path regression test
(`production_runtime_rejects_retired_exec_command_run_as_method_not_found`)
asserting a raw CEP call to `exec.command_run` returns JSON-RPC `-32601`.

### Removed subsystems (do not describe as active)

- **MSB/Upper sandbox workspace** (`lattice-workspace`, `Workspace` trait,
  `UpperHandle`, `UpperSpec`, `UpperLease`, `prepare_upper`, warm templates,
  guest-Git baseline/candidate flow, credential injection, `[workspace]`
  profile section with egress/image/lifecycle fields) was removed in #62.
  Turns run Direct-Local against the canonical project root; profiles with a
  legacy `[workspace]` section now fail strict parsing.
- **Landlock / netns command isolation** was removed (commit `a29072e2`,
  "refactor(isolation): remove landlock and netns command isolation"). The
  `[lancet.isolation.command]` schema (`LancetIsolationSection`,
  `LancetLandlockBehavior`, etc.) was dropped from `lattice-profile`.
  `crates/lattice-profile/src/schema.rs` has no isolation/landlock/netns
  sections (verified: the `[lancet]` sub-sections are project-root,
  observation, network, policy, workspace-policy, effects).
- **Command execution and post-mutation verify** (`exec.command_run`,
  `[lancet.command]`, `[lancet.verify]`) are retired surface; the profile
  schema has no typed sections for them (untyped tables fall through to
  `extensions`).
- **SQLite session graph store** (`sqlite_graph/`) was removed; the session
  graph is now git-backed only (`git_graph/`).
## 5. Directory structure rationale

The `crates/` directory is split into two families that map onto the layer
model. The `lancet-*` crates form the **LANCET CEP runtime** — the lower
layers that can be embedded independently of any host application. The
`lattice-*` crates form the **LATTICE session/agent/profile/api/tui/cli
runtime** — the upper layers that compose LANCET into a session-centric LLM
agent. Layering matters because it enforces a one-way dependency direction
(higher → lower) with no reverse deps, which keeps the LANCET runtime
embeddable and keeps each crate's concerns from leaking downward.

One sentence per crate:

- **`lancet-protocol`** — Pure CEP wire/data contracts (JSON-RPC frames,
  typed errors, policy hook data, effect types, provenance wire types); no I/O
  and no tokio. The bottom contract.
- **`net-guard`** — Neutral URL/DNS safety leaf: base-URL validation,
  public-address resolution, private/reserved IP checks, and
  rebinding-domain checks.
- **`lancet-catalog`** — Typed CEP method contracts, params/results,
  descriptors, and the static production `CATALOG` (5 agent-visible methods);
  it holds no operator-only contracts (`methods/audit.rs` was deleted in GitHub #39);
  depends only on protocol.
- **`lancet-dispatch`** — Generic method registry, typed CEP dispatch,
  in-process transport (`CepClient`/`CepTransport`), cancellation, and event
  sinks.
- **`lancet-audit`** — CEP audit persistence, query filters, stage/impact
  labels, provenance projection, attestation persistence (CEP audit SQLite
  lives here), and — since GitHub #39 — the whole operator read path: `query_audit` /
  `tail_audit` / `replay_audit` with their DTOs, project-keyed store-path
  resolution, and the workspace-wide `ScopedXdgStateHome` test helper behind
  the `test-support` feature. It must never depend on `lancet-native`,
  `lancet-dispatch`, `lancet-cep`, or `lancet-runtime`.
- **`lancet-authority`** — Policy semantics, capability matching, Guard L1
  path/scope checks, effect ledger, and effect-region checks.
- **`lancet-cep`** — Public CEP facade over protocol/catalog/dispatch, plus
  catalog-derived `ToolDefinition`/`ToolExposure` and `BaseAuthorityPlan`; no
  native execution, no policy side effects, no audit persistence, no
  `lattice-*` deps.
- **`lancet-native`** — Production native handlers, server lifecycle, the
  concrete guarded Local Workspace host (secure path resolution, bounded
  observation, Mutation Preconditions, atomic installation), edit/web
  helpers, audit integration, and effect-contract enforcement.
- **`lancet-api`** — Unified LANCET public API: composes `LancetRuntime` /
  `LancetServer` (server lifecycle, `CepClient`, audit, tool definitions)
  from the native crates, taking LANCET-native policy types; independent of
  LATTICE session/agent/profile layers. Its `audit` module is a thin
  delegating facade over `lancet-audit`'s operator read path, not an owner of
  it.
- **`lattice-llm`** — Provider adapters, SSE streaming, model routing,
  tool-call vocabulary, and logging. URL/DNS safety primitives live in the
  shared `net-guard` leaf and are composed here.
- **`lattice-session`** — Session graph contract, immutable turn commits,
  branch refs, HEAD state, and the git-backed `GitSessionGraphStore`.
- **`lattice-profile`** — Strict `agent.toml` schema, discovery, defaults, and
  LANCET profile sections (no isolation section).
- **`lattice-agent`** — Multi-turn `AgentLoop`, turn state machine, message
  assembly, and injectable `LlmClient` / `ToolCallExecutor` traits.
- **`lattice-api`** — Thin public facade for session creation/opening and turn
  execution; translates profile `[lancet]` policy into LANCET-native config
  and wires the agent loop over `lancet-api`'s CEP runtime.
- **`lattice-tui`** — `rustyline` inline REPL over `lattice-api` sessions and
  `stream_turn`; owns line editing, history, and inline rendering (no
  alternate screen, no ratatui).
- **`lattice-cli`** — Binary entry point and argument dispatch over
  API/session/model/audit commands, plus the `lattice-tui` presentation crate.
