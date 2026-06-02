# LATTICE

![LATTICE banner](logo-banner.svg)

LATTICE is a Rust LLM runtime for session-centered agent work: sessions own the active agent profile, profile changes are explicit session operations, and every turn lands as a commit-backed session history entry. Tool execution runs through **LANCET**, which uses CEP (Controlled Execution Protocol) internally for typed JSON-RPC methods, capability envelopes, policy gating, path guards, and audit trails.

The current LANCET tool surface is organized by effect contract: observation,
mutation, command execution, external fetch, session-state mutation, and audit
observation. The canonical catalog has 9 methods; legacy pre-v1.3.1 method names
are removed and are not accepted by the adapter or native registry.

## Workspace

```
lattice/
├── crates/
│   ├── lancet-protocol/ # CEP wire/data contracts: JSON-RPC, envelopes, errors, policy seam
│   ├── lancet-catalog/  # CEP typed methods, params/results, descriptors, static catalog
│   ├── lancet-dispatch/ # CEP typed dispatch, client/transport, cancellation, event sinks
│   ├── lancet-audit/    # LANCET audit store, query filters, rows, stages, attestations
│   ├── lancet-authority/ # CEP policy, Guard L1, envelope store, and effect-region semantics
│   ├── lancet-invariants/ # LANCET invariant cards and Guard L2 AST checks
│   ├── lancet-native/   # LANCET native handlers, transport integration, verify, edit, shell/web helpers
│   ├── lattice-llm/         # Transport, SSE streaming, model resolution, tool types, SSRF security
│   ├── lattice-session/     # Session graph, SQLite persistence, branch refs
│   ├── lattice-profile/     # Agent profile schema ([lancet] policy sections), discovery, defaults
│   ├── lattice-tooling/     # ToolExecutor trait, limits, enabled-tools resolution
│   ├── lattice-agent/       # Multi-turn agent loop (AgentLoop), state machine
│   ├── lattice-api/         # Public facade (turn execution/session); composes the per-turn LANCET runtime
│   └── lattice-cli/         # CLI (session, models, resolve, doctor, lancet, audit)
├── README.md
├── FINDINGS.md
└── FIX.md
```

Internal dependency flow:

```
lancet-protocol ─┬─> lancet-catalog ─┬─> lancet-dispatch ─┐
                      ├─> lattice-profile       ├─> lancet-audit     ├─> lancet-native
                      └─> lancet-authority └─> lancet-invariants┘
lattice-llm ─────────────> lattice-tooling ───────────────┐
                                                           ├─> lattice-api ─> lattice-cli
lattice-session ─────────> lattice-agent <─────────────────┘
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
```

## Future Work

The following capabilities are deferred beyond the current release:

- **TUI (deferred)** — A minimal terminal UI will be rebuilt on top of the `lattice-api` facade in a later milestone.
- **Rich prompt compilation (deferred)** — Layer priorities, token budgets, and provider-specific prompt assembly will be reintroduced after the main path is small and stable.
- **Sandbox & interactive approval (deferred)** — LANCET now provides the capability/policy/guard/verify layer over tool execution; OS-level sandboxing and interactive approval prompts are still future work.
- **Lifecycle hooks (deferred)** — The unused `lattice-hooks` leaf crate was removed. Reintroduce hooks with a concrete hook-dispatch integration point and internal consumer rather than as a standalone vocabulary crate.
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
