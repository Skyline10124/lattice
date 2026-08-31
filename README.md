# LATTICE

![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)
![Rust 1.94+](https://img.shields.io/badge/Rust-1.94%2B-orange.svg)
![Platform: Linux only](https://img.shields.io/badge/platform-Linux%20only-informational.svg)

A session-centric LLM runtime in Rust: every turn commits to a **git-backed
session graph**, and Tool execution flows through **LANCET**: a reservation-
gated, manifest-generated WASM Component runtime with host-enforced Cell
capabilities, path guards, evidence, and an audit trail.

## Platform support

LATTICE supports Linux hosts only for building, testing, deployment, and
runtime operation. macOS, Windows, BSD, and generic Unix targets are not
supported. The project does not preserve compatibility branches or weaker
fallback behavior for possible future ports; a future platform expansion would
require a separate architecture decision and a complete native implementation.

Source Tools are compiled to `wasm32-unknown-unknown` as sandboxed guest
artifacts inside the Linux host. That guest target is not another supported
deployment platform.

## Workspace

LATTICE is a Cargo workspace of 25 packages, layered so dependencies always
point from higher layers down. See [AGENTS.md](AGENTS.md) for the authoritative
graph with every edge spelled out.

```text
Layer 0 (leaf):  lancet-protocol  lattice-store  net-guard
Layer 1:         lattice-conversation(->protocol)
                 lattice-profile(->protocol)
                 lancet-authority(->protocol,net-guard)
                 lancet-audit(->protocol,lattice-store)
Layer 2:         lattice-llm(->net-guard,conversation)
                 lattice-session(->protocol,conversation,lattice-store)
                 lancet-runtime(->protocol,authority,net-guard,lattice-store)
Layer 3:         lattice-agent(->llm,conversation)
                 lancet-api(->protocol,audit,authority,runtime)
Layer 4:         lattice-api(->lancet-api,protocol,audit,authority,agent,
                             conversation,llm,profile,session)
Layer 5:         lattice-tui(->lattice-api)
                 lattice-cli(->lancet-api,lattice-api,session,tui)

Outside the product graph: lattice-tool-prelude and the five Source Tool
packages, mutate-shared, xtask, and lattice-system-harness.
```

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure wire/data contracts: durable identities and bindings, JSON-RPC frames, typed errors, Cell descriptors, and versioned evidence. No I/O or async runtime. |
| `lancet-audit` | Authoritative project-keyed Git `InvocationStore` plus pinned-view operator query/tail/replay; terminal records carry typed Cell evidence under `InvocationId`. |
| `lancet-authority` | Cell capability admission support, Guard L1 path checks, network policy, and workspace visibility policy. |
| `lancet-runtime` | Sole generated-Component execution path: closed catalog, reservation-bound call controls/events, Tool Host enforcement, Local Workspace/history, and pinned GET-only network execution. |
| `lancet-api` | Thin Turn-runtime and operator facade over `lancet-runtime` and `lancet-audit`. |
| `net-guard` | Neutral URL/DNS safety leaf: base-URL validation, private/reserved IP checks. |
| `lattice-llm` | Provider adapters, SSE streaming, model routing, URL/DNS security, and provider replay/projection. |
| `lattice-conversation` | Provider-neutral data-only conversation contract: ordered `NativeMessage` with bound/unbound ToolCall typestates, semantic/replay digests, per-attempt usage, and interruption records. No I/O, stores, or ID generators. |
| `lattice-session` | Git-backed Session graph plus the durable PendingTurn journal and typed `NativeMessage<BoundToolCall>` transcript. |
| `lattice-store` | ADR 0005 domain-neutral private-Git/Linux durability kernel: XDG topology, `ProjectStoreKey`, typed create/open, ref CAS/transactions, flock, durability barriers, atomic first-create, capability probes. No domain schemas, no workspace deps. |
| `lattice-profile` | Strict `agent.toml` schema, discovery, defaults, LANCET `[lancet]` policy sections. |
| `lattice-agent` | Store-free, one-attempt `ModelStepDriver`; it neither dispatches Tools nor commits Turns. |
| `lattice-api` | Public Session/Turn facade and sole production `TurnCoordinator` saga over PendingTurn, InvocationStore, and LANCET. |
| `lattice-tui` | Full-screen chat TUI over `lattice-api` via `ratatui`/`crossterm`: alternate screen, transcript / input / status regions. |
| `lattice-cli` | The `lattice` binary: clap command surface and dispatch. |
| `lattice-tool-prelude` | ADR 0004 guest SDK for Source Tools: generated WIT bindings, descriptor helpers. |
| `lattice-system-harness` | Black-box test harness driving the built binary via `LATTICE_BIN`; takes no product-crate dependency. |

### Component Tool catalog

The built runtime exposes exactly five manifest-owned Component Tools:
`observe.workspace`, `mutate.create`, `mutate.edit`, `fetch.web`, and
`session.todo_write`. Source Tool manifests and generated artifacts are the
only descriptor and provider-projection authority.

Audit query, tail, and replay are **operator-only** (`lattice audit` via
`lancet-api::{query_audit, tail_audit, replay_audit}`); they are not Tools and
cannot be enabled in `[tools].enabled`. Command execution is not an
agent-visible capability.

## Installation

Requires a Linux host and Rust **1.94+** (edition 2024; rustc 1.96.0 is pinned).
Build the shipped Linux host binary explicitly; Source Tool crates are wasm32
guest artifacts:

```sh
cargo build --release -p lattice-cli
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

# 4. Open the interactive full-screen TUI over that session
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

# LANCET self-check and invocation-ledger status
cargo run -p lattice-cli -- lancet status
cargo run -p lattice-cli -- lancet doctor
```

Expected outcomes: `session new` prints the new session id; `session send`
streams assistant tokens to stdout and tool/status lines to stderr, then
records the turn as a commit; `lancet status` shows a pinned ledger view and
recent typed invocation states; `lancet doctor` validates the workspace and
generated closed catalog. Add `--json` (global flag) to
emit one `TurnEvent` JSON object per line on stdout instead of human output.

## Build, test, lint

```sh
cargo build -p lattice-cli
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
