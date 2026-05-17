# LATTICE

![LATTICE banner](logo-banner.svg)

LATTICE is a Rust LLM runtime for running one session-centered agent with simple model routing, direct profile loading, commit-backed session history, and a CLI surface that does not hide extra architecture.

## Workspace

```
lattice/
├── crates/
│   ├── lattice-hooks/       # Lifecycle hook points
│   ├── lattice-llm/         # Transport, streaming, model resolution, tool types
│   ├── lattice-session/     # Session graph, SQLite persistence, branch refs
│   ├── lattice-profile/     # Agent profile specification, discovery, defaults
│   ├── lattice-tools/       # Tool schemas, executor trait, edit operations
│   ├── lattice-agent/       # Multi-turn agent loop, state, memory, hooks
│   ├── lattice-api/         # API surface types and shared protocol
│   └── lattice-cli/         # CLI (run, models, resolve, session, doctor)
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
cargo run -p lattice-cli -- resolve sonnet
cargo run -p lattice-cli -- models --auth
cargo run -p lattice-cli -- session status
cargo run -p lattice-cli -- session log <session_id>
cargo run -p lattice-cli -- doctor
```

## Future Work

The following capabilities are deferred beyond the current release:

- **TUI (deferred)** — A minimal terminal UI will be rebuilt on top of the `lattice-api` facade in a later milestone.
- **Rich prompt compilation (deferred)** — Layer priorities, token budgets, and provider-specific prompt assembly will be reintroduced after the main path is small and stable.
- **Redesigned security model (deferred)** — A new sandbox and approval system will be built on the simplified API/session substrate in a future milestone.
- **Plugins and orchestration (deferred)** — Plugin contracts and DAG runners will be reconsidered after the session and API boundaries are stable.
- **Swarm and multi-agent collaboration (deferred)** — Coordination features will use the session graph as a foundation in a later milestone.

## Environment

```sh
export ANTHROPIC_API_KEY="..."
export OPENAI_API_KEY="..."
export DEEPSEEK_API_KEY="..."
```

## License

Apache-2.0. See [LICENSE](LICENSE).
