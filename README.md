# LATTICE

![LATTICE banner](logo-banner.svg)

LATTICE is a Rust LLM runtime for running one well-instrumented generic agent with reliable model routing, profile-driven prompt assembly, sandboxed tool execution, session persistence, and CLI/TUI feedback.

## Workspace

```
lattice/
├── crates/
│   ├── lattice-hooks/       # Lifecycle hook points
│   ├── lattice-core/        # Core types, transport, streaming, model resolution
│   ├── lattice-session/     # Session graph, SQLite persistence, branch refs
│   ├── lattice-prompt/      # Prompt layers, context providers, compilation
│   ├── lattice-sandbox/     # Landlock isolation, path/command/URL policy, audit
│   ├── lattice-tools/       # Tool schemas, capability bundles, executor
│   ├── lattice-agent/       # Multi-turn agent loop, state, memory, hooks
│   ├── lattice-policy/      # Shared approval/denial foundation
│   ├── lattice-profile/     # Agent profile specification and lowering
│   ├── lattice-runtime/     # Top-level runtime assembly and session wiring
│   └── lattice-cli/         # CLI and TUI
├── benches/
├── examples/
└── tests/
```

## Build

```sh
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --all --check
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

## Environment

```sh
export ANTHROPIC_API_KEY="..."
export OPENAI_API_KEY="..."
export DEEPSEEK_API_KEY="..."
```

## License

Apache-2.0. See [LICENSE](LICENSE).
