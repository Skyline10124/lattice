# Crate Map

This page reflects the current Cargo workspace. Removed or historical crates such as `lattice-bus`, `lattice-python`, and `lattice-registry` are not workspace members.

| Crate | Responsibility | Depends On |
| --- | --- | --- |
| `lattice-core` | Model catalog, routing, transports, streaming, retry, token estimation, shared chat/tool types | external crates only |
| `lattice-hooks` | Runtime lifecycle hook primitives and prompt hook dispatch | `lattice-core`, `lattice-prompt` |
| `lattice-prompt` | Prompt sections, layers, provider registry, system prompt stack, tool schema provider | `lattice-core` |
| `lattice-session` | Session graph, commit objects, refs, turn manifests, artifacts, file and SQLite CAS stores | external crates only |
| `lattice-sandbox` | Command/path/URL safety policy, audit sink traits, optional Landlock configuration | external crates only |
| `lattice-tools` | Built-in tool schemas, capability presets/bundles, default executor, registry-backed tools | `lattice-core`, `lattice-sandbox` |
| `lattice-agent` | Agent state, model loop, prompt integration, memory adapters, hooks, audit | `lattice-core`, `lattice-hooks`, `lattice-prompt`, `lattice-sandbox`, `lattice-tools` |
| `lattice-plugin` | Plugin trait, erased plugin adapter, registry, manifest loader, DAG runner, behavior policies | `lattice-core`, `lattice-prompt`, `lattice-agent`, `lattice-sandbox`, `lattice-tools` |
| `lattice-orchestrator` | TOML profiles, agent assembly, pipelines, event bus, micro-agent RPC/pub-sub, watcher, optional WebSocket bridge | `lattice-core`, `lattice-agent`, `lattice-hooks`, `lattice-prompt`, `lattice-tools`, `lattice-plugin` |
| `lattice-runtime` | Top-level runtime facade that owns router, registries, security config, sessions, pipelines, and agent construction | `lattice-core`, `lattice-session`, `lattice-prompt`, `lattice-agent`, `lattice-plugin`, `lattice-sandbox`, `lattice-tools`, `lattice-orchestrator` |
| `lattice-plugins` | Official plugin bundle implementations | `lattice-core`, `lattice-plugin` |
| `lattice-cli` | CLI, TUI, credentials, config, command dispatch, session commands | all runtime-facing crates |

## Dependency Direction

```text
lattice-core
  ├─ lattice-prompt
  ├─ lattice-hooks
  ├─ lattice-sandbox
  └─ lattice-tools
       └─ lattice-agent
            ├─ lattice-plugin
            └─ lattice-orchestrator
                 └─ lattice-runtime
                      └─ lattice-cli

lattice-session ───────────┘
lattice-plugins ── depends on lattice-plugin
```

The diagram is approximate: it shows the intended layering, not every edge. `lattice-runtime` is the preferred application-facing seam because it keeps model routing, tool capability policy, registry wiring, security config, and session storage in one place.

## Important Modules

### `lattice-core`

- `catalog/`: embedded model catalog, aliases, provider defaults and API protocols.
- `router.rs`: model normalization, alias resolution, provider selection and credential resolution.
- `transport/`: OpenAI-compatible, Anthropic and Gemini transport adapters.
- `streaming.rs`: SSE parsing and normalized streaming events.
- `retry.rs`: jittered exponential backoff and error classification.
- `errors.rs`: `LatticeError` and provider error mapping.
- `types.rs`: `Message`, `Role`, `ToolDefinition`, `ToolCall`, `FunctionCall`.
- `security.rs`: base URL and private/reserved IP validation.

### `lattice-prompt`

- `compiler.rs`: collect, sort, budget, trim and render prompt sections.
- `registry.rs`: runtime provider registry.
- `system_stack.rs`: kernel, agent and dynamic system prompt deltas.
- `tool_schema_provider.rs`: exposes tool definitions as prompt context.
- `project_instructions_provider.rs`: loads project instruction files from root to cwd.

### `lattice-sandbox`

- `checks.rs`: read/write/command/URL checks.
- `config.rs`: `SandboxConfig` and `LandlockConfig`.
- `command_utils.rs`: command allowlist matching.
- `path_utils.rs`: sensitive-file and prefix checks.
- `security.rs`: private/reserved address detection.
- `landlock.rs`: Linux Landlock adapter.

### `lattice-tools`

- `tool_definitions.rs`: built-in tool JSON schemas.
- `capabilities.rs`: `ToolPreset`, `ToolBundle`, `CapabilitySelection`.
- `executor.rs`: `DefaultToolExecutor`, bash execution, file tools, `web_search`.
- `tool_registry.rs`: custom/native registry-backed tools.
- `tool_error.rs`: structured tool error formatting.

### `lattice-agent`

- `lib.rs`: `Agent`, `ToolExecutor`, `PluginAgent`, loop events.
- `state.rs`: conversation state and context trimming.
- `memory/`: in-memory and SQLite memory adapters.
- `hook.rs`: hook chain integration for tool validation.
- `audit.rs`: append-only audit log.
- `events_provider.rs`: bus/event context provider.

### `lattice-plugin`

- `lib.rs`: `Plugin`, `Behavior`, `PluginRunner`.
- `erased.rs` and `erased_runner.rs`: type-erased plugin execution.
- `bundle.rs`: `PluginBundle`, metadata and default behavior/tool config.
- `registry.rs`: plugin registry.
- `manifest.rs` and `loader.rs`: declarative local plugin loading.
- `dag_runner.rs`: intra-plugin DAG execution.
- `orchestration.rs`: DAG edge config and handoff rule evaluation.

### `lattice-orchestrator`

- `profile.rs`: TOML-backed `AgentProfile`.
- `assembly.rs`: profile-to-agent construction.
- `runner.rs`: profile-backed agent execution and JSON validation.
- `pipeline.rs`: handoff/fork pipeline control flow.
- `lib.rs`: orchestrator trait, in-memory implementation, RPC/pub-sub types.
- `micro_agent.rs`: micro-agent runtime loop.
- `events.rs`: broadcast `EventBus` and `PipelineEvent`.
- `watcher.rs`: agent profile hot reload.
- `ws.rs`: optional WebSocket event bridge.

### `lattice-runtime`

- `Runtime`: shared model router, registries, memory, event bus and session store.
- `RuntimeBuilder`: application-facing construction API.
- `AgentSpec`: CLI-facing agent builder with capability presets/bundles.
- `RuntimeSecurity`: sandbox and audit configuration wrapper.
- `messages_for_session()` and `compact_session()`: session-to-agent helpers.

### `lattice-session`

- `Session`, `SessionMessage`, `SessionToolCall`.
- `SessionStore` trait.
- `FileSessionStore` and `SqliteCasSessionStore`.
- Commit/ref/tag/stash/diff/GC models.
- Turn manifest and artifact records.
