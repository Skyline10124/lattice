# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```sh
cargo build
cargo test
cargo clippy -- -D warnings
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
| `lattice-tools` | Tool schemas, edit operations, executor trait, project-root containment |
| `lattice-agent` | Multi-turn async agent loop (AgentLoop), state machine, memory |
| `lattice-api` | Thin public facade for create/open/run/stream/session operations |
| `lattice-cli` | Binary entry point, subcommands (run, models, resolve, session, doctor) |

### Key Types

- `ResolvedModel` — canonical model + provider + protocol + credential status
- `StreamEvent` — normalized streaming events (tokens, reasoning, tool calls, usage)
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

## Environment

Copy `.env.example` to `.env`. Supports 20+ providers (Anthropic, OpenAI, Gemini, DeepSeek,
Mistral, Groq, xAI, OpenRouter, Ollama, etc.). Only set keys are used.

## Future Work

The following capabilities are deferred beyond the current release:

- **TUI (deferred)** — A minimal terminal UI will be rebuilt on top of `lattice-api`.
- **Rich prompt compilation (deferred)** — Layer priorities, token budgets, and provider-specific assembly.
- **Redesigned security model (deferred)** — A new sandbox and approval system on the simplified API/session substrate.
- **Plugins and orchestration (deferred)** — Plugin contracts and DAG runners for multi-agent coordination.
- **Swarm and multi-agent collaboration (deferred)** — Session-graph-based coordination features.

## License

Apache-2.0
