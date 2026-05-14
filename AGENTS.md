# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

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
cargo run -p lattice-cli -- code "fix the failing tests" -m sonnet
cargo run -p lattice-cli -- run "1+1=?" -m deepseek-v4-flash
cargo run -p lattice-cli -- resolve sonnet --trace
cargo run -p lattice-cli -- models --auth
cargo run -p lattice-cli -- tui -m sonnet
```

## Architecture

LATTICE is a model-centric LLM engine. The workspace enforces a layered dependency flow:

```
hooks → core → session/prompt/sandbox/tools → agent → plugin → orchestrator → runtime → cli
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
| `lattice-plugin` | Plugin trait (typed LLM functions), DAG runner, loader, registry |
| `lattice-orchestrator` | Pipelines, micro-agent bus, agent registry, handoff rules, profiles |
| `lattice-runtime` | Top-level `Runtime` builder that wires everything together |
| `lattice-plugins` | Official plugin implementations (feature-gated) |
| `lattice-cli` | Binary entry point, subcommands, TUI, config, credentials |

### Key Types

- `Runtime` / `RuntimeBuilder` — main entrypoint; `Runtime::builder().build()`
- `ResolvedModel` — canonical model + provider + protocol + credential status
- `StreamEvent` — normalized streaming events (tokens, reasoning, tool calls, usage)
- `Agent` — multi-turn conversation loop with tool execution
- `Pipeline` — profile-driven agent orchestration with handoff/fork
- `Plugin` trait — typed LLM function contracts with Input/Output generics
- `EventBus` — async pub/sub for runtime and pipeline events
- `SandboxConfig` — platform-specific isolation policy

### Plugin Feature Flags

`lattice-plugins` gates each plugin behind a feature: `code-review`, `refactor`, `test-gen`,
`security-audit`, `doc-gen`, `pptx-gen`, `deep-research`, `image-gen`, `knowledge-base`,
`plan-gen`, `verification`. All enabled by default.

## Environment

Copy `.env.example` to `.env`. Supports 20+ providers (Anthropic, OpenAI, Gemini, DeepSeek,
Mistral, Groq, xAI, OpenRouter, Ollama, etc.). Only set keys are used.

## License

Apache-2.0
