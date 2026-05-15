# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

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
cargo run -p lattice-cli -- run "review src/" --agent reviewer
cargo run -p lattice-cli -- resolve sonnet --trace
cargo run -p lattice-cli -- models --auth
cargo run -p lattice-cli -- sessions list
cargo run -p lattice-cli -- config show
cargo run -p lattice-cli -- debug "test prompt" -m sonnet
cargo run -p lattice-cli -- doctor
cargo run -p lattice-cli -- tui -m sonnet
```

## Architecture

LATTICE is a model-centric LLM runtime. The workspace enforces a layered dependency flow:

```
hooks → core → session/prompt/sandbox/tools → agent → runtime → cli
```

| Crate | Role |
|---|---|
| `lattice-hooks` | Lifecycle hook points (pre/post agent turn, tool call, inference, session) |
| `lattice-core` | Foundation types, transport adapters, streaming, model resolution, routing |
| `lattice-session` | Git-style conversation graph with SQLite persistence and branch refs |
| `lattice-prompt` | Prompt layer stack, context providers, system prompt compilation |
| `lattice-sandbox` | Landlock isolation, path/command/URL policy, audit logging |
| `lattice-tools` | Tool schemas, capability bundles, executor trait, registry |
| `lattice-agent` | Multi-turn async agent loop, state machine, memory, context management |
| `lattice-policy` | Shared approval/denial foundation for sandbox and runtime security |
| `lattice-profile` | Agent profile specification, discovery, and lowering to runtime spec |
| `lattice-runtime` | Top-level `Runtime` builder that wires assembly, session, and turn execution |
| `lattice-cli` | Binary entry point, subcommands, TUI, config, credentials |

### Key Types

- `Runtime` / `RuntimeBuilder` — main entrypoint; `Runtime::builder().build()`
- `ResolvedModel` — canonical model + provider + protocol + credential status
- `StreamEvent` — normalized streaming events (tokens, reasoning, tool calls, usage)
- `RuntimeAgentSession` — assembled agent session from profile + runtime invocation context
- `TurnRequest` / `SubmittedTurn` / `TurnCompletion` — turn lifecycle types
- `RuntimeSecurity` / `RuntimeSecurityConfig` — sandbox, audit, and policy wiring
- `SessionStore` — session persistence trait

### Turn Execution

The main agent turn path is:

1. `prepare_profile_runtime_session` lowers a profile into an `AgentSessionRequest`, assembles a `RuntimeAgentSession`, and opens or resumes a session from the session store.
2. The agent loop runs inside the runtime's `Agent` state machine, streaming events as `RuntimeTurnEvent` values.
3. Turn outcomes are persisted as `SubmittedTurn` / `TurnCompletion` through the session store.

Common entry points are `prepare_profile_runtime_session`, `submit_session_turn`, and the `profile_drift_warning` guard that detects when a profile source has changed since the session start.

## Environment

Copy `.env.example` to `.env`. Supports 20+ providers (Anthropic, OpenAI, Gemini, DeepSeek,
Mistral, Groq, xAI, OpenRouter, Ollama, etc.). Only set keys are used.

## License

Apache-2.0
