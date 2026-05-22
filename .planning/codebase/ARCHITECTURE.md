<!-- refreshed: 2026-05-19 -->
# Architecture

**Analysis Date:** 2026-05-19

## System Overview

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                         Entry Layer: CLI binary                              │
│  `crates/lattice-cli/src/main.rs`  (clap subcommands)                        │
│  run / models / resolve / session / doctor                                   │
└──────────────────────────────────┬───────────────────────────────────────────┘
                                   │
                                   ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│                       Facade Layer: lattice-api                              │
│  `SessionApi` (sessions)  `run_turn` / `stream_turn` (turns)                 │
│  `crates/lattice-api/src/lib.rs`  `crates/lattice-api/src/session.rs`        │
│  `crates/lattice-api/src/turn/mod.rs`                                        │
└──────┬─────────────────┬────────────────┬───────────────────────────────────-┘
       │                 │                │
       ▼                 ▼                ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│  Agent       │  │  Profile     │  │  Session     │
│  AgentLoop   │  │  Loader      │  │  Graph store │
│ `agent_loop` │  │ `profile`    │  │ `session`    │
└──┬───┬───────┘  └──────────────┘  └──────────────┘
   │   │
   ▼   ▼
┌──────────────┐  ┌──────────────┐
│  LLM         │  │  Tools       │
│  Router      │  │  Executor    │
│  Stub chat() │  │  Patch eng.  │
│ `llm`        │  │ `tools`      │
└──────────────┘  └──────────────┘
       │                 │
       └─────────┬───────┘
                 ▼
        (Provider HTTP / SSE
         in `llm/providers/`,
         `llm/sse.rs`)

┌──────────────────────────────────────────────────────────────────────────────┐
│  Persistence: SQLite session graph (rusqlite, bundled)                       │
│  `crates/lattice-session/src/sqlite_graph.rs`                                │
└──────────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────────┐
│  Standalone (not wired into binary): `lattice-hooks` lifecycle vocabulary    │
│  `crates/lattice-hooks/src/lib.rs`                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

## Component Responsibilities

| Component | Responsibility | File |
|-----------|----------------|------|
| `lattice-hooks` | Lifecycle hook vocabulary (`HookPoint`, `Hook`, `HookChain`, `InMemoryHookRegistry`). Standalone crate, not a workspace member, not used by api/cli/agent today. | `crates/lattice-hooks/src/lib.rs` |
| `lattice-llm` | Vocabulary types (`Message`, `ChatRequest`, `ChatResponse`, `LlmEvent`, `ToolCall`, `ToolDefinition`, `ModelTarget`, `TokenUsage`), `ModelRouter` alias table, base-URL SSRF validation, SSE framing, provider modules, deterministic `StubLlm` and stub-backed `chat()`/`stream()` entry points. No internal crate dependencies. | `crates/lattice-llm/src/lib.rs`, `crates/lattice-llm/src/router.rs`, `crates/lattice-llm/src/types.rs`, `crates/lattice-llm/src/stub.rs`, `crates/lattice-llm/src/security.rs`, `crates/lattice-llm/src/sse.rs`, `crates/lattice-llm/src/providers/` |
| `lattice-session` | Git-style conversation graph with SQLite persistence (`SqliteSessionGraphStore`), commit/branch/HEAD operations, `TurnCommitInput`/`TurnCommit`, transcript reads. No internal crate dependencies. | `crates/lattice-session/src/lib.rs`, `crates/lattice-session/src/graph.rs`, `crates/lattice-session/src/sqlite_graph.rs` |
| `lattice-profile` | `agent.toml` schema (`AgentProfile`, `AgentSection`, `PromptSection`, `ToolsSection`), strict allow-list loading, discovery, default templates. No internal crate dependencies. | `crates/lattice-profile/src/lib.rs`, `crates/lattice-profile/src/schema.rs`, `crates/lattice-profile/src/discovery.rs`, `crates/lattice-profile/src/load.rs`, `crates/lattice-profile/src/defaults.rs` |
| `lattice-tools` | `ToolExecutor` async trait, `DefaultToolExecutor` with project-root containment, enabled-tool resolver, structured patch engine (`edit/`), tool schema inventory. Depends on `lattice-llm`. | `crates/lattice-tools/src/lib.rs`, `crates/lattice-tools/src/enabled_tools.rs`, `crates/lattice-tools/src/executor/mod.rs`, `crates/lattice-tools/src/edit/mod.rs`, `crates/lattice-tools/src/tool_definitions.rs` |
| `lattice-agent` | `AgentLoop` with injectable `LlmClient` and `ToolCallExecutor` traits, multi-round streaming LLM-and-tool loop, cooperative cancellation and deadline support, `MAX_TURN_ROUNDS=200` safety cap. Depends on `lattice-llm`, `lattice-session`, `lattice-tools`. | `crates/lattice-agent/src/lib.rs`, `crates/lattice-agent/src/agent_loop/mod.rs`, `crates/lattice-agent/src/agent_loop/types.rs`, `crates/lattice-agent/src/agent_loop/traits.rs`, `crates/lattice-agent/src/agent_loop/messages.rs` |
| `lattice-api` | Public facade for sessions (`SessionApi`) and turns (`run_turn`, live `stream_turn`) with a buffered `TurnDeps` seam for tests, stage breadcrumbs (`RunTurnStage`), `TurnEvent` stream mapping, atomic turn commit. Depends on `lattice-agent`, `lattice-llm`, `lattice-profile`, `lattice-session`. | `crates/lattice-api/src/lib.rs`, `crates/lattice-api/src/session.rs`, `crates/lattice-api/src/turn/mod.rs`, `crates/lattice-api/src/turn/execute.rs`, `crates/lattice-api/src/turn/deps.rs`, `crates/lattice-api/src/turn/types.rs`, `crates/lattice-api/src/turn/mapping.rs` |
| `lattice-cli` | Binary entry, clap subcommands (`run`, `models`, `resolve`, `session`, `doctor`), `.env` loading via `dotenvy`, JSON/verbose flags, terminal output formatting. Depends on `lattice-api`, `lattice-llm`, `lattice-session`, `lattice-agent`, `lattice-tools`, `lattice-profile`. | `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/commands/mod.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/commands/models.rs`, `crates/lattice-cli/src/commands/resolve.rs`, `crates/lattice-cli/src/commands/session.rs`, `crates/lattice-cli/src/commands/doctor.rs` |

## Pattern Overview

**Overall:** Layered Rust workspace with a stub-backed LLM core, a SQLite session graph, and a thin API facade between the CLI and the agent loop.

**Key Characteristics:**

- One-way dependency direction: `llm -> session/profile -> tools -> agent -> api -> cli`. `lattice-hooks` exists as a standalone crate but is currently unused by all other workspace members.
- The agent loop is the only multi-round component. It is stateless across turns; per-turn state lives in `AgentTurnRequest`.
- LLM calls are abstracted behind the agent's `LlmClient` trait. Production `lattice_llm::chat()` is currently the deterministic `StubLlm` (`crates/lattice-llm/src/stub.rs`); provider HTTP/SSE code (`providers/anthropic.rs`, `providers/openai.rs`, `providers/gemini.rs`, `client.rs`, `security.rs`) supports model resolution, validation, and SSE framing but is not driving live model calls in the default path.
- Tool safety is intentionally minimal: project-root containment, no `..` traversal, file-size limits (`ToolLimits`), no command output beyond `MAX_COMMAND_OUTPUT_SIZE = 200 KiB`. There is no sandbox crate, approval flow, or policy engine.
- Sessions are persisted as immutable turn commits in SQLite (`SqliteSessionGraphStore`). Turn commits include the parent commit, full message list, profile snapshot id, model, provider, status, usage, and terminal reason.
- `lattice-api` exposes only typed request/response/event values. The CLI does not import `lattice-agent` or `lattice-session` directly for turn execution paths (it does import them for type re-use and the `session` subcommand wiring).

## Layers

**LLM Layer**

- Purpose: shared LLM vocabulary, model alias resolution, SSRF base-URL validation, SSE framing, provider request and parser modules, stub-backed entrypoints.
- Location: `crates/lattice-llm/src/`
- Contains: `ModelRouter`, `ProviderKind`, `ResolvedModel`, `CredentialStatus`, `Message`, `Role`, `ChatRequest`, `ChatResponse`, `LlmEvent`, `ToolCall`, `ToolDefinition`, `ModelTarget`, `TokenUsage`, `FunctionCall`, `StubLlm`, `LlmError`, `LlmStream`, `chat()`, `stream()`, `init_logging`.
- Depends on: `reqwest`, `serde`, `futures`, `tracing-subscriber`, `url`, `bytes` (no internal crates).
- Used by: `lattice-tools`, `lattice-agent`, `lattice-api`, `lattice-cli`.

**Session Layer**

- Purpose: durable git-style conversation graph with SQLite persistence.
- Location: `crates/lattice-session/src/`
- Contains: `SessionGraphStore` trait, `SqliteSessionGraphStore`, `GraphSession`, `GraphRef`, `GraphHead`, `GraphMessage`, `GraphToolCall`, `HeadTranscript`, `TurnCommit`, `TurnCommitInput`, `TurnCommitStatus`, `TurnLogEntry`, `TurnUsage`, `GraphDiff`, `ResetResult`, `SessionGraphSummary`, `SessionStatus`, `derive_commit_message`, `canonical_turn_seed`, `validate_branch_name`.
- Depends on: `rusqlite` (bundled), `sha2`, `chrono`, `serde`, `serde_json`, `uuid` (no internal crates).
- Used by: `lattice-agent`, `lattice-api`, `lattice-cli`.

**Profile Layer**

- Purpose: strict `agent.toml` schema and read-only loading.
- Location: `crates/lattice-profile/src/`
- Contains: `AgentProfile`, `AgentSection`, `PromptSection`, `ToolsSection`, `LoadedAgentProfile`, `ProfileSource`, `DiscoveryPaths`, `ProfileError`, `load_agent_profile`, `resolve_agent_profile`, `default_agent_template`, plus `default_dev_template`, `default_editor_template`, `default_reader_template`.
- Depends on: `serde`, `toml`, `thiserror` (no internal crates).
- Used by: `lattice-api`, `lattice-cli`.

**Tools Layer**

- Purpose: tool schema inventory, project-root-bounded execution, structured patch engine.
- Location: `crates/lattice-tools/src/`
- Contains: `ToolExecutor` trait, `DefaultToolExecutor`, `ToolLimits`, `default_tool_definitions`, `resolve_enabled_tools`, `EnabledTools`, `EnabledToolError`, `PatchDocument` and patch types (`PatchHunk`, `PatchOperation`, `PatchResultEnvelope`, `PatchErrorCode`, `PatchDiagnostic`, `ChangedFile`, `AttemptedOperation`, `FileOperationKind`).
- Depends on: `lattice-llm`, `reqwest`, `regex`, `url`, `blake3`, `tokio`, `tracing`, `thiserror`, `indexmap`.
- Used by: `lattice-agent`, `lattice-cli`.

**Agent Layer**

- Purpose: turn execution loop with injectable LLM client and tool executor.
- Location: `crates/lattice-agent/src/`
- Contains: `AgentLoop`, `AgentLoopConfig`, `LlmClient` trait, `ToolCallExecutor` trait (re-exported `lattice_tools::ToolExecutor`), `AgentTurnRequest`, `AgentTurnResult`, `AgentTurnStatus`, `AgentTurnEvent`, `AgentToolResultSummary`, `MAX_TURN_ROUNDS = 200`.
- Depends on: `lattice-llm`, `lattice-session`, `lattice-tools`, `async-trait`, `tokio`.
- Used by: `lattice-api`, `lattice-cli`.

**API Facade Layer**

- Purpose: public typed entry points for session and turn operations.
- Location: `crates/lattice-api/src/`
- Contains: `SessionApi`, `CreateSessionRequest`, `OpenSessionRequest`, `ApiError`, `run_turn`, `stream_turn`, `RunTurnRequest`, `RunTurnResult`, `RunTurnStage` (`Profile` / `Session` / `Agent` / `Llm` / `Tools` / `Commit` / `Render`), `RunTurnStatus`, `StreamTurnRequest`, `TurnEvent`, `TurnEventStream`, `TurnUsageSummary`, `ToolResultSummary`, `TurnDeps` seam (test-only) and `RealTurnDeps`.
- Depends on: `lattice-agent`, `lattice-llm`, `lattice-profile`, `lattice-session`, `serde`, `serde_json`, `thiserror`, `futures`, `uuid`, `async-trait`.
- Used by: `lattice-cli`.

**CLI Layer**

- Purpose: binary entry, command dispatch, terminal rendering.
- Location: `crates/lattice-cli/src/`
- Contains: `Cli` clap parser with subcommands `Run`, `Models`, `Resolve`, `Session { action }`, `Doctor`. Top-level `--json` and `--verbose` flags. `tokio::main` runtime. Logging initialized via `lattice_llm::init_logging`.
- Depends on: all other workspace crates except `lattice-hooks`.

**Hooks Layer (standalone)**

- Purpose: lifecycle hook vocabulary for future use.
- Location: `crates/lattice-hooks/src/lib.rs`
- Contains: `HookPoint` (`AgentTurnStart`/`AgentTurnEnd`/`ToolCallStart`/`ToolCallEnd`/`InferenceStart`/`InferenceEnd`/`SessionStart`/`SessionEnd`/`Custom`), `Hook` trait, `HookChain`, `HookContext`, `HookDecision`, `HookExecutionRecord`, `HookDispatch`, `HookRegistry` trait, `InMemoryHookRegistry`, `HookError`.
- Depends on: `serde`, `serde_json`, `async-trait`, `tokio`, `thiserror` (no internal crates).
- Used by: nothing today (the crate is not a workspace member, no in-tree code imports it).

## Crate Dependency Graph

```text
    ┌────────────────┐  ┌────────────────┐  ┌────────────────┐
    │  lattice-llm   │  │lattice-session │  │lattice-profile │
    │  (no deps)     │  │  (no deps)     │  │  (no deps)     │
    └───────┬────────┘  └────────┬───────┘  └────────┬───────┘
            │                    │                    │
            ▼                    │                    │
     ┌──────────────┐            │                    │
     │lattice-tools │            │                    │
     │ (deps: llm)  │            │                    │
     └──────┬───────┘            │                    │
            │                    │                    │
            ▼                    ▼                    ▼
        ┌─────────────────────────────────────────────────┐
        │ lattice-agent  (deps: llm, session, tools)      │
        └─────────────────────┬───────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────────┐
        │ lattice-api    (deps: agent, llm, profile,      │
        │                       session)                  │
        └─────────────────────┬───────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────────────┐
        │ lattice-cli    (deps: api, agent, llm, profile, │
        │                       session, tools)           │
        └─────────────────────────────────────────────────┘

   ┌────────────────┐
   │ lattice-hooks  │ standalone, NOT a workspace member, NO consumers
   │  (no deps)     │
   └────────────────┘
```

Workspace `[workspace] members` (`Cargo.toml`) lists exactly seven crates: `lattice-llm`, `lattice-api`, `lattice-session`, `lattice-agent`, `lattice-tools`, `lattice-cli`, `lattice-profile`. `lattice-hooks` is referenced only in `[workspace.dependencies]`.

## Data Flow

### Primary Turn Execution Path (`lattice-api::run_turn`)

1. CLI parses `Run { prompt, model, profile, session }` and calls `commands::run::run`. (`crates/lattice-cli/src/main.rs:84`, `crates/lattice-cli/src/commands/run.rs`)
2. `run_turn(request)` constructs `RealTurnDeps` and invokes `execute_turn_inner`. (`crates/lattice-api/src/turn/execute.rs:16`)
3. Stage `Profile`: `resolve_model_and_prompt` derives the model alias, system prompt, and provider override from the loaded profile. (`crates/lattice-api/src/turn/mapping.rs`)
4. Stage `Session`: open existing session by id or create a new one via `SessionApi`, then read `head_transcript`. (`crates/lattice-api/src/session.rs:71`, `crates/lattice-api/src/turn/execute.rs:31`)
5. Stage `Agent`: `build_agent_request` assembles `AgentTurnRequest`, then `deps.execute_agent_turn` drives `AgentLoop::run_turn`. (`crates/lattice-agent/src/agent_loop/mod.rs:72`)
6. Inside the loop: build messages (`system + transcript + user_input`), call `LlmClient::stream_chat`, emit process events while consuming the stream, accumulate usage, append assistant/tool-call messages, execute requested tool calls sequentially via `ToolCallExecutor::execute`, append tool messages, repeat until no tool calls or terminal status. Hard cap `MAX_TURN_ROUNDS = 200`.
7. Stages `Llm` and `Tools` are recorded as breadcrumbs after the agent returns. (`crates/lattice-api/src/turn/execute.rs:58`)
8. Stage `Commit`: `derive_commit_message` summarizes the new messages; `commit_turn(TurnCommitInput)` atomically appends the commit and advances HEAD/branch ref. (`crates/lattice-session/src/sqlite_graph.rs`, `crates/lattice-api/src/turn/execute.rs:96`)
9. Stage `Render`: `RunTurnResult` returned to CLI for terminal output.

### Streaming Turn Path (`lattice-api::stream_turn`)

- `stream_turn` prepares the session/head synchronously, then spawns one live task with a bounded event channel. Agent process events are mapped 1:1 into public `TurnEvent` values (`ModelStarted`, `ReasoningDelta`, `TokenDelta`, `ToolCallStarted`, `ToolCallFinished`, `ModelFinished`) before terminal and commit events are derived.
- Terminal public events (`TurnFailed`, `TurnCancelled`) are derived from `AgentTurnResult.status`; commit lifecycle events are `TurnCommitting`, `TurnCommitted`, and `CommitFailed`. The stream is `Pin<Box<dyn Stream<Item = TurnEvent> + Send>>`. `stream_turn_with_deps` remains a buffered replay seam for tests only.

### Session Operations (read-only and pointer-only)

- `SessionApi::status` / `list_sessions` / `log` / `checkout` / `branch` / `diff` / `reset` delegate to `SqliteSessionGraphStore`. The CLI `session` subcommand maps directly to these methods. (`crates/lattice-api/src/session.rs:77`, `crates/lattice-cli/src/commands/session.rs`)

### Model Resolution Flow

1. `ModelRouter::resolve(target)` looks up the alias in a fixed alias table and returns `ResolvedModel { alias, provider, provider_model, base_url, api_key_env, credential_status }`. (`crates/lattice-llm/src/router.rs:75`)
2. `CredentialStatus::from_env(env_var)` reports whether the API key is present.
3. `lattice_llm::security::validate_base_url` enforces HTTPS-only for non-loopback hosts and rejects private/reserved IPs before any HTTP call. (`crates/lattice-llm/src/security.rs`)
4. `client::validate_and_read_key` and `build_auth_headers` build provider-specific auth headers; provider-specific request shaping and SSE parsing live in `providers/{anthropic,openai,gemini}.rs`.

**State management:**

- No long-lived runtime handle; `SessionApi` holds a `SqliteSessionGraphStore` value (file-backed). The CLI constructs a `SessionApi` per command.
- The agent loop holds only its `AgentLoopConfig` (`Arc<dyn LlmClient>` and `Arc<dyn ToolCallExecutor>`).
- Cancellation is via an optional `Arc<AtomicBool>` flag in `AgentTurnRequest`.

## Key Abstractions

**`AgentLoop`**

- Purpose: drive one turn of LLM-and-tool execution.
- File: `crates/lattice-agent/src/agent_loop/mod.rs`
- Pattern: configuration-by-injection (`Arc<dyn LlmClient>`, `Arc<dyn ToolCallExecutor>`); per-turn state passed in `AgentTurnRequest`; events accumulated in `AgentTurnResult.events`.

**`SessionApi` and `SessionGraphStore`**

- Purpose: durable session creation, lookup, and pointer movement.
- Files: `crates/lattice-api/src/session.rs`, `crates/lattice-session/src/graph.rs`, `crates/lattice-session/src/sqlite_graph.rs`
- Pattern: trait + concrete SQLite implementation; `SessionApi` is a thin facade that maps `SessionError` to `ApiError`.

**`ResolvedModel` and `ModelRouter`**

- Purpose: alias-to-provider mapping and credential status.
- File: `crates/lattice-llm/src/router.rs`
- Pattern: fixed alias table; no catalog file or prefix parsing.

**`LlmEvent` (and `TurnEvent`)**

- Purpose: streaming vocabulary at two layers.
- Files: `crates/lattice-llm/src/types.rs` (`LlmEvent`), `crates/lattice-api/src/turn/types.rs` (`TurnEvent`)
- Pattern: typed enums; agent loop emits internal `AgentTurnEvent`; `lattice-api` maps to `TurnEvent` for streaming consumers.

**`TurnDeps` seam**

- Purpose: testable dependency injection for `run_turn` / `stream_turn`.
- File: `crates/lattice-api/src/turn/deps.rs`
- Pattern: `RealTurnDeps` for production; test fakes inject `LlmClient` and `ToolCallExecutor` to avoid live HTTP and SQLite.

## Entry Points

**Binary**

- `crates/lattice-cli/src/main.rs` — clap parser, `tokio::main`, dispatches to `commands::{run,models,resolve,session,doctor}`.

**Programmatic**

- `lattice_api::SessionApi::new`, `SessionApi::sqlite(dir)` — session operations.
- `lattice_api::run_turn(request)` / `lattice_api::stream_turn(request)` — turn execution.

**Tests**

- `crates/lattice-cli/tests/cli_cutover.rs`, `crates/lattice-cli/tests/release_smoke.rs` — CLI surface and smoke tests.
- `crates/lattice-agent/tests/agent_loop.rs` — agent-loop integration tests.
- `crates/lattice-api/tests/usage_turn.rs` — turn usage end-to-end test.
- `crates/lattice-tools/tests/executor.rs` — tool executor integration tests.

## Architectural Constraints

- **Threading:** single Tokio runtime in the CLI binary (`#[tokio::main]`). Tool calls inside a turn execute sequentially; no parallel tool fan-out. The agent loop is fully async and may be driven from any Tokio context.
- **Global state:** none. No statics or `OnceCell` singletons in production crates. `lattice_llm::init_logging` configures a tracing subscriber once per process.
- **Circular imports:** none. The dependency graph is acyclic; `lattice-tools` re-exports `ToolExecutor` to `lattice-agent` to avoid agent code importing tool internals beyond the trait.
- **`MAX_TURN_ROUNDS = 200`:** hard cap on LLM rounds per turn, enforced in `agent_loop::run_turn_inner`. Acts as a safety net when caller-supplied timeout/cancel are missing.
- **Tool containment:** `DefaultToolExecutor` enforces project-root containment via `executor::path_guard` and rejects `..`. File reads/writes capped by `ToolLimits`. Command output capped at `MAX_COMMAND_OUTPUT_SIZE = 200 KiB`.
- **HTTP safety:** `lattice_llm::security::validate_base_url` rejects non-HTTPS public hosts and private IPs before any provider call. `reqwest` clients in `lattice-tools` disable redirects and cap response sizes.
- **API surface stability:** `crates/lattice-api/src/session.rs` includes a compile-time assertion (`session_api_does_not_expose_legacy_operations`) that `SessionApi` exposes only the documented method set.

## Anti-Patterns

### Importing `lattice-session` types from CLI for turn execution

**What happens:** new commands could grow direct `SqliteSessionGraphStore` calls for new turn flows.
**Why it's wrong:** it bypasses the `lattice-api` facade, the `RunTurnStage` breadcrumbs, and the `TurnEvent` mapping. Tests would then need to fake `SqliteSessionGraphStore` rather than the `TurnDeps` seam.
**Do this instead:** add the new operation to `lattice-api` (extend `SessionApi` for session-level ops or `run_turn`/`stream_turn` for turn-level ops) and call from CLI. See `crates/lattice-api/src/turn/execute.rs` for the canonical wiring.

### Adding a new tool by hand-rolling JSON in the agent loop

**What happens:** ad-hoc tool definitions inlined into agent code.
**Why it's wrong:** the loop relies on `resolve_enabled_tools(&request.enabled_tools)` returning typed `EnabledTools` with both `definitions` (`Vec<ToolDefinition>`) and the executor wiring. Bypassing it loses validation and breaks the explicit allow-list.
**Do this instead:** add the tool definition to `crates/lattice-tools/src/tool_definitions.rs` and the execution branch to `crates/lattice-tools/src/executor/`. The agent loop picks it up automatically once the profile lists the new name.

### Using `unwrap()` on agent-loop error paths

**What happens:** panic instead of producing an `Error` `AgentTurnResult`.
**Why it's wrong:** agent-loop turn failures must surface as a structured `AgentTurnResult` with `status = Error` and `terminal_reason = Some(...)`; the API derives public terminal events and can persist an error commit.
**Do this instead:** return early via the `match`/`?`-equivalent error arms in `agent_loop::run_turn_inner` (see `crates/lattice-agent/src/agent_loop/mod.rs:142`-`195`).

### Bypassing the SSRF guard for new HTTP clients

**What happens:** a new provider or tool builds a `reqwest::Client` without `validate_base_url`.
**Why it's wrong:** loses the SSRF floor (no private/reserved IPs, HTTPS-only for public hosts).
**Do this instead:** call `lattice_llm::security::validate_base_url` for any URL controlled by user/profile config, and disable redirects on `reqwest::Client::builder()` (`crates/lattice-tools/src/executor/mod.rs:55`).

## Error Handling

**Strategy:** typed enums per layer with `thiserror`; layers map errors at boundaries.

- `lattice_llm::LlmError` (provider/auth/transport).
- `lattice_session::SessionError` (`Io`, `Database`, `InvalidBranchName`, `NotFound`, `Conflict`, `MissingCommit`).
- `lattice_profile::ProfileError`.
- `lattice_tools::ToolError` and `EnabledToolError`.
- `lattice_api::ApiError` (`NotFound`, `Internal`).

**Patterns:**

- Agent loop never panics: every error path produces an `AgentTurnResult` with `status = Error`/`Cancelled` and a `terminal_reason`.
- API maps `SessionError::NotFound` to `ApiError::NotFound`; everything else becomes `ApiError::Internal`.
- CLI converts errors to `ExitCode` and prints colored `error: ...` lines via `colored`.

## Cross-Cutting Concerns

**Logging:** `tracing` everywhere, `tracing-subscriber` initialized once via `lattice_llm::init_logging(verbose)` from `crates/lattice-cli/src/main.rs:71`. Log level controlled via `RUST_LOG` and the `--verbose` flag.

**Validation:** strict allow-list in `lattice-profile` (no extra TOML fields), branch name validation in `lattice_session::validate_branch_name`, base-URL validation in `lattice_llm::security::validate_base_url`, path containment in `lattice_tools::executor::path_guard`.

**Authentication:** API keys read from environment by name (`api_key_env` in `ResolvedModel`). `.env` loaded via `dotenvy::dotenv()` at CLI entry. No credentials at rest in this codebase.

**Cancellation/timeouts:** caller-supplied `cancel_flag: Option<Arc<AtomicBool>>` and `timeout: Option<Duration>` in `AgentTurnRequest`; cancellation is checked cooperatively at round/stream/tool boundaries. Timeout is a cooperative deadline around stream creation, stream consumption, and tool execution; live channel backpressure is an accepted alpha gap.

---

*Architecture analysis: 2026-05-19*
