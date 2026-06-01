# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

## Build & Test

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
cargo test <test_name>          # single test
cargo bench                     # benchmarks
```

## CLI

```sh
cargo run -p lattice-cli -- session new --agent default
cargo run -p lattice-cli -- session send <session_id> "1+1=?"
cargo run -p lattice-cli -- session agent <session_id> reviewer
cargo run -p lattice-cli -- resolve sonnet
cargo run -p lattice-cli -- models --auth
cargo run -p lattice-cli -- session status
cargo run -p lattice-cli -- session list
cargo run -p lattice-cli -- session log <session_id>
cargo run -p lattice-cli -- doctor
cargo run -p lattice-cli -- lancet status
cargo run -p lattice-cli -- lancet doctor
cargo run -p lattice-cli -- audit
```

## Architecture

LATTICE is a session-centric LLM runtime. Tool execution flows through **LANCET**,
which uses CEP (Controlled Execution Protocol) internally as a JSON-RPC method
protocol with capability envelopes, policy gating, path guards, and an audit trail. The workspace
enforces a layered internal dependency flow:

```
Layer 0 (leaf):  lancet-protocol   lattice-llm   lattice-session
Layer 1:         lancet-catalog(→protocol)  lattice-profile(→protocol)  lattice-tooling(→llm)  lattice-agent(→llm,session)
Layer 2:         lancet-dispatch(→protocol,catalog)  lancet-audit(→protocol,catalog)  lancet-authority(→protocol,catalog,llm)  lancet-invariants(→catalog)
Layer 3:         lancet-native(→protocol,catalog,dispatch,audit,authority,invariants,llm,tooling)
Layer 4:         lattice-api(→ all of the above)
Layer 5:         lattice-cli(→ api + llm/session/agent/profile/tooling/authority/invariants)
```

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contract: JSON-RPC 2.0 envelopes, frame aliases (`ToolCall`/`ToolResult`/`ToolEvent`), execution envelopes (intent/capabilities/scope/impact/guard), typed `CepError` families, policy hook types, provenance wire types. No I/O, no tokio. |
| `lancet-catalog` | Typed CEP methods: `CepMethod`, params/results, `MethodDescriptor`, static 14-method `CATALOG`, and schema data needed for catalog projection. |
| `lancet-dispatch` | CEP typed dispatch and in-process transport: generic `MethodRegistry`, `CepClient`/`CepTransport`, cancellation, and event sink. Does not depend on native policy/audit/effect implementations. |
| `lancet-audit` | Audit persistence: `CepAuditStore`, query filters, row model, stage labels, impact/capability labels, PROV projection, and attestation persistence. SQLite dependency is kept here. |
| `lancet-authority` | CEP authority semantics: default policy, capability matching, command/network/observation/budget policy, Guard L1 path/scope checks, active envelope store, effect ledger, authorized effect regions, and effect-region checker. |
| `lancet-invariants` | LANCET invariant card model, loading/index/review helpers, opaque-human-review helpers, `.lancet/invariants` resolution, and Guard L2 AST checks. |
| `lancet-native` | Production native handlers plus native transport/lifecycle integration, verify, edit, shell/web helpers, and LLM-facing tool definition projection. It keeps compatibility re-exports for authority/invariant types; low-level helpers remain crate-internal behind native handlers. |
| `lattice-llm` | Transport adapters, SSE streaming, model resolution/routing, tool types, SSRF/URL security, logging |
| `lattice-session` | Git-style conversation graph with SQLite persistence and branch refs |
| `lattice-profile` | Agent `agent.toml` profile schema (incl. `[lancet]` policy sections), discovery, defaults |
| `lattice-tooling` | Tool `ToolExecutor` trait, `ToolLimits`, and enabled-tools resolution shared types |
| `lattice-agent` | Multi-turn async agent loop (`AgentLoop`), state machine, injectable `LlmClient`/`ToolCallExecutor` |
| `lattice-api` | Thin public facade for create/open/turn execution/session; composes the per-turn LANCET runtime |
| `lattice-cli` | Binary entry point, subcommands (session, models, resolve, doctor, lancet, audit) |

### Key Types

- `ResolvedModel` — canonical model + provider + protocol + credential status
- `LlmEvent` / `TurnEvent` — provider streaming events and public turn stream events
- `AgentLoop` — multi-turn agent loop with tool execution and injectable `LlmClient`/`ToolCallExecutor`
- `CepMethod` / `MethodDescriptor` / `CATALOG` — typed CEP methods and the static 14-method catalog
- `ExecutionEnvelope` / `Capability` / `GuardVerdict` — CEP capability/scope/guard model
- `PolicyHook` / `PolicyDecision` — pluggable capability/command/network/budget policy
- `CepClient` / `CepTransport` / `MethodRegistry` — CEP client + in-process transport + server registry
- `CepExecutorAdapter` — maps LLM-shape `ToolCall`s into typed CEP invocations (implements `ToolCallExecutor`)
- `SessionGraphStore` / `SessionApi` — session graph persistence trait + operations facade

### LANCET Method Catalog

The catalog is exactly 14 methods (`lancet-catalog/src/catalog.rs`):

`workspace.read_file`, `workspace.grep`, `workspace.list_directory`,
`workspace.find_files`, `workspace.write_file`, `workspace.patch`, `web.fetch`,
`session.todo_write`, `command.run`, `envelope.declare`,
`envelope.submit_intent`, `audit.query`, `audit.tail`, `audit.replay`.

LLM-facing tool schemas are generated **only** via `to_tool_definitions` from
the catalog — there is no legacy `default_tool_definitions()` path.

### Turn Execution

The main agent turn path is:

1. `lattice-api` exposes `run_turn` / `stream_turn`; `stream_turn` is the single live execution core and `run_turn` drains it via `RunTurnCollector`.
2. `turn/cep.rs` composes a **turn-scoped LANCET runtime**: it builds a native `MethodRegistry` (`register_production_method_registry`), a dispatch `CepTransport` server + `CepClient`, installs the authority policy hook from the profile `[lancet]` sections, runs the server with `run_native`, and wraps the client in a `CepExecutorAdapter`.
3. `AgentLoop` reads session state, builds messages, calls the LLM via `LlmClient`, and for each requested tool call invokes the adapter, which sends a typed CEP JSON-RPC frame over the transport.
4. The native server lifecycle records audit start/finish and effect checks; native handlers invoke authority policy/Guard L1/effect helpers and invariant Guard L2 checks around execution and verify. Mutating methods require a prior `envelope.declare` authority.
5. Turn outcomes are persisted as commits in the session graph via `SessionGraphStore`.
6. CLI renders events and requires successful completion (exit-code semantics).

### LANCET Policy & Safety

Safety is enforced by the LANCET runtime, configured via profile `[lancet]` sub-tables:

- **`mode`** — `enforce` (default), `warn`, or `off` for effect-region enforcement. `off` skips the effect-region check only; policy, capability checks, and native LANCET routing remain active.
- **`[lancet.project_root]`** — effective workspace scope; the `guard` enforces no escape and no `..` traversal.
- **`[lancet.observation]`** — read/grep/list/find/todo budgets and allowed roots.
- **`[lancet.network]`** — `web.fetch` host allow policy (public HTTPS / localhost / allowlist) and response size cap.
- **`[lancet.command]`** — `command.run` timeouts, output cap, inline-shell toggle, diagnostic allowlist/denylist, env passthrough.
- **`[lancet.policy]`** — pointer to an external structured TOML policy file.

Mutations (`workspace.write_file` / `workspace.patch`) require an active
`envelope.declare` capability scope (Guard L1) and run an optional post-mutation
`verify` command. The pre-CEP "removed sandbox/approval" note no longer applies.

## Where to make changes

Use this table to skip the "which crate owns this?" exploration step.

| If you're changing… | Go to | Key files |
|---|---|---|
| CEP protocol types (envelopes, frames, errors, JSON-RPC, policy seam, provenance wire types) | `lancet-protocol/src/` | `capability.rs`, `envelope.rs`, `frame.rs`, `error.rs`, `jsonrpc.rs`, `policy.rs`, `provenance.rs` |
| CEP method definitions / catalog | `lancet-catalog/src/` | `catalog.rs`, `method.rs`, `methods/{workspace,command,external,session,envelope,audit}.rs` |
| CEP generic dispatch / transport | `lancet-dispatch/src/` | `registry.rs`, `transport.rs` |
| LANCET audit persistence / provenance projection | `lancet-audit/src/` | `audit.rs`, `provenance/{attestation,canonicalize,projector}.rs` |
| CEP authority semantics (policy/capability/network/command/observation/budget, Guard L1, active envelope store, effect ledger/checker) | `lancet-authority/src/` | `policy/`, `guard.rs`, `envelope_store.rs`, `effect/` |
| LANCET invariant cards and Guard L2 AST checks | `lancet-invariants/src/` | `invariants/`, `guard_l2.rs` |
| Native CEP method handlers (server impls) | `lancet-native/src/handlers/` | `mutation.rs`, `command.rs`, `external.rs`, `observation.rs`, `session_state.rs`, `envelope.rs`, `audit.rs` |
| Native policy/guard bridge, verify, and transport lifecycle | `lancet-native/src/` | `guard_l1.rs`, `verify.rs`, `transport.rs`, `registry.rs`, `native_registry.rs` |
| Low-level native helpers | `lancet-native/src/executor/` | `shell.rs`, `web.rs`, `render.rs` |
| Edit/diff application logic | `lancet-native/src/edit/` | `apply.rs`, `parser.rs`, `diagnostics.rs`, `hash.rs` |
| LLM-facing tool schema generation | `lancet-native/src/` | `tool_definitions.rs` |
| Enabled-tools resolution | `lattice-tooling/src/` | `enabled_tools.rs` |
| Provider protocol adapters | `lattice-llm/src/providers/` | `anthropic.rs`, `openai.rs`, `gemini.rs` |
| SSE streaming parser | `lattice-llm/src/` | `sse.rs`, `client.rs` |
| Model resolution / aliases | `lattice-llm/src/` | `router.rs`, `endpoints.rs` |
| URL / DNS / SSRF validation | `lattice-llm/src/` | `security.rs` |
| Agent loop, turn state machine | `lattice-agent/src/agent_loop/` | `mod.rs`, `messages.rs`, `traits.rs`, `types.rs` |
| Public turn API + per-turn LANCET composition | `lattice-api/src/turn/` | `execute.rs`, `cep.rs`, `live.rs`, `mapping.rs`, `collector.rs` |
| Session graph / SQLite persistence | `lattice-session/src/` | `graph.rs`, `sqlite_graph/{mod,commit,refs,session,schema}.rs` |
| Profile schema (incl. `[lancet]`), defaults, discovery | `lattice-profile/src/` | `schema.rs`, `defaults.rs`, `discovery.rs`, `load.rs` |
| CLI subcommands | `lattice-cli/src/commands/` | `run.rs`, `models.rs`, `resolve.rs`, `session.rs`, `doctor.rs` |

## Repo meta files

These are not part of the source tree but show up at the root — know what they are so you don't waste time guessing.

- **`FIX.md`** — append-only changelog of applied code-review fixes (HIGH / MEDIUM / LOW per phase). Authoritative record of what's been fixed.
- **`FINDINGS.md`** — open issues deferred past the current release. Every entry has a location, problem, and plan. If you're picking up follow-up work, start here.
- **`.planning/`** — GSD workflow artifacts (`PROJECT.md`, `ROADMAP.md`, `REQUIREMENTS.md`, `STATE.md`, `phases/`). Drives planning skills; treat as scoping context, not source.
- **`.agents/`, `.codex/`, `.sisyphus/`, `.worktrees/`** — agent/tooling scratch dirs. Don't edit unless that's the task.

## Future Work

The following capabilities are deferred beyond the current release:

- **TUI (deferred)** — A minimal terminal UI will be rebuilt on top of `lattice-api`.
- **Rich prompt compilation (deferred)** — Layer priorities, token budgets, and provider-specific assembly.
- **Redesigned security model (deferred)** — A new sandbox and approval system on the simplified API/session substrate.
- **Plugins and orchestration (deferred)** — Plugin contracts and DAG runners for multi-agent coordination.
- **Lifecycle hooks (deferred)** — The unused `lattice-hooks` leaf crate was removed. Reintroduce hooks only with a concrete dispatch integration point and internal consumer.
- **Swarm and multi-agent collaboration (deferred)** — Session-graph-based coordination features.

## Cursor Cloud specific instructions

- **Rust version**: The declared MSRV is 1.88 to match locked transitive dependencies (e.g. `icu_*`, `time`). Use `rustup default stable` (currently 1.95) to satisfy all deps.
- **Build dirs**: `.cargo/config.toml` pins `target-dir = "target"` and `TMPDIR = build/tmp` so agent/sandbox builds avoid `/tmp/cursor-sandbox-cache` disk quota errors. No manual `CARGO_TARGET_DIR`/`TMPDIR` exports needed for normal `cargo build` / `cargo test`.
- **System dependency**: `libssl-dev` (and `pkg-config`) must be installed for the `openssl-sys` transitive dependency (pulled in via `reqwest`'s native-tls feature in the dependency graph).
- **No external services needed**: All tests use mock/fake LLM clients. `cargo test` passes without any API keys or network access.
- **Lint note**: `cargo clippy --all-targets -- -D warnings` may flag new lints as Rust stable advances (e.g. `clippy::io_other_error` on 1.95). `cargo fmt --all --check` may also report diffs if rustfmt formatting rules have evolved since the code was last formatted.
- **CLI demo commands** (no API key required): `cargo run -p lattice-cli -- doctor`, `resolve <alias>`, `models`, `session status`.
- **Running `session send`** requires: (1) an existing session created with `session new --agent <name>`, (2) a matching agent profile at `.lattice/agents/<name>/agent.toml` whose `[agent].name` is `<name>`, and (3) the relevant provider API key env var (e.g. `ANTHROPIC_API_KEY`).

## License

Apache-2.0
