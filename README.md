# LATTICE

![LATTICE banner](logo-banner.svg)

LATTICE is a Rust LLM runtime for running one session-centered agent with simple model routing, direct profile loading, commit-backed session history, and a CLI surface that does not hide extra architecture. Tool execution runs over **CEP** (Controlled Execution Protocol) — a typed JSON-RPC method protocol with capability envelopes, policy gating, path guards, and an audit trail.

## Workspace

```
lattice/
├── crates/
│   ├── cep-protocol/     # CEP pure data layer: JSON-RPC envelopes, method catalog, errors
│   ├── cep-executor/     # CEP server runtime: transport, registry, handlers, policy, guard, verify, audit
│   ├── lattice-llm/         # Transport, SSE streaming, model resolution, tool types, SSRF security
│   ├── lattice-session/     # Session graph, SQLite persistence, branch refs
│   ├── lattice-profile/     # Agent profile schema ([cep] policy sections), discovery, defaults
│   ├── lattice-tooling/     # ToolExecutor trait, limits, enabled-tools resolution
│   ├── lattice-agent/       # Multi-turn agent loop (AgentLoop), state machine
│   ├── lattice-api/         # Public facade (run/stream/session); composes the per-turn CEP runtime
│   ├── lattice-cli/         # CLI (run, models, resolve, session, doctor)
│   └── lattice-hooks/       # Lifecycle hook vocabulary (leaf; no internal consumers yet)
├── README.md
├── FINDINGS.md
└── FIX.md
```

Internal dependency flow:

```
cep-protocol ─┬─> lattice-profile ─┐
                 └─> cep-executor ─┤
lattice-llm ─────┼─> lattice-tooling ─┼─> lattice-api ─> lattice-cli
                 └─> lattice-agent <──┘
lattice-session ─────> lattice-agent
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
cargo run -p lattice-cli -- session list
cargo run -p lattice-cli -- session log <session_id>
cargo run -p lattice-cli -- doctor
```

## Future Work

The following capabilities are deferred beyond the current release:

- **TUI (deferred)** — A minimal terminal UI will be rebuilt on top of the `lattice-api` facade in a later milestone.
- **Rich prompt compilation (deferred)** — Layer priorities, token budgets, and provider-specific prompt assembly will be reintroduced after the main path is small and stable.
- **Sandbox & interactive approval (deferred)** — CEP now provides the capability/policy/guard/verify layer over tool execution; OS-level sandboxing and interactive approval prompts are still future work.
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
