<!-- refreshed: 2026-05-17 -->
# Codebase Structure

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## Directory Layout

```text
lattice/
├── Cargo.toml                    # Rust workspace manifest, shared dependency versions
├── Cargo.lock                    # Resolved Rust dependency lockfile
├── AGENTS.md                     # Agent-facing repo instructions
├── CLAUDE.md                     # Claude-facing repo instructions
├── README.md                     # Project overview and crate map
├── scripts/                      # Release verification scripts
│   ├── source-loc-budget.sh      # Strict LOC budget gate (D-10/D-11)
│   └── release-boundary-check.sh # Removed-crate/concept/docs drift gate
├── crates/                       # Workspace member crates (8 total)
│   ├── lattice-hooks/            # Lifecycle hook points
│   ├── lattice-llm/              # Transport, streaming, model resolution, tool types
│   ├── lattice-session/          # Session graph, SQLite persistence, branch refs
│   ├── lattice-profile/          # Agent profile schema, loading, discovery, defaults
│   ├── lattice-tools/            # Tool schemas, executor, enabled-tool resolver
│   ├── lattice-agent/            # AgentLoop, agent state, memory
│   ├── lattice-api/              # Thin session/turn facade
│   └── lattice-cli/              # Binary, commands (run, models, resolve, session, doctor)
├── .planning/codebase/           # Codebase intelligence maps
└── target/                       # Cargo build artifacts (generated, not committed)
```

## Directory Purposes

**Root**

- Purpose: workspace-level coordination and documentation.
- Contains: `Cargo.toml`, `Cargo.lock`, `AGENTS.md`, `CLAUDE.md`, `README.md`.

**`crates/lattice-hooks/`**

- Purpose: lifecycle hook points and hook chain dispatch. No internal crate dependencies.
- Single-file crate: `crates/lattice-hooks/src/lib.rs`.
- Contains: `HookPoint`, `Hook` trait, `CompositeHook`, `HookContext`, `HookResult`.

**`crates/lattice-llm/`**

- Purpose: model-centric foundation crate. No internal crate dependencies.
- Contains:
  - `lib.rs` -- convenience re-exports, `resolve()`, `inspect_model()`, `init_logging()`
  - `catalog/` -- model catalog loader, types (`ResolvedModel`, `ApiProtocol`, `CredentialStatus`)
  - `router.rs` -- `ModelRouter` with alias resolution and credential fallback
  - `invocation.rs` -- `chat()`, `stream()`, `chat_with_effort()`, SSRF protection
  - `transport/` -- transport adapters (Anthropic, OpenAI, Gemini), `TransportDispatcher`
  - `streaming/` -- SSE parsers for Anthropic, OpenAI, Gemini; `StreamEvent` enum
  - `errors.rs` -- `LatticeError` enum, `ErrorClassifier`
  - `retry.rs` -- `RetryPolicy` with jittered backoff
  - `tokens.rs` -- `TokenEstimator`
- Key files: `crates/lattice-llm/src/lib.rs`, `crates/lattice-llm/src/router.rs`, `crates/lattice-llm/src/invocation.rs`, `crates/lattice-llm/src/streaming/`, `crates/lattice-llm/src/transport/`.

**`crates/lattice-session/`**

- Purpose: durable conversation graph with SQLite persistence. No internal crate dependencies.
- Contains:
  - `lib.rs` -- `SessionGraphStore` trait, `SqliteSessionGraphStore`, `SessionApi`, branch/commit/HEAD operations
  - `turn_tx.rs` -- `OpenTurnRequest`, `CommitTurnRequest`, `ProfileSnapshot`, `ProfileDrift`, turn manifest persistence
- Key files: `crates/lattice-session/src/lib.rs`, `crates/lattice-session/src/turn_tx.rs`.

**`crates/lattice-profile/`**

- Purpose: direct `agent.toml` profile schema, loading, discovery, defaults. No internal crate dependencies.
- Contains: `schema.rs`, `load.rs`, `discovery.rs`, `defaults.rs`, `error.rs`.
- Key files: `crates/lattice-profile/src/lib.rs`, `crates/lattice-profile/src/schema.rs`, `crates/lattice-profile/src/discovery.rs`.

**`crates/lattice-tools/`**

- Purpose: built-in tool schemas, enabled-tool resolver, executor with project-root containment. Depends on `lattice-llm`.
- Contains: `enabled_tools.rs`, `executor.rs`, `tool_definitions.rs`, patch engine, `capabilities.rs`.
- Key files: `crates/lattice-tools/src/lib.rs`, `crates/lattice-tools/src/enabled_tools.rs`, `crates/lattice-tools/src/executor.rs`.

**`crates/lattice-agent/`**

- Purpose: multi-turn `AgentLoop` with injectable traits. Depends on `lattice-llm`, `lattice-session`, `lattice-tools`.
- Contains:
  - `agent_loop.rs` -- `AgentLoop`, `LlmClient` trait, `ToolCallExecutor` trait, `TurnDeps` trait
  - `lib.rs` -- public module declarations and re-exports
  - Other modules for state, memory, and loop events
- Key files: `crates/lattice-agent/src/lib.rs`, `crates/lattice-agent/src/agent_loop.rs`.

**`crates/lattice-api/`**

- Purpose: thin public facade for session and turn operations. Depends on `lattice-agent`, `lattice-llm`, `lattice-profile`, `lattice-session`.
- Contains: `SessionApi`, `run_turn`, `stream_turn`, `create_session`, `open_session`.
- Key files: `crates/lattice-api/src/lib.rs`.

**`crates/lattice-cli/`**

- Purpose: binary entry point, command dispatch, config/credentials. Depends on all other workspace crates.
- Contains:
  - `main.rs` -- clap CLI definition, subcommand dispatch for `run`, `models`, `resolve`, `session`, `doctor`
  - `commands/` -- `run.rs`, `models.rs`, `resolve.rs`, `session.rs`, `doctor.rs`
  - `config.rs` -- TOML config loading
  - `credentials.rs` -- API key management
  - `tests/` -- `cli_cutover.rs`, `release_smoke.rs`
- Key files: `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/commands/run.rs`.

## Crate Ownership Map

| Crate | Owns | Add here when |
|-------|------|---------------|
| `lattice-hooks` | Lifecycle hook points and hook chain dispatch | Adding hook points or hook chain behavior |
| `lattice-llm` | Model routing, transport, streaming, common LLM types | Adding provider protocol, catalog handling, or stream parsing |
| `lattice-session` | Conversation persistence, turn transactions, profile snapshots | Adding session refs, graph operations, or turn manifest persistence |
| `lattice-profile` | Profile schema, loading, discovery, defaults | Adding profile fields, validation rules, or discovery paths |
| `lattice-tools` | Tool schemas, enabled-tool resolver, executor | Adding built-in tools, containment rules, or patch behavior |
| `lattice-agent` | AgentLoop, agent state, turn execution | Adding turn-loop behavior, injectable traits, or agent state |
| `lattice-api` | Session/turn facade operations | Adding session or turn API methods |
| `lattice-cli` | Command-line user experience | Adding user commands, config flags, or output formatting |

**Crate dependency summary:**

| Crate | Internal dependencies |
|-------|----------------------|
| `lattice-hooks` | none |
| `lattice-llm` | none |
| `lattice-session` | none |
| `lattice-profile` | none |
| `lattice-tools` | `lattice-llm` |
| `lattice-agent` | `lattice-llm`, `lattice-session`, `lattice-tools` |
| `lattice-api` | `lattice-agent`, `lattice-llm`, `lattice-profile`, `lattice-session` |
| `lattice-cli` | all other workspace crates |

## Key File Locations

**Entry Points**

- `crates/lattice-cli/src/main.rs`: binary command parser and top-level dispatch
- `crates/lattice-api/src/lib.rs`: programmatic API entry

**Configuration**

- `Cargo.toml`: workspace members, Rust version, shared dependencies
- `crates/*/Cargo.toml`: crate-local dependencies and feature flags
- `crates/lattice-cli/src/config.rs`: CLI config file schema/defaults
- `crates/lattice-cli/src/credentials.rs`: credential store/config bridge

**Core Logic**

- `crates/lattice-api/src/lib.rs`: session/turn facade wiring
- `crates/lattice-agent/src/agent_loop.rs`: multi-turn agent loop with injectable traits
- `crates/lattice-llm/src/router.rs`: model alias resolution
- `crates/lattice-llm/src/invocation.rs`: `chat()`, transport dispatch, streaming
- `crates/lattice-tools/src/enabled_tools.rs`: explicit enabled-tool resolver
- `crates/lattice-tools/src/executor.rs`: tool execution with project-root containment
- `crates/lattice-session/src/lib.rs`: session graph store and operations
- `crates/lattice-profile/src/discovery.rs`: profile resolution

**Testing**

- `crates/lattice-cli/tests/cli_cutover.rs`: CLI surface boundary tests
- `crates/lattice-cli/tests/release_smoke.rs`: isolated CLI smoke tests
- Module-local tests live inside `src/*.rs` files in each crate

**Release Verification**

- `scripts/source-loc-budget.sh`: strict LOC budget gate
- `scripts/release-boundary-check.sh`: removed-crate/concept/docs drift checks

## Module Boundary Rules

- **No api/cli imports in lower crates:** `lattice-llm`, `lattice-session`, `lattice-profile`, `lattice-tools`, `lattice-agent`, and `lattice-hooks` must not import from `lattice-api` or `lattice-cli`.
- **Profile schema in `lattice-profile`:** API and CLI consume loaded profiles.
- **LLM transport in `lattice-llm`:** Agent and tools call `lattice_llm::chat()` rather than constructing provider HTTP requests.
- **User-facing rendering in CLI:** Lower crates return typed data or typed errors; CLI formats output.

## Special Directories

**`.planning/codebase`**

- Purpose: codebase intelligence documents.
- Note: `.planning/` is gitignored; files require forced add for commit.

**`target`**

- Purpose: Cargo build artifacts.
- Generated: yes. Committed: no.

**`scripts/`**

- Purpose: release verification scripts for LOC budget, boundary checks, and docs drift.
- Contains: `source-loc-budget.sh`, `release-boundary-check.sh`.

**`.lattice/` (project-local)**

- Purpose: project-local agent profiles overriding global defaults.
- Expected by: `crates/lattice-profile/src/discovery.rs`.
- Expected contents: `.lattice/agents/<name>/agent.toml`.

---

*Structure analysis: 2026-05-17 (post-Phase 11 deletion)*
