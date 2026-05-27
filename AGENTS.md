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
cargo run -p lattice-cli -- session log <session_id>
cargo run -p lattice-cli -- doctor
```

## Architecture

LATTICE is a model-centric LLM runtime. The workspace enforces a layered dependency flow:

```
hooks -> llm -> session/profile/tools -> agent -> api -> cli
```

| Crate | Role |
|---|---|
| `lattice-hooks` | Lifecycle hook points (pre/post agent turn, tool call, inference, session) |
| `lattice-llm` | Transport adapters, streaming, model resolution, tool types, logging |
| `lattice-session` | Git-style conversation graph with SQLite persistence and branch refs |
| `lattice-profile` | Agent profile specification, discovery, and defaults |
| `lattice-tooling` | Tool schema and executor-facing shared types |
| `lancet-executor` | CEP/LANCET tool execution, edit operations, project-root containment |
| `lattice-agent` | Multi-turn async agent loop (AgentLoop), state machine, memory |
| `lattice-api` | Thin public facade for create/open/run/stream/session operations |
| `lattice-cli` | Binary entry point, subcommands (run, models, resolve, session, doctor) |

### Key Types

- `ResolvedModel` — canonical model + provider + protocol + credential status
- `LlmEvent` / `TurnEvent` — provider streaming events and public turn stream events
- `AgentLoop` — multi-turn agent loop with tool execution and injectable LlmClient/ToolCallExecutor
- `ToolCall` / `ToolDefinition` — tool invocation types
- `SessionGraphStore` — session graph persistence trait
- `SessionApi` — session operations facade (status, log, checkout, branch, diff, reset)

### Turn Execution

The main agent turn path is:

1. `lattice-api` exposes `run_turn` / `stream_turn` which assemble a session and call `AgentLoop`.
2. `AgentLoop` reads current session state, builds messages, calls the LLM via `LlmClient`, executes requested tools sequentially, and repeats until a final assistant response is produced.
3. Turn outcomes are persisted as commits in the session graph via `SessionGraphStore`.
4. CLI renders events and requires successful completion.

### Tool Safety

v1.1 tool safety is intentionally simple: project-root containment, no `..` traversal, file-size limits, and profile-gated optional shell/web tools. The removed sandbox and approval modules are not part of the current release.

## Where to make changes

Use this table to skip the "which crate owns this?" exploration step.

| If you're changing… | Go to | Key files |
|---|---|---|
| Tool implementations (fs, shell, web, search) | `lancet-executor/src/executor/` | `fs.rs`, `shell.rs`, `web.rs`, `search.rs` |
| Path containment / project-root checks | `lancet-executor/src/executor/` | `path_guard.rs` |
| Edit/diff application logic | `lancet-executor/src/edit/` | `apply.rs`, `parser.rs`, `diagnostics.rs` |
| Tool schemas / definitions exposed to LLM | `lattice-tooling/src/`, `lancet-executor/src/` | `enabled_tools.rs`, `tool_definitions.rs` |
| Provider protocol adapters | `lattice-llm/src/providers/` | `anthropic.rs`, `openai.rs`, `gemini.rs` |
| SSE streaming parser | `lattice-llm/src/` | `sse.rs`, `client.rs` |
| Model resolution / aliases | `lattice-llm/src/` | `router.rs`, `endpoints.rs` |
| URL / DNS / SSRF validation | `lattice-llm/src/` | `security.rs` |
| Agent loop, turn state machine | `lattice-agent/src/agent_loop/` | `mod.rs`, `messages.rs`, `traits.rs` |
| Public turn API (`run_turn` / `stream_turn`) | `lattice-api/src/turn/` | `execute.rs`, `mapping.rs`, `deps.rs` |
| Session graph / SQLite persistence | `lattice-session/src/` | `sqlite_graph.rs`, `graph.rs` |
| Profile schema, defaults, discovery | `lattice-profile/src/` | `schema.rs`, `defaults.rs`, `discovery.rs` |
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
