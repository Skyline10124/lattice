<!-- generated-by: gsd-doc-writer -->

# LATTICE

![LATTICE banner](logo-banner.svg)

![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)
![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-orange.svg)

A session-centric LLM runtime in Rust: every turn commits to a **git-backed
session graph**, and all tool execution flows through **LANCET**, which uses
CEP (Controlled Execution Protocol) — a typed JSON-RPC method protocol with
policy gating, path guards, effect checks, and an audit trail.

## Workspace

LATTICE is a Cargo workspace of 16 crates, layered so dependencies always
point from higher layers down.

```text
Layer 0 (leaf):  lancet-protocol   lattice-llm   lattice-session   lattice-workspace
Layer 1:         lancet-catalog(->protocol)
                 lattice-profile(->protocol)
                 lattice-agent(->llm,session)
Layer 2:         lancet-dispatch(->protocol,catalog)
                 lancet-audit(->protocol,catalog)
                 lancet-authority(->protocol,catalog,llm)
Layer 2.5:       lancet-cep(->protocol,catalog,dispatch)
Layer 3:         lancet-native(->protocol,catalog,dispatch,audit,authority,llm)
Layer 3.5:       lancet-api(->cep,protocol,catalog,dispatch,audit,authority,native)
Layer 4:         lattice-api(-> lancet-api + lattice runtime crates)
Layer 5:         lattice-tui(-> api + presentation deps)
                 lattice-cli(-> api,tui + presentation deps)
```

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contracts: JSON-RPC frames, errors, policy/effect/provenance types. No I/O, no tokio. |
| `lancet-catalog` | Typed CEP method contracts, descriptors, and the static production catalog. |
| `lancet-dispatch` | Method registry, typed CEP dispatch, in-process transport, cancellation. |
| `lancet-cep` | Public CEP facade; projects LLM-facing tool schemas from the catalog. |
| `lancet-audit` | CEP audit persistence, query filters, attestation persistence. |
| `lancet-authority` | Policy semantics, capability matching, Guard L1 path/scope checks, effect ledger. |
| `lancet-native` | Production native handlers, server lifecycle, verify, edit/shell/web helpers. |
| `lancet-api` | Unified LANCET public API composing the CEP runtime (`LancetRuntime`/`LancetServer`). |
| `lattice-llm` | Provider adapters, SSE streaming, model routing, URL/DNS security. |
| `lattice-session` | Session graph contract and the **git-backed** graph store (`GitSessionGraphStore`). |
| `lattice-profile` | Strict `agent.toml` schema, discovery, defaults, LANCET `[lancet]` policy sections. |
| `lattice-agent` | Multi-turn `AgentLoop`, state machine, message assembly. |
| `lattice-workspace` | Swappable upper-workspace execution adapter seam (microsandbox/libkrun backend). |
| `lattice-api` | Thin public facade for session creation/opening and turn execution (`stream_turn`). |
| `lattice-tui` | Inline CLI chat REPL over `lattice-api` via `rustyline` (no alternate screen). |
| `lattice-cli` | The `lattice` binary: clap command surface and dispatch. |

### CEP method catalog

`lancet-catalog::CATALOG` is the authoritative production catalog with exactly
**8 methods**:

`observe.workspace`, `mutate.create`, `mutate.edit`, `mutate.batch`,
`fetch.web`, `session.todo_write`, `observe.audit`, `observe.audit_replay`.

There is no `session.envelope` method (it was removed; the catalog and tests
assert its absence). There is no `exec.command_run` method — it and
post-mutation verify were permanently retired from the CEP layer (issue #4);
command execution is not an agent-visible capability.

## Installation

Requires Rust **1.88+** (edition 2024). Build the workspace with cargo:

```sh
cargo build --release
```

The CLI binary is `lattice` (declared in `crates/lattice-cli/Cargo.toml`).
Run it without installing via `cargo run -p lattice-cli -- <command>`, or
install the release binary to `$CARGO_HOME/bin`:

```sh
cargo install --path crates/lattice-cli
```

## Quick start

```sh
# 1. Set a provider key (Anthropic, OpenAI, or Gemini)
export ANTHROPIC_API_KEY="..."

# 2. Create a session bound to an agent profile and project branch
cargo run -p lattice-cli -- session new --agent default --project-branch main

# 3. Send a prompt; the turn streams to the terminal and commits to the graph
cargo run -p lattice-cli -- session send <session_id> "Summarize this repo in one line."

# 4. Open an interactive inline REPL over that session
cargo run -p lattice-cli -- tui <session_id>
```

## Usage examples

```sh
# List authenticated models available to the configured providers
cargo run -p lattice-cli -- models --auth

# Resolve a model alias to its provider details
cargo run -p lattice-cli -- resolve sonnet

# Branch and inspect session history (every turn is a commit in the graph)
cargo run -p lattice-cli -- session branch <session_id> experiment
cargo run -p lattice-cli -- session log <session_id>

# LANCET self-check and audit-derived CEP status
cargo run -p lattice-cli -- lancet status
cargo run -p lattice-cli -- doctor
```

Expected outcomes: `session new` prints the new session id; `session send`
streams assistant tokens to stdout and tool/status lines to stderr, then
records the turn as a commit; `lancet status` shows recent CEP calls and the
last guard verdict; `doctor` runs diagnostics. Add `--json` (global flag) to
emit one `TurnEvent` JSON object per line on stdout instead of human output.

Other `session` subcommands: `agent`, `status`, `list`, `checkout`, `diff`,
`reset`, `resume`, `prune-snapshots`, and `migrate` (one-shot SQLite→Git
session graph migration for legacy stores).

## Build, test, lint

```sh
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --all --check
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines. For workspace
architecture guidance, see [AGENTS.md](AGENTS.md); for crate-local ownership,
read the `AGENTS.md` in the relevant `crates/<crate>/` directory.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
