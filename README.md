# LATTICE

![LATTICE banner](logo-banner.svg)

LATTICE is a model-centric LLM engine with model routing, agent execution, plugin runtime, and federated orchestration.

## Workspace

```
lattice/
├── crates/
│   ├── lattice-hooks/       # Universal hook system
│   ├── lattice-core/        # Core types, transport, streaming
│   ├── lattice-session/     # Session graph and git-style conversation refs
│   ├── lattice-prompt/      # Prompt layers, providers, and compilation
│   ├── lattice-sandbox/     # Command, path, URL, and Landlock policy
│   ├── lattice-tools/       # Tool schemas, capability bundles, and executor
│   ├── lattice-agent/       # Agent loop, state, memory, hooks, audit
│   ├── lattice-plugin/      # Plugin trait, loader, registry, DAG runner
│   ├── lattice-orchestrator/# Profiles, pipelines, event bus, micro-agents
│   ├── lattice-runtime/     # Top-level runtime and shared wiring policy
│   ├── lattice-plugins/     # Official plugin implementations
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
cargo run -p lattice-cli -- code "fix the failing tests" -m sonnet
cargo run -p lattice-cli -- run "1+1=?" -m deepseek-v4-flash
cargo run -p lattice-cli -- resolve sonnet --trace
cargo run -p lattice-cli -- models --auth
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
