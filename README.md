<!-- generated-by: gsd-doc-writer -->

# LATTICE

![LATTICE banner](logo-banner.svg)

![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)
![Rust 1.94+](https://img.shields.io/badge/Rust-1.94%2B-orange.svg)
![Platform: Linux only](https://img.shields.io/badge/platform-Linux%20only-informational.svg)

A session-centric LLM runtime in Rust: every turn commits to a **git-backed
session graph**, and all tool execution flows through **LANCET**, which uses
CEP (Controlled Execution Protocol) — a typed JSON-RPC method protocol with
policy gating, path guards, effect checks, and an audit trail.

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

LATTICE is a Cargo workspace of 21 crates, layered so dependencies always
point from higher layers down. See [AGENTS.md](AGENTS.md) for the authoritative
graph with every edge spelled out.

```text
Layer 0 (leaf):  lancet-protocol  lattice-store
Layer 0.5:       net-guard
                 lattice-conversation(->protocol)
                 lattice-llm(->net-guard,conversation)
Layer 1:         lancet-catalog(->protocol)
                 lattice-profile(->protocol)
                 lattice-session(->conversation)
                 lattice-agent(->llm,conversation)
Layer 2:         lancet-dispatch(->protocol,catalog)
                 lancet-audit(->protocol,catalog)
                 lancet-authority(->protocol,catalog,net-guard)
Layer 2.5:       lancet-cep(->protocol,catalog,dispatch)
                 lancet-runtime(->protocol,dispatch)
Layer 3:         lancet-native(->protocol,catalog,dispatch,audit,authority,
                               net-guard,runtime)
Layer 3.5:       lancet-api(->protocol,catalog,dispatch,cep,audit,authority,
                            native,runtime)
Layer 4:         lattice-api(->lancet-api,lancet-protocol,lancet-catalog,
                             lancet-authority,lancet-native,lattice-agent,
                             lattice-conversation,lattice-llm,lattice-profile,
                             lattice-session)
Layer 5:         lattice-tui(->lattice-api + presentation deps)
                 lattice-cli(->lattice-api,lattice-session,lattice-tui,
                              lancet-api + presentation deps)

Outside the graph: lattice-tool-prelude (guest SDK, wit-bindgen only) and
lattice-system-harness (black-box tests; must not depend on product crates).
```

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contracts: JSON-RPC frames, errors, policy/effect and versioned Cell-evidence types. No I/O, no tokio. |
| `lancet-catalog` | Typed CEP method contracts, descriptors, and the static production catalog. |
| `lancet-dispatch` | Method registry, typed CEP dispatch, in-process transport, cancellation. |
| `lancet-cep` | Public CEP facade; projects LLM-facing tool schemas from the catalog. |
| `lancet-audit` | Authoritative project-keyed Git `InvocationStore` plus pinned-view operator query/tail/replay; terminal records carry typed Cell evidence under `InvocationId`. |
| `lancet-authority` | Policy semantics, capability matching, Guard L1 path/scope checks, effect ledger. |
| `lancet-native` | Remaining native `fetch.web` handler, CEP server lifecycle, reservation-gated dispatch, and the Component/native split. |
| `lancet-runtime` | WASM Component engine, closed composition gate, Tool Host enforcement, and guarded Local Workspace/history. |
| `lancet-api` | Unified LANCET facade for runtime composition and Git-ledger operator reads. |
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

### CEP method catalog

`lancet-catalog::CATALOG` is the authoritative production catalog with exactly
**5 agent-visible methods**:

`observe.workspace`, `mutate.create`, `mutate.edit`,
`fetch.web`, `session.todo_write`.

Audit query, tail, and replay are **operator-only** (`lattice audit` via
`lancet-api::{query_audit, tail_audit, replay_audit}`); they are not CEP catalog
methods and cannot be enabled in `[tools].enabled`.

There is no `session.envelope` method (it was removed; the catalog and tests
assert its absence). There is no `exec.command_run` method — it and
post-mutation verify were permanently retired from the CEP layer (issue #26);
command execution is not an agent-visible capability.

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

# LANCET self-check and audit-derived CEP status
cargo run -p lattice-cli -- lancet status
cargo run -p lattice-cli -- doctor
```

Expected outcomes: `session new` prints the new session id; `session send`
streams assistant tokens to stdout and tool/status lines to stderr, then
records the turn as a commit; `lancet status` shows recent CEP calls and the
last guard verdict; `doctor` runs diagnostics. Add `--json` (global flag) to
emit one `TurnEvent` JSON object per line on stdout instead of human output.

Other `session` subcommands: `agent`, `status`, `list`, `checkout`, `diff`, and
`reset`. Legacy Session/audit formats are not imported and there is no `migrate`
command; disposable development state is rebuilt explicitly using
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#manual-development-state-rebuild).
`resume` and `prune-snapshots` were removed with the Upper snapshot lifecycle
(#59/#62, Codeberg #37/#40).

## Build, test, lint

```sh
cargo build -p lattice-cli
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines. For workspace
architecture guidance, see [AGENTS.md](AGENTS.md); for crate-local ownership,
read the `AGENTS.md` in the relevant `crates/<crate>/` directory.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
