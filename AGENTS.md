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
cargo run -p lattice-cli -- run "1+1=?" -m sonnet
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

LATTICE is a model-centric LLM runtime. Tool execution flows through **CEP**
(Controlled Execution Protocol) — a JSON-RPC method protocol with capability
envelopes, policy gating, path guards, and an audit trail. The workspace
enforces a layered internal dependency flow:

```
Layer 0 (leaf):  lancet-protocol   lattice-llm   lattice-session   lattice-hooks
Layer 1:         lattice-profile(→protocol)  lattice-tooling(→llm)  lattice-agent(→llm,session)
Layer 2:         lancet-executor(→protocol, llm, tooling)
Layer 3:         lattice-api(→ all of the above)
Layer 4:         lattice-cli(→ api + llm/session/agent/profile/tooling)
```

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP data layer: JSON-RPC 2.0 envelopes, frame aliases (`ToolCall`/`ToolResult`/`ToolEvent`), execution envelopes (intent/capabilities/scope/impact/guard), typed `CepError` families, typed method catalog (`CATALOG`), policy hook types. No I/O, no tokio. |
| `lancet-executor` | CEP **server-side runtime**: in-process JSON-RPC `transport`, `MethodRegistry`, native method `handlers`, `policy` engine, `guard` (path-scope), `verify` (post-mutation), `audit` store, envelope store. Low-level `executor/` and `edit/` helpers are private implementation details behind native handlers. |
| `lattice-llm` | Transport adapters, SSE streaming, model resolution/routing, tool types, SSRF/URL security, logging |
| `lattice-session` | Git-style conversation graph with SQLite persistence and branch refs |
| `lattice-profile` | Agent `agent.toml` profile schema (incl. `[cep]` policy sections), discovery, defaults |
| `lattice-tooling` | Tool `ToolExecutor` trait, `ToolLimits`, and enabled-tools resolution shared types |
| `lattice-agent` | Multi-turn async agent loop (`AgentLoop`), state machine, injectable `LlmClient`/`ToolCallExecutor` |
| `lattice-api` | Thin public facade for create/open/run/stream/session; composes the per-turn CEP runtime |
| `lattice-cli` | Binary entry point, subcommands (run, models, resolve, session, doctor, lancet, audit) |
| `lattice-hooks` | Lifecycle hook vocabulary (turn/tool/inference/session points). **Currently has no internal consumers** — a leaf crate kept for the planned hook-dispatch integration. |

### Key Types

- `ResolvedModel` — canonical model + provider + protocol + credential status
- `LlmEvent` / `TurnEvent` — provider streaming events and public turn stream events
- `AgentLoop` — multi-turn agent loop with tool execution and injectable `LlmClient`/`ToolCallExecutor`
- `CepMethod` / `MethodDescriptor` / `CATALOG` — typed CEP methods and the static 14-method catalog
- `ExecutionEnvelope` / `Capability` / `GuardVerdict` — CEP capability/scope/guard model
- `PolicyHook` / `PolicyDecision` — pluggable capability/command/network/budget policy
- `LancetClient` / `LancetTransport` / `MethodRegistry` — CEP client + in-process transport + server registry
- `LancetExecutorAdapter` — maps LLM-shape `ToolCall`s into typed CEP invocations (implements `ToolCallExecutor`)
- `SessionGraphStore` / `SessionApi` — session graph persistence trait + operations facade

### CEP Method Catalog

The catalog is exactly 14 methods (`lancet-protocol/src/catalog.rs`):

`workspace.read_file`, `workspace.grep`, `workspace.list_directory`,
`workspace.find_files`, `workspace.write_file`, `workspace.patch`, `web.fetch`,
`session.todo_write`, `command.run`, `envelope.declare`,
`envelope.submit_intent`, `audit.query`, `audit.tail`, `audit.replay`.

LLM-facing tool schemas are generated **only** via `to_tool_definitions` from
the catalog — there is no legacy `default_tool_definitions()` path.

### Turn Execution

The main agent turn path is:

1. `lattice-api` exposes `run_turn` / `stream_turn`; `stream_turn` is the single live execution core and `run_turn` drains it via `RunTurnCollector`.
2. `turn/lancet.rs` composes a **turn-scoped CEP runtime**: it builds a `MethodRegistry` (`register_production_method_registry`), a `LancetTransport` server + `LancetClient`, installs the policy hook from the profile `[cep]` sections, and wraps the client in a `LancetExecutorAdapter`.
3. `AgentLoop` reads session state, builds messages, calls the LLM via `LlmClient`, and for each requested tool call invokes the adapter, which sends a typed CEP JSON-RPC frame over the transport.
4. The server registry runs the method through policy → guard (path scope) → execution → verify (post-mutation), recording an audit entry; mutating methods require a prior `envelope.declare` authority.
5. Turn outcomes are persisted as commits in the session graph via `SessionGraphStore`.
6. CLI renders events and requires successful completion (exit-code semantics).

### CEP Policy & Safety

Safety is enforced by the CEP runtime, configured via profile `[cep]` sub-tables:

- **`mode`** — `enforce` (default), `warn`, or `off` for effect-region enforcement. `off` skips the effect-region check only; policy, capability checks, and native CEP routing remain active.
- **`[cep.project_root]`** — effective workspace scope; the `guard` enforces no escape and no `..` traversal.
- **`[cep.observation]`** — read/grep/list/find/todo budgets and allowed roots.
- **`[cep.network]`** — `web.fetch` host allow policy (public HTTPS / localhost / allowlist) and response size cap.
- **`[cep.command]`** — `command.run` timeouts, output cap, inline-shell toggle, diagnostic allowlist/denylist, env passthrough.
- **`[cep.policy]`** — pointer to an external structured TOML policy file.

Mutations (`workspace.write_file` / `workspace.patch`) require an active
`envelope.declare` capability scope (Guard L1) and run an optional post-mutation
`verify` command. The pre-CEP "removed sandbox/approval" note no longer applies.

## Where to make changes

Use this table to skip the "which crate owns this?" exploration step.

| If you're changing… | Go to | Key files |
|---|---|---|
| CEP protocol types (envelopes, frames, errors, JSON-RPC) | `lancet-protocol/src/` | `envelope.rs`, `frame.rs`, `error.rs`, `jsonrpc.rs`, `method.rs` |
| CEP method definitions / catalog | `lancet-protocol/src/` | `catalog.rs`, `methods/{workspace,command,external,session,envelope}.rs` |
| Native CEP method handlers (server impls) | `lancet-executor/src/handlers/` | `mutation.rs`, `command.rs`, `external.rs`, `observation.rs`, `session_state.rs`, `envelope.rs` |
| CEP transport / registry / server wiring | `lancet-executor/src/` | `transport.rs`, `registry.rs`, `native_registry.rs` |
| Policy engine (capability/command/network/budget) | `lancet-executor/src/policy/` | `default.rs`, `command.rs`, `network.rs`, `observation.rs`, `budget.rs`, `capability.rs`, `resolved.rs` |
| Path guard / verify / audit | `lancet-executor/src/` | `guard.rs`, `verify.rs`, `audit.rs`, `envelope_store.rs` |
| Low-level executor helpers | `lancet-executor/src/executor/` | `fs.rs`, `shell.rs`, `web.rs`, `search.rs`, `path_guard.rs`, `render.rs` |
| Edit/diff application logic | `lancet-executor/src/edit/` | `apply.rs`, `parser.rs`, `diagnostics.rs`, `hash.rs` |
| LLM-facing tool schema generation | `lancet-executor/src/` | `tool_definitions.rs` |
| Enabled-tools resolution | `lattice-tooling/src/` | `enabled_tools.rs` |
| Provider protocol adapters | `lattice-llm/src/providers/` | `anthropic.rs`, `openai.rs`, `gemini.rs` |
| SSE streaming parser | `lattice-llm/src/` | `sse.rs`, `client.rs` |
| Model resolution / aliases | `lattice-llm/src/` | `router.rs`, `endpoints.rs` |
| URL / DNS / SSRF validation | `lattice-llm/src/` | `security.rs` |
| Agent loop, turn state machine | `lattice-agent/src/agent_loop/` | `mod.rs`, `messages.rs`, `traits.rs`, `types.rs` |
| Public turn API + per-turn CEP composition | `lattice-api/src/turn/` | `execute.rs`, `lancet.rs`, `live.rs`, `mapping.rs`, `collector.rs` |
| Session graph / SQLite persistence | `lattice-session/src/` | `graph.rs`, `sqlite_graph/{mod,commit,refs,session,schema}.rs` |
| Profile schema (incl. `[cep]`), defaults, discovery | `lattice-profile/src/` | `schema.rs`, `defaults.rs`, `discovery.rs`, `load.rs` |
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
- **Swarm and multi-agent collaboration (deferred)** — Session-graph-based coordination features.

## Cursor Cloud specific instructions

- **Rust version**: The declared MSRV is 1.88 to match locked transitive dependencies (e.g. `icu_*`, `time`). Use `rustup default stable` (currently 1.95) to satisfy all deps.
- **Build dirs**: `.cargo/config.toml` pins `target-dir = "target"` and `TMPDIR = build/tmp` so agent/sandbox builds avoid `/tmp/cursor-sandbox-cache` disk quota errors. No manual `CARGO_TARGET_DIR`/`TMPDIR` exports needed for normal `cargo build` / `cargo test`.
- **System dependency**: `libssl-dev` (and `pkg-config`) must be installed for the `openssl-sys` transitive dependency (pulled in via `reqwest`'s native-tls feature in the dependency graph).
- **No external services needed**: All tests use mock/fake LLM clients. `cargo test` passes without any API keys or network access.
- **Lint note**: `cargo clippy --all-targets -- -D warnings` may flag new lints as Rust stable advances (e.g. `clippy::io_other_error` on 1.95). `cargo fmt --all --check` may also report diffs if rustfmt formatting rules have evolved since the code was last formatted.
- **CLI demo commands** (no API key required): `cargo run -p lattice-cli -- doctor`, `resolve <alias>`, `models`, `session status`.
- **Running the `run` subcommand** requires: (1) an agent profile at `.lattice/agents/default/agent.toml` in the working directory, and (2) the relevant provider API key env var (e.g. `ANTHROPIC_API_KEY`).

## License

Apache-2.0
