# Codebase Structure

**Analysis Date:** 2026-05-14

## Directory Layout

```text
lattice/
├── Cargo.toml                 # Rust workspace manifest and shared dependency versions
├── Cargo.lock                 # Resolved Rust dependency lockfile
├── AGENTS.md                  # Codex-facing repo instructions
├── CLAUDE.md                  # Claude-facing repo instructions
├── README.md                  # Project overview and crate map
├── crates/                    # Workspace member crates
│   ├── lattice-policy/        # Approval, denial, risk, validation policy primitives
│   ├── lattice-hooks/         # Prompt/tool/session lifecycle hook types
│   ├── lattice-core/          # Model catalog, router, transport, streaming, common types
│   ├── lattice-session/       # Session graph, refs, CAS stores, turn manifests
│   ├── lattice-prompt/        # Prompt layers, providers, specs, compiler
│   ├── lattice-sandbox/       # Path/command/URL sandbox, Landlock, audit hooks
│   ├── lattice-tools/         # Tool definitions, capability bundles, default executor
│   ├── lattice-agent/         # Agent loop, state, memory, manifests, context manager
│   ├── lattice-plugin/        # Plugin trait/runtime, declarative loader, DAG runner
│   ├── lattice-orchestrator/  # Profiles, agent assembly, pipeline, event bus, micro-agents
│   ├── lattice-runtime/       # Top-level runtime builder, handles, turn execution
│   ├── lattice-plugins/       # Official plugin implementations
│   └── lattice-cli/           # Binary, commands, config, sessions, TUI frontend
├── examples/                  # Root examples plus agent/plugin manifest examples
├── benches/                   # Root benchmark entry
├── tests/                     # Python tests for documentation/link scripts
├── scripts/                   # Python wiki/mdBook helper scripts
├── wiki/                      # Source mdBook documentation
├── book.toml                  # mdBook configuration, source = wiki, output = book
├── book/                      # Generated mdBook output, not tracked by git
├── findings/                  # Local findings artifacts, not tracked by git
└── .planning/codebase/        # GSD codebase maps
```

## Directory Purposes

**Root**

- Purpose: workspace-level coordination and documentation.
- Contains: `Cargo.toml`, `Cargo.lock`, `README.md`, `AGENTS.md`, `CLAUDE.md`, `book.toml`, logos.
- Key files: `Cargo.toml`, `AGENTS.md`, `CLAUDE.md`, `README.md`, `book.toml`.
- Note: `RTK.md` is referenced by the user-provided instruction include list, but no `RTK.md` file exists at the repo root.

**`crates/lattice-policy`**

- Purpose: canonical policy vocabulary and dynamic approval authority.
- Contains: `approval.rs`, `authority.rs`, `decision.rs`, `denial.rs`, `proposal.rs`, `risk.rs`, `validation.rs`.
- Key files: `crates/lattice-policy/src/lib.rs`, `crates/lattice-policy/src/authority.rs`, `crates/lattice-policy/src/validation.rs`.

**`crates/lattice-hooks`**

- Purpose: hook abstractions around prompt/tool/session lifecycle points.
- Contains: `PromptHookChain`, hook context/outcome construction, and hook builder logic.
- Key file: `crates/lattice-hooks/src/lib.rs`.

**`crates/lattice-core`**

- Purpose: model-centric foundation crate.
- Contains: catalog loader/data/types, router, transport adapters, SSE streaming, provider/types/errors, token estimation, logging.
- Key files: `crates/lattice-core/src/lib.rs`, `crates/lattice-core/src/router.rs`, `crates/lattice-core/src/invocation.rs`, `crates/lattice-core/src/catalog/data.json`, `crates/lattice-core/src/transport/`, `crates/lattice-core/src/streaming/`.

**`crates/lattice-session`**

- Purpose: durable conversation/session graph.
- Contains: `Session`, `SessionStore`, `FileSessionStore`, `SqliteCasSessionStore`, branch/tag/reflog/stash/gc, artifact objects, and turn manifest objects.
- Key file: `crates/lattice-session/src/lib.rs`.
- Note: this crate is currently a large single source file. Add session features carefully around existing trait/store sections.

**`crates/lattice-prompt`**

- Purpose: prompt engine and provider abstraction.
- Contains: compiler, registry, provider/factory traits, `PromptSpec`, system stack, template rendering, tool schema provider, project instructions provider, and workspace provider.
- Key files: `crates/lattice-prompt/src/compiler.rs`, `crates/lattice-prompt/src/registry.rs`, `crates/lattice-prompt/src/types.rs`, `crates/lattice-prompt/src/spec.rs`, `crates/lattice-prompt/src/project_instructions_provider.rs`, `crates/lattice-prompt/src/workspace_provider.rs`.

**`crates/lattice-sandbox`**

- Purpose: tool execution safety checks and OS sandbox integration.
- Contains: sandbox config, read/write/command/URL checks, sensitive-file rules, command hook trait, audit sink, Landlock, and resource limits.
- Key files: `crates/lattice-sandbox/src/config.rs`, `crates/lattice-sandbox/src/checks.rs`, `crates/lattice-sandbox/src/landlock.rs`, `crates/lattice-sandbox/src/security.rs`.

**`crates/lattice-tools`**

- Purpose: built-in tool schema inventory, capability exposure policy, executor implementation, and structured patch engine.
- Contains: `capabilities.rs`, `tool_definitions.rs`, `executor.rs`, `tool_registry.rs`, `edit/`.
- Key files: `crates/lattice-tools/src/capabilities.rs`, `crates/lattice-tools/src/tool_definitions.rs`, `crates/lattice-tools/src/executor.rs`, `crates/lattice-tools/src/edit/apply.rs`.

**`crates/lattice-agent`**

- Purpose: agent execution loop and live state model.
- Contains: `Agent`, `TurnEngine`, `AgentState`, `TaskState`, context manager, tool result classification, final report, audit, memory, prompt adapters, and manifest emission.
- Key files: `crates/lattice-agent/src/lib.rs`, `crates/lattice-agent/src/turn_engine.rs`, `crates/lattice-agent/src/state.rs`, `crates/lattice-agent/src/context_manager.rs`, `crates/lattice-agent/src/tool_result.rs`.

**`crates/lattice-plugin`**

- Purpose: plugin runtime contracts and declarative plugin support.
- Contains: `Plugin`, `Behavior`, `PluginRunner`, erased runner, manifest loader, registry, DAG runner, and built-in Superpowers plugin bridge.
- Key files: `crates/lattice-plugin/src/lib.rs`, `crates/lattice-plugin/src/dag_runner.rs`, `crates/lattice-plugin/src/loader.rs`, `crates/lattice-plugin/src/manifest.rs`, `crates/lattice-plugin/src/builtin/superpowers.rs`.

**`crates/lattice-orchestrator`**

- Purpose: agent profile assembly and multi-agent orchestration.
- Contains: profiles, registry, `AgentFactory`, `AgentExecutor`, `Pipeline`, event bus, micro-agent bus, `.lattice` discovery, and optional WebSocket support.
- Key files: `crates/lattice-orchestrator/src/assembly.rs`, `crates/lattice-orchestrator/src/pipeline.rs`, `crates/lattice-orchestrator/src/agent_spec.rs`, `crates/lattice-orchestrator/src/micro_agent.rs`, `crates/lattice-orchestrator/src/events.rs`.

**`crates/lattice-runtime`**

- Purpose: top-level application API and wiring policy.
- Contains: `Runtime`, `RuntimeBuilder`, security builder, agent handle registry integration, turn streaming/persistence, presets, and runtime-backed orchestrator executor.
- Key files: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-runtime/src/handle.rs`, `crates/lattice-runtime/src/turn.rs`, `crates/lattice-runtime/src/turn_executor.rs`, `crates/lattice-runtime/src/preset.rs`.

**`crates/lattice-plugins`**

- Purpose: official plugin implementations behind Cargo features.
- Contains: `official/` modules for code review, refactor, test generation, security audit, doc generation, PPTX, research, image generation, knowledge base, planning, and verification.
- Key files: `crates/lattice-plugins/src/lib.rs`, `crates/lattice-plugins/src/official/mod.rs`, `crates/lattice-plugins/src/official/*.rs`.

**`crates/lattice-cli`**

- Purpose: user-facing binary, command dispatch, config/credentials, session helpers, and TUI.
- Contains: `main.rs`, command modules, coding agent adapter, runtime bootstrap, security/session wrappers, and TUI frontend modules.
- Key files: `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/coding_agent.rs`, `crates/lattice-cli/src/frontend/tui/`.

**`examples`**

- Purpose: manual examples and declarative manifest examples.
- Contains: root Rust examples plus `examples/agents/` and `examples/plugins/` TOML fixtures.
- Key files: `examples/agents/local-hello/agent.toml`, `examples/agents/superpowers/agent.toml`, `examples/plugins/hello/plugin.toml`, `examples/chat_test.rs`.

**`wiki` / `book`**

- Purpose: `wiki/` is mdBook source; `book/` is generated output.
- Contains: source documentation in `wiki/`; generated HTML/API docs in `book/`.
- Key files: `wiki/SUMMARY.md`, `book.toml`, `scripts/generate-summary.py`, `scripts/rewrite-links.py`.
- Generated output should not be edited by hand.

## Crate Ownership Map

| Crate | Owns | Add here when |
| --- | --- | --- |
| `lattice-policy` | Approval, risk, denial validation, and dynamic authority | Adding approval rule kinds, denial shapes, or risk classes |
| `lattice-core` | Model routing, transport, streaming, common LLM types | Adding provider protocol, catalog handling, or stream parsing |
| `lattice-session` | Conversation persistence and graph operations | Adding session refs, CAS object types, or turn manifest persistence |
| `lattice-prompt` | Prompt provider/spec/compiler semantics | Adding context providers, prompt layers, rendering, or budget behavior |
| `lattice-sandbox` | Execution safety checks | Adding path/command/URL policy application or OS sandbox controls |
| `lattice-tools` | Tool schema, exposure, and execution | Adding built-in tools, capability bundles, or patch/edit behavior |
| `lattice-agent` | Agent loop and live task state | Adding turn-loop behavior, task recovery state, or final report logic |
| `lattice-plugin` | Plugin contract and plugin DAG runtime | Adding shared plugin manifest/runtime features |
| `lattice-orchestrator` | Profile assembly and pipeline routing | Adding profile fields, handoff/fork behavior, or bus integration |
| `lattice-runtime` | Top-level wiring and stable runtime API | Adding app-facing runtime methods, handle lifecycle, or turn persistence |
| `lattice-plugins` | Official plugin implementations | Adding first-party plugin behavior behind feature flags |
| `lattice-cli` | Command-line and TUI user experience | Adding user commands, config flags, TUI widgets, or TUI state |

## Key File Locations

**Entry Points**

- `crates/lattice-cli/src/main.rs`: binary command parser and top-level dispatch.
- `crates/lattice-cli/src/frontend/tui/mod.rs`: TUI module entry.
- `crates/lattice-runtime/src/lib.rs`: programmatic `Runtime` entry.
- `crates/lattice-runtime/src/turn.rs`: runtime-owned turn submission stream.
- `crates/lattice-core/src/lib.rs`: core convenience API re-exports.

**Configuration**

- `Cargo.toml`: workspace members, Rust version, shared dependencies.
- `crates/*/Cargo.toml`: crate-local dependencies and feature flags.
- `crates/lattice-core/src/catalog/data.json`: embedded model/provider catalog.
- `crates/lattice-cli/src/config.rs`: CLI config file schema/defaults.
- `crates/lattice-cli/src/credentials.rs`: credential store/config bridge.
- `book.toml`: mdBook source/output configuration.

**Core Logic**

- `crates/lattice-runtime/src/lib.rs`: runtime composition and security builder.
- `crates/lattice-runtime/src/turn.rs`: handle-backed turn execution and session persistence.
- `crates/lattice-agent/src/turn_engine.rs`: agent run loop and tool execution.
- `crates/lattice-prompt/src/compiler.rs`: prompt compile algorithm.
- `crates/lattice-tools/src/executor.rs`: built-in tool execution.
- `crates/lattice-sandbox/src/checks.rs`: read/write/command/URL checks.
- `crates/lattice-orchestrator/src/pipeline.rs`: pipeline loop/handoff/fork.
- `crates/lattice-plugin/src/dag_runner.rs`: plugin slot DAG execution.

**Testing**

- `crates/lattice-agent/tests/`: agent integration-style tests.
- `crates/lattice-core/tests/`: core transport/e2e tests.
- `crates/lattice-tools/tests/exposure.rs`: tool exposure tests.
- `crates/lattice-cli/tests/`: CLI/runtime guard tests.
- Module-local tests live inside many `src/*.rs` files, especially `crates/lattice-session/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`, and `crates/lattice-tools/src/executor.rs`.
- `tests/test_rewrite_links.py`: Python tests for wiki link helper.

**Examples**

- `examples/agents/local-hello/agent.toml`: local plugin DAG agent profile example.
- `examples/plugins/hello/plugin.toml`: local hot-plug plugin manifest example.
- `crates/lattice-core/examples/*.rs`: core/provider debugging examples.
- Root `examples/*.rs`: manual examples mirrored from core examples.

## Naming Conventions

- Rust modules use snake_case, such as `turn_engine.rs`, `tool_registry.rs`, and `project_instructions_provider.rs`.
- Crates use kebab-case with a `lattice-` prefix, such as `crates/lattice-runtime` and `crates/lattice-orchestrator`.
- Official plugin modules use snake_case, such as `code_review.rs` and `security_audit.rs`.
- CLI command modules use snake_case under `crates/lattice-cli/src/commands/`, such as `config_cmd.rs`, `list_agents.rs`, and `orchestrator_status.rs`.
- Each Rust crate follows `crates/<crate-name>/src/`, with crate-local `tests/`, `examples/`, or `benches/` only when needed.
- TUI code is grouped under `crates/lattice-cli/src/frontend/tui/` and `crates/lattice-cli/src/frontend/tui/widgets/`.
- Generated docs output is `book/`; source docs are `wiki/`.

## Where To Add New Code

**New model/provider support**

- Catalog data/defaults: `crates/lattice-core/src/catalog/data.json`
- Protocol type: `crates/lattice-core/src/catalog/types.rs`
- Router behavior: `crates/lattice-core/src/router.rs`
- Transport implementation: `crates/lattice-core/src/transport/`
- Streaming parser: `crates/lattice-core/src/streaming/`
- Tests: `crates/lattice-core/tests/` or module tests near changed transport/parser.

**New runtime-facing feature**

- Public API and wiring: `crates/lattice-runtime/src/lib.rs`
- Turn lifecycle/state streaming: `crates/lattice-runtime/src/turn.rs`
- Handle metadata/lifecycle: `crates/lattice-runtime/src/handle.rs`
- CLI adapter: `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/commands/`, or `crates/lattice-cli/src/coding_agent.rs`.

**New built-in tool**

- Schema: `crates/lattice-tools/src/tool_definitions.rs`
- Capability exposure: `crates/lattice-tools/src/capabilities.rs`
- Execution: `crates/lattice-tools/src/executor.rs`
- Sandbox/policy changes, if needed: `crates/lattice-sandbox/src/checks.rs`, `crates/lattice-policy/src/`
- Tests: `crates/lattice-tools/tests/exposure.rs` and module tests in `crates/lattice-tools/src/executor.rs`.

**New prompt context provider**

- Provider implementation: `crates/lattice-prompt/src/<provider_name>.rs`
- Provider trait/factory/spec support: `crates/lattice-prompt/src/provider_factory.rs`, `crates/lattice-prompt/src/spec.rs`
- Assembly wiring: `crates/lattice-orchestrator/src/assembly.rs` or `crates/lattice-runtime/src/lib.rs`
- Agent-specific adapter, if it needs live task state or memory: `crates/lattice-agent/src/prompt/`.

**New agent loop behavior or recovery state**

- Live state model: `crates/lattice-agent/src/state.rs`
- Turn loop: `crates/lattice-agent/src/turn_engine.rs`
- Tool output classification: `crates/lattice-agent/src/tool_result.rs`
- Final report rendering: `crates/lattice-agent/src/final_report.rs`
- Runtime event mapping: `crates/lattice-runtime/src/turn.rs`

**New session persistence feature**

- Trait/model/store implementation: `crates/lattice-session/src/lib.rs`
- Runtime persistence bridge: `crates/lattice-runtime/src/turn.rs`
- CLI session command: `crates/lattice-cli/src/commands/sessions.rs`
- TUI session command/state: `crates/lattice-cli/src/frontend/tui/commands.rs`, `crates/lattice-cli/src/frontend/tui/session_state.rs`

**New orchestrator/pipeline feature**

- Profile schema: `crates/lattice-orchestrator/src/profile.rs`
- Lowering contract: `crates/lattice-orchestrator/src/agent_spec.rs`
- Assembly: `crates/lattice-orchestrator/src/assembly.rs`
- Pipeline execution: `crates/lattice-orchestrator/src/pipeline.rs`
- Runtime-backed executor bridge: `crates/lattice-runtime/src/turn_executor.rs`

**New plugin implementation**

- Shared plugin runtime changes: `crates/lattice-plugin/src/`
- Official plugin: `crates/lattice-plugins/src/official/<name>.rs`
- Official feature flag/export/registration: `crates/lattice-plugins/Cargo.toml`, `crates/lattice-plugins/src/lib.rs`, `crates/lattice-plugins/src/official/mod.rs`
- Declarative plugin example: `examples/plugins/<name>/plugin.toml`
- Agent profile example: `examples/agents/<name>/agent.toml`

**New CLI command**

- Command enum and dispatch: `crates/lattice-cli/src/main.rs`
- Command implementation: `crates/lattice-cli/src/commands/<name>.rs`
- Command module export: `crates/lattice-cli/src/commands/mod.rs`
- Runtime/session/security helpers: `crates/lattice-cli/src/runtime.rs`, `crates/lattice-cli/src/session.rs`, `crates/lattice-cli/src/security.rs`

**New TUI control/view**

- State and app logic: `crates/lattice-cli/src/frontend/tui/state.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`
- Event/action/update loop: `crates/lattice-cli/src/frontend/tui/event.rs`, `action.rs`, `update.rs`
- Rendering/widgets: `crates/lattice-cli/src/frontend/tui/ui.rs`, `crates/lattice-cli/src/frontend/tui/widgets/`
- Runtime integration: `crates/lattice-cli/src/frontend/tui/agent_runner.rs`, `turn.rs`

**Documentation update**

- Source docs: `wiki/`
- Summary generation: `scripts/generate-summary.py`
- Link checks/rewrite: `scripts/rewrite-links.py`
- Generated output: `book/`, but only through mdBook builds, not by hand.

## Module Boundary Rules

- Keep lower crates free of runtime imports. Use data structs and traits in lower crates, with implementations in `crates/lattice-runtime/src/`.
- Keep policy vocabulary in `crates/lattice-policy/src/`; sandbox/tools/agent should consume or re-export it.
- Keep application defaults in `lattice-runtime` or `lattice-cli`, not in `lattice-core` or `lattice-agent`.
- Keep model transport/provider logic in `lattice-core`; agent/runtime should call `lattice_core::chat*()` rather than constructing provider HTTP requests.
- Keep user-facing command rendering in `lattice-cli`; lower crates should return typed data or typed errors.
- Keep generated docs out of source-doc edits: edit `wiki/`, not `book/`.

## Special Directories

**`.planning/codebase`**

- Purpose: GSD codebase intelligence documents.
- Generated: yes, by mapping workflow.
- Note: `.planning/` is ignored by `.gitignore`, so these files require forced add when they are committed.

**`book`**

- Purpose: generated mdBook HTML output from `wiki/`.
- Generated: yes.
- Committed: no, based on tracked-file checks during mapping.

**`wiki`**

- Purpose: source documentation for mdBook.
- Generated: no, except `wiki/SUMMARY.md` may be regenerated by `scripts/generate-summary.py`.
- Committed: yes, based on tracked files under `wiki/`.

**`target`**

- Purpose: Cargo build artifacts.
- Generated: yes.
- Committed: no.

**`findings`**

- Purpose: local audit/review findings artifacts.
- Generated: yes/local.
- Committed: no, based on tracked-file checks during earlier cleanup.

**`.lattice` project directory**

- Purpose: runtime discovery of project-local orchestrator config and agents when present.
- Expected by: `crates/lattice-orchestrator/src/lattice_dir.rs`.
- Expected contents: `.lattice/orchestrator.toml` and `.lattice/agents/`.
- Status during this scan: not detected at repository root.

---

*Structure analysis: 2026-05-14*
