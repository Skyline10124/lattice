<!-- refreshed: 2026-05-17 -->
# Architecture

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## System Overview

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                      Entry Layer: CLI                                        │
│  `crates/lattice-cli/src/main.rs`                                           │
└──────────────────────────────────┬───────────────────────────────────────────┘
                                   │
                                   ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│                     Facade Layer: lattice-api                                │
│  `SessionApi` / `run_turn` / `stream_turn`                                  │
│  `crates/lattice-api/src/lib.rs`                                            │
└──────┬───────────┬─────────────┬─────────────┬──────────────────────────────┘
       │           │             │             │
       ▼           ▼             ▼             │
┌──────────────┐ ┌─────────┐ ┌──────────┐     │
│  Agent       │ │ Profile │ │ Session  │     │
│  AgentLoop   │ │ Loader  │ │ Graph    │     │
│  `agent`     │ │`profile`│ │`session` │     │
└──┬───┬───────┘ └─────────┘ └──────────┘     │
   │   │                                       │
   ▼   ▼                                       │
┌──────────────┐ ┌─────────┐                   │
│  LLM         │ │ Tools   │                   │
│  Transport   │ │ Executor│                   │
│  Router      │ │`tools`  │                   │
│ `llm`        │ └─────────┘                   │
└──────────────┘                               │
                                               │
┌──────────────┐                               │
│  Hooks       │◄──────────────────────────────┘
│  `hooks`     │
└──────────────┘
```

## Component Responsibilities

| Component | Responsibility | Evidence |
|-----------|----------------|----------|
| `lattice-hooks` | Lifecycle hook points (BeforeAgentTurn, AfterAgentTurn, BeforeToolCall, AfterToolCall, BeforeInference, AfterInference, SessionStart) and hook chain dispatch. No internal crate dependencies. | `crates/lattice-hooks/src/lib.rs` |
| `lattice-llm` | Model alias resolution, provider credential fallback, transport dispatch (Anthropic/OpenAI/Gemini), normalized SSE streaming events, token estimation, retry policy, error classification, logging. No internal crate dependencies. | `crates/lattice-llm/src/lib.rs`, `crates/lattice-llm/src/router.rs`, `crates/lattice-llm/src/invocation.rs`, `crates/lattice-llm/src/streaming/` |
| `lattice-session` | Git-style conversation graph with SQLite persistence (SqliteSessionGraphStore), commits, branches, refs, turn manifests, profile snapshots. No internal crate dependencies. | `crates/lattice-session/src/lib.rs`, `crates/lattice-session/src/turn_tx.rs` |
| `lattice-profile` | Direct `agent.toml` profile schema, loading, discovery, defaults. No internal crate dependencies. | `crates/lattice-profile/src/lib.rs`, `crates/lattice-profile/src/schema.rs`, `crates/lattice-profile/src/discovery.rs` |
| `lattice-tools` | Built-in tool schema inventory, enabled-tool resolver, executor with project-root containment, structured patch engine. Depends on `lattice-llm`. | `crates/lattice-tools/src/lib.rs`, `crates/lattice-tools/src/enabled_tools.rs`, `crates/lattice-tools/src/executor.rs` |
| `lattice-agent` | Multi-turn `AgentLoop` with injectable `LlmClient` and `ToolCallExecutor` async traits, agent state, memory, turn execution. Depends on `lattice-llm`, `lattice-session`, `lattice-tools`. | `crates/lattice-agent/src/lib.rs`, `crates/lattice-agent/src/agent_loop.rs` |
| `lattice-api` | Thin public facade for session creation, turn execution, streaming, and session operations (status, log, checkout, branch, diff, reset). Depends on `lattice-agent`, `lattice-llm`, `lattice-profile`, `lattice-session`. | `crates/lattice-api/src/lib.rs` |
| `lattice-cli` | Binary entry point (clap), config, credentials, subcommand dispatch for `run`, `models`, `resolve`, `session`, and `doctor`. Depends on all other workspace crates. | `crates/lattice-cli/src/main.rs` |

## Pattern Overview

**Overall pattern:** layered Rust workspace with direct profile-to-session-to-agent execution. v1.1 "Cut Off Cleverness" removed the orchestrator, plugin, prompt compiler, sandbox, and policy layers, replacing them with a thin API facade and simple project-root tool containment.

**Key characteristics:**

- The dependency flow is `hooks -> llm -> session/profile/tools -> agent -> api -> cli`.
- `lattice-api` is the public facade. It owns `SessionApi`, `run_turn`, and `stream_turn` without long-running runtime handles or policy logic.
- `AgentLoop` uses injectable `LlmClient` and `ToolCallExecutor` async traits for testability. The loop repeats until a final assistant response or terminal error.
- Tool safety is simple in v1.1: project-root containment, no `..` traversal, file-size limits, and profile-gated optional shell/web tools.
- `lattice-profile` defines the v1 `agent.toml` contract with direct parsing (no lowering pipeline or drift detection).
- Removed: pipeline, event bus, micro-agents, plugin DAG, sandbox crate, policy crate, prompt compiler, and terminal UI (see Future/Deferred section below).

## Crate Dependency Graph

```text
    ┌────────────────┐  ┌────────────────┐  ┌────────────────┐  ┌────────────────┐
    │ lattice-hooks  │  │  lattice-llm   │  │lattice-session │  │lattice-profile │
    │  (no deps)     │  │  (no deps)     │  │  (no deps)     │  │  (no deps)     │
    └────────────────┘  └───────┬────────┘  └────────────────┘  └────────────────┘
                               │
                               ▼
                        ┌──────────────┐
                        │lattice-tools │
                        │(deps: llm)   │
                        └──────┬───────┘
                               │
              ┌────────────────┤
              ▼                │
       ┌──────────────┐       │
       │lattice-agent │       │
       │(deps: llm,   │       │
       │ session,     │       │
       │ tools)       │       │
       └──────┬───────┘       │
              │               │
              ▼               │
       ┌──────────────┐       │
       │ lattice-api  │       │
       │(deps: agent, │       │
       │ llm, profile,│       │
       │ session)     │       │
       └──────┬───────┘       │
              │               │
              ▼               ▼
       ┌──────────────────────────────────┐
       │          lattice-cli              │
       │  (deps: all other workspace      │
       │   crates)                         │
       └──────────────────────────────────┘
```

## Layers

**Hook Layer**

- Purpose: lifecycle hook points for agent turns, tool calls, inference, and sessions.
- Location: `crates/lattice-hooks/src/`
- Contains: `HookPoint`, `PromptHookPoint`, `Hook` trait, `CompositeHook`, `HookContext`, `HookResult`.
- Depends on: no internal crates.
- Used by: `lattice-cli` (hook registration).

**LLM Layer**

- Purpose: resolve model aliases into `ResolvedModel`, choose provider/protocol/credentials, normalize provider streaming, classify errors.
- Location: `crates/lattice-llm/src/`
- Contains: `ModelRouter`, `ResolvedModel`, `ApiProtocol`, `TransportDispatcher`, `StreamEvent`, `chat()`, `stream()`, `LatticeError`, `RetryPolicy`, `TokenEstimator`.
- Depends on: foundational external crates (reqwest, serde, tokio, tracing).
- Used by: tools, agent, api, cli.

**Session Layer**

- Purpose: Git-style conversation graph with SQLite persistence. Commits, branches, refs, turn manifests, profile snapshots.
- Location: `crates/lattice-session/src/`
- Contains: `SessionGraphStore` trait, `SqliteSessionGraphStore`, `SessionApi`, `OpenTurnRequest`, `CommitTurnRequest`, `ProfileSnapshot`, `ProfileDrift`.
- Depends on: no internal crates.
- Used by: agent, api, cli.

**Profile Layer**

- Purpose: direct `agent.toml` profile schema, loading, discovery, defaults.
- Location: `crates/lattice-profile/src/`
- Contains: `AgentProfile` (schema), `ProfileSource`, `DiscoveryPaths`, `load_agent_profile()`, `resolve_agent_profile()`, `default_agent_template()`.
- Depends on: no internal crates.
- Used by: api, cli.

**Tools Layer**

- Purpose: expose callable tools, execute tools with project-root containment, resolve enabled tools.
- Location: `crates/lattice-tools/src/`
- Contains: `ToolExecutor` trait, `DefaultToolExecutor`, `enabled_tools()`, tool definitions, patch engine.
- Depends on: `lattice-llm`.
- Used by: agent, cli.

**Agent Layer**

- Purpose: multi-turn `AgentLoop` with injectable LLM client and tool executor, agent state, memory, turn execution.
- Location: `crates/lattice-agent/src/`
- Contains: `AgentLoop`, `LlmClient` trait, `ToolCallExecutor` trait, `AgentState`, `LoopEvent`, `TurnDeps`.
- Depends on: `lattice-llm`, `lattice-session`, `lattice-tools`.
- Used by: api, cli.

**API Facade Layer**

- Purpose: thin public facade for session and turn operations.
- Location: `crates/lattice-api/src/`
- Contains: `SessionApi`, `run_turn`, `stream_turn`, `create_session`, `open_session`.
- Depends on: `lattice-agent`, `lattice-llm`, `lattice-profile`, `lattice-session`.
- Used by: `lattice-cli`.

**CLI Layer**

- Purpose: translate config, credentials, and user commands into API calls.
- Location: `crates/lattice-cli/src/`
- Contains: `main.rs`, `commands/` (run, models, resolve, session, doctor), `config.rs`, `credentials.rs`.
- Depends on: all other workspace crates.
- Used by: the `lattice` binary.

## Data Flow

### Primary Turn Execution Path

1. CLI parses `run` subcommand, loads `.env`, config, and credentials.
2. CLI calls `lattice_api::run_turn` or `lattice_api::stream_turn` with the prompt, model alias, and profile.
3. `lattice-api` resolves the profile, opens or creates a session via `SessionGraphStore`, and assembles an `AgentLoop`.
4. `AgentLoop` reads current session state, builds messages, calls the LLM via `LlmClient`, executes requested tools via `ToolCallExecutor`, and repeats until a final assistant response is produced.
5. Turn outcomes are persisted as commits in the session graph via `SessionGraphStore`.
6. CLI renders events and requires successful completion.

### Model Resolution Flow

1. `lattice-llm` `ModelRouter::resolve()` normalizes model IDs, resolves aliases, chooses provider, checks credentials.
2. `TransportDispatcher` selects OpenAI, Anthropic, or Gemini transport from `ApiProtocol`.
3. Provider-specific SSE parsers normalize streaming bodies into `StreamEvent` variants.

## Architectural Constraints

- **Dependency direction:** `hooks -> llm -> session/profile/tools -> agent -> api -> cli`. Lower crates must not import upper crates.
- **No multi-agent coordination:** v1.1 runs exactly one agent per turn (removed: pipeline, event bus, multi-agent).
- **Profile ownership:** Profile schema and loading belong in `lattice-profile`. API and CLI consume loaded profiles.
- **Tool safety is simple:** Project-root containment, path validation, and file-size limits. No sandbox crate or policy model.
- **API is the public facade:** `lattice-api` owns session and turn operations. CLI calls `lattice-api`, not agent or session directly for turn execution.

## Future/Deferred Work

The following were removed in v1.1 and may be reintroduced in later milestones:

- **TUI (deferred)** — A minimal terminal UI rebuilt on `lattice-api`.
- **Rich prompt compilation (deferred)** — Layer priorities, token budgets, and provider-specific assembly.
- **Redesigned security model (deferred)** — A new sandbox and approval system on the simplified substrate.
- **Plugins and orchestration (deferred)** — Plugin contracts and DAG runners for multi-agent coordination.
- **Swarm and multi-agent collaboration (deferred)** — Session-graph-based coordination features.

---

*Architecture analysis: 2026-05-17 (post-Phase 11 deletion)*
