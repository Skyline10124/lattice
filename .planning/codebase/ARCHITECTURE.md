<!-- refreshed: 2026-05-14 -->
# Architecture

**Analysis Date:** 2026-05-14

## System Overview

```text
┌────────────────────────────────────────────────────────────────────────────┐
│                         Entry Layer: CLI / TUI / examples                  │
│ `crates/lattice-cli/src/main.rs` `crates/lattice-cli/src/frontend/tui/`      │
└───────────────────────────────┬────────────────────────────────────────────┘
                                │
                                ▼
┌────────────────────────────────────────────────────────────────────────────┐
│                         Composition Layer: lattice-runtime                 │
│ `Runtime` / `RuntimeBuilder` / `RuntimeAgentTurnExecutor`                   │
│ `crates/lattice-runtime/src/lib.rs` `crates/lattice-runtime/src/turn.rs`     │
└──────────────┬───────────────┬───────────────┬───────────────┬─────────────┘
               │               │               │               │
               ▼               ▼               ▼               ▼
┌───────────────────┐ ┌─────────────────┐ ┌─────────────────┐ ┌──────────────┐
│ Model / Transport │ │ Agent / Prompt   │ │ Tools / Sandbox │ │ Session CAS  │
│ `lattice-core`    │ │ `agent`,`prompt` │ │ `tools`,`policy`│ │ `session`    │
└─────────┬─────────┘ └────────┬────────┘ └────────┬────────┘ └──────┬───────┘
          │                    │                   │                 │
          ▼                    ▼                   ▼                 ▼
┌────────────────────────────────────────────────────────────────────────────┐
│                  Orchestration / Plugins: orchestrator + plugin(s)         │
│ `Pipeline` `AgentTurnExecutor` `PluginDagRunner` `PluginRegistry`           │
└────────────────────────────────────────────────────────────────────────────┘
```

## Component Responsibilities

| Component | Responsibility | Evidence |
| --- | --- | --- |
| `lattice-policy` | Canonical approval grants, denials, risk classes, and path/command/tool policy decisions. It does not depend on `lattice-tools`. | `crates/lattice-policy/src/lib.rs`, `crates/lattice-policy/src/authority.rs`, `crates/lattice-policy/src/validation.rs` |
| `lattice-hooks` | Prompt, tool, turn, and session lifecycle hook types and hook chain construction. | `crates/lattice-hooks/src/lib.rs`, `crates/lattice-agent/src/hook.rs` |
| `lattice-core` | Model catalog, alias resolution, provider credential fallback, transport dispatch, and normalized SSE events. | `crates/lattice-core/src/router.rs`, `crates/lattice-core/src/invocation.rs`, `crates/lattice-core/src/streaming/mod.rs` |
| `lattice-session` | Git-style conversation graph, branches, tags, reflog, commit/message/artifact CAS, and turn manifest persistence. | `crates/lattice-session/src/lib.rs` |
| `lattice-prompt` | Prompt contribution collection, layer ordering, budget allocation, context manifests, project instructions, and workspace providers. | `crates/lattice-prompt/src/registry.rs`, `crates/lattice-prompt/src/compiler.rs`, `crates/lattice-prompt/src/project_instructions_provider.rs` |
| `lattice-sandbox` | Pre-execution path/command/URL checks, sensitive-file blocking, Landlock, audit, and command hook interfaces. | `crates/lattice-sandbox/src/config.rs`, `crates/lattice-sandbox/src/checks.rs`, `crates/lattice-sandbox/src/landlock.rs` |
| `lattice-tools` | Built-in tool schemas, capability presets/bundles, default tool executor, and registry-backed custom tools. | `crates/lattice-tools/src/tool_definitions.rs`, `crates/lattice-tools/src/capabilities.rs`, `crates/lattice-tools/src/executor.rs` |
| `lattice-agent` | Multi-turn agent loop, prompt lifecycle, tool-call execution, task/recovery state, context compaction, and turn manifests. | `crates/lattice-agent/src/lib.rs`, `crates/lattice-agent/src/turn_engine.rs`, `crates/lattice-agent/src/state.rs` |
| `lattice-plugin` | Typed LLM function trait, behavior model, erased runner, declarative plugin loader, and intra-agent DAG runner. | `crates/lattice-plugin/src/lib.rs`, `crates/lattice-plugin/src/loader.rs`, `crates/lattice-plugin/src/dag_runner.rs` |
| `lattice-orchestrator` | Agent profiles, assembly, pipeline handoff/fork, event bus, micro-agent bus, and runtime turn trait. | `crates/lattice-orchestrator/src/assembly.rs`, `crates/lattice-orchestrator/src/pipeline.rs`, `crates/lattice-orchestrator/src/agent_spec.rs` |
| `lattice-runtime` | Top-level wiring policy for router, registries, memory, session store, security, agent handles, and turn streaming. | `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`, `crates/lattice-runtime/src/turn_executor.rs` |
| `lattice-plugins` | Official plugin implementations and feature-gated bundle registration. | `crates/lattice-plugins/src/lib.rs`, `crates/lattice-plugins/src/official/` |
| `lattice-cli` | Binary entry point, config and credentials, `run`/`code`/`sessions` commands, and the TUI frontend. | `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/frontend/tui/` |

## Pattern Overview

**Overall pattern:** layered Rust workspace with runtime-owned composition.

Key characteristics:

- Repository instructions declare the dependency flow as `hooks -> core -> session/prompt/sandbox/tools -> agent -> plugin -> orchestrator -> runtime -> cli`. The current code also includes `lattice-policy` as a shared policy foundation for sandboxing and agent/tool approval state.
- `lattice-runtime` is the preferred application entry point. It owns shared registries, model router, session store, memory, event bus, and the agent handle registry.
- `lattice-orchestrator` defines the `AgentTurnExecutor` trait without depending on `lattice-runtime`. `lattice-runtime` implements that trait through `RuntimeAgentTurnExecutor`, preserving dependency direction.
- Prompt assembly is a contribution/layer/budget model rather than direct string concatenation. Non-system context is exposed through `<lattice-context>` markers and `<lattice-context-manifest>`.
- Tool authorization is split into exposure and execution. Capabilities decide which tools an agent can see and call; sandbox/policy checks decide whether a specific path, command, or URL operation is allowed.
- Evidence: `AGENTS.md`, `Cargo.toml`, `crates/lattice-policy/src/lib.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-orchestrator/src/agent_spec.rs`, `crates/lattice-runtime/src/turn_executor.rs`, `crates/lattice-prompt/src/compiler.rs`, `crates/lattice-tools/src/capabilities.rs`, `crates/lattice-sandbox/src/checks.rs`.

## Layers

**Policy Layer**

- Purpose: define shared approval, denial, risk, and path-operation types.
- Location: `crates/lattice-policy/src/`
- Contains: `ApprovalGrant`, `ApprovalKind`, `PolicyAuthority`, `DenialReport`, `RiskClass`, `ValidationError`.
- Depends on: foundational external crates only; no LATTICE business crate dependencies.
- Used by: `lattice-sandbox`, `lattice-agent`, `lattice-runtime`, and `lattice-cli`.

**Core Model Layer**

- Purpose: resolve model aliases into `ResolvedModel`, choose provider/protocol/credentials, and normalize provider streaming.
- Location: `crates/lattice-core/src/`
- Contains: `ModelRouter`, `ResolvedModel`, `ApiProtocol`, `TransportDispatcher`, `StreamEvent`, `chat_with_effort()`.
- Depends on: HTTP, serde, futures, tracing, and related foundational dependencies.
- Used by: agent, runtime, plugin, orchestrator, and CLI crates.

**Prompt Layer**

- Purpose: compile system stack, project instructions, workspace context, tool schema, memory/events, and user input into model messages.
- Location: `crates/lattice-prompt/src/`
- Contains: `PromptRegistry`, `PromptContribution`, `PromptSpec`, `ProjectInstructionsProvider`, `WorkspaceContextProvider`, `ToolSchemaProvider`.
- Depends on: `lattice-core` model/message/token primitives.
- Used by: `lattice-agent`, `lattice-orchestrator`, and `lattice-runtime`.

**Sandbox / Tools Layer**

- Purpose: expose callable tools, execute tools, constrain file/command/URL behavior, and emit structured denials.
- Location: `crates/lattice-tools/src/`, `crates/lattice-sandbox/src/`
- Contains: `DefaultToolExecutor`, `ToolExecutor`, `ToolOutcome`, `ToolRegistry`, `SandboxConfig`, `LandlockConfig`.
- Depends on: `lattice-core`, `lattice-sandbox`, `lattice-policy`.
- Used by: agent turn execution, orchestrator assembly, and runtime security.

**Agent Layer**

- Purpose: preserve conversation/task state, run multi-turn loops, process prompt hooks, stream LLM output, execute tool calls, emit manifests, and compact context.
- Location: `crates/lattice-agent/src/`
- Contains: `Agent`, `TurnEngine`, `AgentState`, `TaskState`, `ContextManager`, `LoopEvent`, `FinalReport`.
- Depends on: `lattice-core`, `lattice-hooks`, `lattice-policy`, `lattice-prompt`, `lattice-sandbox`, `lattice-tools`.
- Used by: plugin, orchestrator, runtime, and CLI layers.

**Plugin Layer**

- Purpose: package typed LLM functions, behavior, tools, and DAG slots into reusable plugin execution units.
- Location: `crates/lattice-plugin/src/`, `crates/lattice-plugins/src/`
- Contains: `Plugin`, `Behavior`, `PluginRunner`, `PluginRegistry`, `PluginDagRunner`, official plugins.
- Depends on: agent, core, prompt, sandbox, and tools crates.
- Used by: orchestrator plugin DAG paths, runtime plugin registry, and CLI plugin loading.

**Orchestration Layer**

- Purpose: assemble agents from profiles, run pipeline handoff/fork behavior, run the micro-agent bus, and expose a runtime-backed turn trait.
- Location: `crates/lattice-orchestrator/src/`
- Contains: `AgentProfile`, `AgentFactory`, `assemble_agent_with_options()`, `Pipeline`, `AgentExecutor`, `EventBus`, `MicroAgent`, `AgentTurnExecutor`.
- Depends on: core, agent, hooks, prompt, tools, and plugin crates.
- Used by: runtime and CLI.

**Runtime Layer**

- Purpose: centralize application-level wiring policy, hide lower-crate assembly details, and expose handle, turn, session, and security APIs.
- Location: `crates/lattice-runtime/src/`
- Contains: `Runtime`, `RuntimeBuilder`, `RuntimeSecurity`, `RuntimeHandle`, `RuntimeTurnEvent`, `TurnCompletion`, `RuntimeAgentTurnExecutor`.
- Depends on: core, session, prompt, agent, plugin, sandbox, tools, and orchestrator crates.
- Used by: `lattice-cli` and downstream embedding applications.

**CLI / TUI Layer**

- Purpose: translate config, credentials, sessions, security options, and user commands into runtime API calls.
- Location: `crates/lattice-cli/src/`
- Contains: `main.rs`, `commands/run.rs`, `coding_agent.rs`, `runtime.rs`, `frontend/tui/`.
- Depends on: upper workspace crates, especially `lattice-runtime`.
- Used by: the `lattice` binary.

## Data Flow

### CLI `run` / `code` Request Path

1. The CLI parses subcommands, loads `.env`, config, credentials, and runtime security. Evidence: `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/security.rs`.
2. `run` creates a `Runtime` with a `SqliteCasSessionStore` through `crate::runtime::model_runtime()`. `code` creates a coding preset turn spec through `build_coding_agent()`. Evidence: `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/coding_agent.rs`.
3. `Runtime::get_or_create_agent_handle_for_session_seeded()` reuses or creates an internal `Agent` based on workspace, model, provider, profile, security, and session identity. Evidence: `crates/lattice-runtime/src/lib.rs`.
4. `Runtime::build_internal_agent()` calls `lattice_orchestrator::assembly::assemble_agent_with_options()` and injects project instructions, memory, tool schema provider, sandboxed tool executor, coding workspace provider, and task-state provider. Evidence: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-orchestrator/src/assembly.rs`.
5. `Runtime::submit_turn()` takes the agent from the handle registry, spawns an async turn task, and emits a `RuntimeTurnEvent` stream. Evidence: `crates/lattice-runtime/src/turn.rs`.
6. `Agent::run_streaming()` delegates to `TurnEngine`, which collects prompt contributions, runs prompt hooks, calls `lattice_core::chat_with_effort()`, handles `StreamEvent`, and executes tool calls. Evidence: `crates/lattice-agent/src/turn_engine.rs`, `crates/lattice-agent/src/chat.rs`.
7. Tool calls are executed by `DefaultToolExecutor::execute_outcome()`. Unauthorized tools and path/command/URL denials return typed `ToolOutcome::Denied`, which the agent converts into approval proposals and task state. Evidence: `crates/lattice-tools/src/executor.rs`, `crates/lattice-agent/src/tool_result.rs`.
8. On turn completion, runtime saves `TurnCompletion`. If `persist_session` is true, it calls `SessionStore::save_with_manifests()`. Stores without manifest support fall back to `save()`. Evidence: `crates/lattice-runtime/src/turn.rs`, `crates/lattice-session/src/lib.rs`.

### Model Resolution And Streaming

1. `Runtime::resolve_with_provider()` calls `ModelRouter::resolve()`. Evidence: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-core/src/router.rs`.
2. `ModelRouter` normalizes model ids, resolves aliases, chooses catalog or custom model providers, checks external credentials or environment credentials, and validates base URLs. Evidence: `crates/lattice-core/src/router.rs`, `crates/lattice-core/src/catalog/data.json`.
3. `chat_with_effort()` builds a `ChatRequest` and selects OpenAI, Anthropic, or Gemini transport from `TransportDispatcher` based on `ApiProtocol`. Evidence: `crates/lattice-core/src/invocation.rs`, `crates/lattice-core/src/transport/dispatcher.rs`.
4. Provider-specific `SseParser` implementations normalize streaming bodies into `StreamEvent::{Token, Reasoning, ToolCall*, Done, Error}`. Evidence: `crates/lattice-core/src/streaming/mod.rs`, `crates/lattice-core/src/streaming/anthropic.rs`, `crates/lattice-core/src/streaming/openai.rs`, `crates/lattice-core/src/streaming/gemini.rs`.

### Prompt Assembly Flow

1. `PromptRegistry::collect_contributions()` always creates the system stack contribution first, then adds static `PromptSpec` contributions and dynamic providers. Evidence: `crates/lattice-prompt/src/registry.rs`.
2. `ProjectInstructionsProvider` searches from root to cwd for `CLAUDE.md`, `AGENTS.md`, and `LATTICE.md`. Within the same directory, rendering order is `CLAUDE.md -> AGENTS.md -> LATTICE.md`; local directories override parents. Evidence: `crates/lattice-prompt/src/project_instructions_provider.rs`.
3. `WorkspaceContextProvider` emits top-level files, git state, visible instruction files, and project-type hints as `Layer::Memory`. Evidence: `crates/lattice-prompt/src/workspace_provider.rs`.
4. `compile_contributions()` adds user input, sorts by layer and priority, allocates budget in `Fixed -> Ratio -> Dynamic` order, drops sections over budget, and records manifests for withheld memory/event fragments. Evidence: `crates/lattice-prompt/src/compiler.rs`.

### Pipeline / Plugin Flow

1. `Runtime::pipeline()` creates a `Pipeline` with `AgentRegistry`, memory, `EventBus`, `PluginRegistry`, `ToolRegistry`, shared `ModelRouter`, and `RuntimeAgentTurnExecutor`. Evidence: `crates/lattice-runtime/src/lib.rs`.
2. `Pipeline::run()` starts from the configured start agent, follows handoff rules and fallback behavior, and runs fork branches with `tokio::spawn` and branch timeouts. Evidence: `crates/lattice-orchestrator/src/pipeline.rs`.
3. Standard profiles run through `AgentExecutor::run_standard()`. With a runtime-backed executor, the profile is first lowered through `lower_profile_to_turn_spec()`, then executed by runtime turn APIs. Evidence: `crates/lattice-orchestrator/src/agent_executor.rs`, `crates/lattice-orchestrator/src/agent_spec.rs`, `crates/lattice-runtime/src/turn_executor.rs`.
4. Profiles with `[plugins]` use `PluginDagRunner`, executing plugin slots and edges. Forking is not supported inside an intra-agent DAG and should use pipeline fork instead. Evidence: `crates/lattice-orchestrator/src/pipeline.rs`, `crates/lattice-plugin/src/dag_runner.rs`.
5. Official plugins are registered into `PluginRegistry` by `lattice-plugins` feature gates. Evidence: `crates/lattice-plugins/src/lib.rs`, `crates/lattice-plugins/Cargo.toml`.

## Key Abstractions

**`Runtime` / `RuntimeBuilder`**

- Purpose: construct router, registries, session store, memory, event bus, and agent handles at the application boundary.
- Examples: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/runtime.rs`.
- Pattern: builder injection with shared `Arc` registries.

**`AgentTurnExecutor`**

- Purpose: let orchestrator code request runtime-owned turn execution without depending on `lattice-runtime`.
- Examples: `crates/lattice-orchestrator/src/agent_spec.rs`, `crates/lattice-runtime/src/turn_executor.rs`.
- Pattern: lower crate defines the trait; upper crate implements it.

**`Agent` / `TurnEngine`**

- Purpose: agent state machine for prompt lifecycle, chat streaming, tool execution, and manifest emission.
- Examples: `crates/lattice-agent/src/lib.rs`, `crates/lattice-agent/src/turn_engine.rs`.
- Pattern: `Agent` owns mutable state; `TurnEngine` binds to `&mut Agent` for each run.

**`PromptContribution` / `PromptRegistry`**

- Purpose: carry authority, source, cache, and budget metadata for all context fragments entering prompt compilation.
- Examples: `crates/lattice-prompt/src/types.rs`, `crates/lattice-prompt/src/registry.rs`, `crates/lattice-prompt/src/compiler.rs`.
- Pattern: typed metadata, layer ordering, and budget allocation.

**`ToolExecutor` / `DefaultToolExecutor`**

- Purpose: abstract tool invocation; the default implementation handles built-in filesystem, shell, web, patch, todo, and registry-backed custom tools.
- Examples: `crates/lattice-tools/src/lib.rs`, `crates/lattice-tools/src/executor.rs`, `crates/lattice-tools/src/tool_registry.rs`.
- Pattern: capability exposure plus execution-time sandbox gates.

**`PolicyAuthority`**

- Purpose: share dynamic approval grants across runtime, sandbox, and tool executor paths.
- Examples: `crates/lattice-policy/src/authority.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-sandbox/src/checks.rs`.
- Pattern: cloneable authority handle backed by `Arc<Mutex<Vec<ApprovalGrant>>>`.

**`SessionStore`**

- Purpose: unify session persistence across file and SQLite CAS stores.
- Examples: `crates/lattice-session/src/lib.rs`, `crates/lattice-cli/src/session.rs`.
- Pattern: trait abstraction with a default-method fallback for turn manifests.

**`Plugin` / `PluginDagRunner`**

- Purpose: typed LLM function execution and slot DAG orchestration.
- Examples: `crates/lattice-plugin/src/lib.rs`, `crates/lattice-plugin/src/dag_runner.rs`, `examples/plugins/hello/plugin.toml`.
- Pattern: typed plugin, erased runner, declarative manifest.

## Entry Points

**CLI binary**

- Location: `crates/lattice-cli/src/main.rs`
- Triggers: `cargo run -p lattice-cli -- ...` or installed `lattice` binary.
- Responsibilities: parse subcommands, load `.env`, config, credentials, and route into TUI/run/code/pipeline/session paths.

**TUI frontend**

- Location: `crates/lattice-cli/src/frontend/tui/`
- Triggers: `lattice tui` or the default no-subcommand path.
- Responsibilities: interactive transcript, composer, runtime handle reuse, plugin mode, and session commands.

**Programmatic runtime**

- Location: `crates/lattice-runtime/src/lib.rs`
- Triggers: downstream calls to `Runtime::builder().build()`.
- Responsibilities: model resolution, chat, agent handles, pipelines, security, and session compaction.

**Agent profile loading**

- Location: `crates/lattice-orchestrator/src/lattice_dir.rs`, `crates/lattice-orchestrator/src/registry.rs`
- Triggers: `.lattice/agents/` or CLI `--agents-dir`.
- Responsibilities: load `agent.toml` and merge optional project/global registries.

**Declarative plugin loading**

- Location: `crates/lattice-plugin/src/loader.rs`, `crates/lattice-plugin/src/manifest.rs`
- Triggers: runtime builder `load_plugin_dir()` or CLI `--plugins-dir`.
- Responsibilities: find `plugin.toml`, parse manifests, and reject duplicate plugin names.

## Architectural Constraints

- **Dependency direction:** Keep `lattice-runtime` above orchestrator, agent, plugin, tools, session, and core crates. Lower crates should not import `lattice-runtime`; use traits such as `AgentTurnExecutor`.
- **Policy ownership:** New approval, denial, and risk types belong in `crates/lattice-policy/src/`; `lattice-sandbox` should apply or re-export them rather than duplicating policy structs.
- **Tool exposure vs execution:** Add built-in tool schemas in `crates/lattice-tools/src/tool_definitions.rs`, expose them through bundles/presets in `crates/lattice-tools/src/capabilities.rs`, and enforce authorization/sandbox rules in `crates/lattice-tools/src/executor.rs`.
- **Prompt hierarchy:** Project instructions should flow through `ProjectInstructionsProvider`; do not manually concatenate `AGENTS.md` or `CLAUDE.md` inside CLI/runtime code.
- **Threading:** Runtime turn execution uses Tokio tasks and `tokio::sync::mpsc::unbounded_channel` for `RuntimeTurnEvent` streaming. Evidence: `crates/lattice-runtime/src/turn.rs`.
- **Global state:** `lattice-core` uses lazy/static shared state for dispatching, provider credential maps, and embedded catalog data. Evidence: `crates/lattice-core/src/invocation.rs`, `crates/lattice-core/src/router.rs`, `crates/lattice-core/src/catalog/data.json`.
- **Shared mutable state:** Runtime agent handles, task state, and dynamic approval grants use shared mutex-backed state. Evidence: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-agent/src/state.rs`, `crates/lattice-policy/src/authority.rs`.
- **Session persistence:** CLI default session management uses `SqliteCasSessionStore`; runtime code should depend on the `SessionStore` trait rather than concrete store types. Evidence: `crates/lattice-cli/src/session.rs`, `crates/lattice-runtime/src/lib.rs`.
- **Workspace instruction references:** Root `AGENTS.md` and `CLAUDE.md` are present. The user-provided include list references `RTK.md`, but `RTK.md` is not present at the repository root.

## Anti-Patterns

### Runtime Back-Dependency From Lower Crates

**What happens:** A lower crate imports `lattice-runtime` to execute a turn or build an agent.

**Why it is wrong:** It breaks the current composition direction where `lattice-runtime` owns wiring above lower crates. `lattice-orchestrator` already provides `AgentTurnExecutor` to avoid this dependency.

**Do this instead:** Define pure data and trait contracts in lower crates, then implement them in runtime.

### Duplicating Policy Types

**What happens:** A sandbox or tool feature defines its own denial, approval, or risk structs.

**Why it is wrong:** Policy vocabulary is centralized in `lattice-policy`. Duplicate types make runtime approval and tool denial enrichment drift.

**Do this instead:** Extend `crates/lattice-policy/src/` and apply or re-export the types through `lattice-sandbox`.

### Bypassing Prompt Contributions

**What happens:** Code appends raw project context directly to user messages outside `PromptRegistry`.

**Why it is wrong:** The context loses authority, source, cache, and budget metadata and bypasses context manifest behavior.

**Do this instead:** Add a `ContextProvider` or `PromptSpec` provider in `lattice-prompt`, then wire it through orchestrator assembly or runtime.

### Tool Without Capability And Sandbox Wiring

**What happens:** A tool handler is added directly to `DefaultToolExecutor` but not exposed in capabilities or authorized names.

**Why it is wrong:** The model may never see the tool, or execution may be denied as not enabled.

**Do this instead:** Update schema, capability bundle, executor match arm, and tests together.

## Error Handling

**Strategy:** Domain-specific typed errors at crate boundaries, string rendering at CLI/user-facing surfaces, and structured denial JSON for sandbox/tool denials.

Patterns:

- Core crates use `thiserror` domain enums, such as `RuntimeError`, `SessionError`, and `DAGError`.
- CLI commands wrap operations in `anyhow::Result` and convert lower-level errors into readable command failures.
- Streaming/provider errors become `StreamEvent::Error` or `LatticeError` classifications.
- Sandbox denials are typed `DenialReport` values serialized to JSON with sentinel fields, allowing agent/tool-result logic to distinguish denials from ordinary output.
- `Runtime::submit_turn()` handles panics and cancellation and stores a terminal `TurnCompletion`.
- Evidence: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-session/src/lib.rs`, `crates/lattice-plugin/src/dag_runner.rs`, `crates/lattice-cli/src/main.rs`, `crates/lattice-core/src/errors.rs`, `crates/lattice-sandbox/src/checks.rs`, `crates/lattice-agent/src/tool_result.rs`.

## Cross-Cutting Concerns

**Logging:** `lattice-core` exposes `init_logging()` and `init_debug_logging()`. CLI selects normal, verbose, or debug logging. Pipeline and security paths use `tracing`. Evidence: `crates/lattice-core/src/logging.rs`, `crates/lattice-cli/src/main.rs`, `crates/lattice-orchestrator/src/pipeline.rs`, `crates/lattice-sandbox/src/landlock.rs`.

**Validation:** Model endpoints are validated in `lattice-core`; approval rules are validated in `lattice-policy`; profile and plugin TOML parsing is validated through orchestrator and plugin loader modules.

**Authentication:** Credentials are loaded by CLI config/credential store and passed as external credentials to `Runtime::builder().credentials()`. `ModelRouter` also supports environment-variable fallback per provider. Evidence: `crates/lattice-cli/src/credentials.rs`, `crates/lattice-cli/src/runtime.rs`, `crates/lattice-core/src/router.rs`.

**Persistence:** Session persistence is abstracted through `SessionStore`. SQLite CAS is the CLI default, while `FileSessionStore` remains available and appears in tests. Evidence: `crates/lattice-cli/src/session.rs`, `crates/lattice-session/src/lib.rs`.

**Security:** Runtime security config builds `SandboxConfig` with read/write/command allowlists, optional hook chain, optional Landlock, audit sink, size/time limits, and shared approval rules. Evidence: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/security.rs`, `crates/lattice-sandbox/src/config.rs`.

---

*Architecture analysis: 2026-05-14*
