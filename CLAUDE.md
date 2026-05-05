# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

**LATTICE** is a model-centric LLM engine split into six Rust crates. The user specifies a model name or alias (e.g., `"sonnet"`), and the engine resolves which provider serves it, picks the best credential, formats the request for the correct API protocol, and handles streaming, tool calls, and retries. There is no `set_provider()` call — provider selection is automatic.

**Crate architecture** (one-way deps):

```
lattice-cli           CLI + TUI (ratatui)
  → lattice-bus           Pipeline, AgentRunner, AgentProfile, event bus, registry, watcher
    → lattice-plugin       ErasedPlugin, PluginRegistry, PluginDagRunner, handoff rules, 11 builtin plugins
    → lattice-agent       Async Agent::run(), tool execution (tokio), SqliteMemory, ToolRegistry, conversation state
      → lattice-core       resolve() + chat() — model routing + streaming inference
  → lattice-python       PyO3 bindings (pip package: lattice-core)
```

Note: There is no separate `lattice-tui` or `lattice-harness` crate. The TUI lives in `lattice-cli/src/frontend/tui/`. Pipeline/AgentRunner/profile code lives in `lattice-bus`; PluginDagRunner and the canonical handoff rule language live in `lattice-plugin`.

- **Rust workspace**: Cargo workspace at repo root, all crates under `lattice-*/`
- **Python package**: `lattice-core` (from `lattice-python/`) via [maturin](https://github.com/PyO3/maturin)

## Build, test, lint

All commands run from the **repo root** (Cargo workspace).

```bash
# Build everything
cargo build

# Release build
cargo build --release

# Build and install Python bindings into the active venv
cd lattice-python && maturin develop

# Run all Rust unit tests (no Python runtime required)
cargo test

# Run a single crate's tests
cargo test -p lattice-core

# Run a single test
cargo test -p lattice-core <test_name>

# Run benchmarks
cargo bench -p lattice-core

# Lint (treat warnings as errors)
cargo clippy -- -D warnings

# Format check
cargo fmt --check --all

# Format code
cargo fmt --all
```

**CI** (`.github/workflows/ci.yml`): runs `cargo test`, `cargo clippy`, `cargo fmt --check --all`, and Python smoke tests (3.12, 3.13, 3.14) via `maturin develop` + import check.

## Architecture

```
User Code (Python / Rust / CLI)
  → lattice_core::resolve("sonnet")              → ResolvedModel
  → lattice_core::chat(resolved, msgs)            → impl Stream<Item = StreamEvent>
  → lattice_agent::Agent::new(resolved)           → agent.run("prompt", 10).await (async, with tool loop)
  → lattice_bus::Pipeline::new(...)               → pipeline.run("start", "input").await (async, plugin DAG or agent runner)
```

### Crate map

| Crate | Purpose |
|-------|---------|
| `lattice-core` | Model resolution, streaming inference, retry, token estimation. **No PyO3.** |
| `lattice-agent` | `Agent` struct: async multi-turn conversation, tool execution (tokio), token budget, provider fallback, `Memory` trait + `InMemoryMemory` + `SqliteMemory` (FTS5), `ToolRegistry`, `DefaultToolExecutor` |
| `lattice-plugin` | `Plugin` trait, `ErasedPlugin`, `PluginRegistry`, `PluginDagRunner`, canonical handoff rule language for pipelines and DAG edges, 11 builtin plugins (CodeReview, Refactor, TestGen, SecurityAudit, DocGen, PptxGen, DeepResearch, ImageGen, KnowledgeBase, PlanGen, Verification), `Behavior` (Strict/Yolo) |
| `lattice-cli` | CLI binary: `run`, `print`, `resolve`, `models`, `doctor`, `bus`, `config`, `sessions`, `stats`, `debug`, `validate`, `list`, `new`, `tui`. TUI in `src/frontend/tui/` (ratatui): streaming chat, slash commands, session persistence |
| `lattice-python` | PyO3 bindings: `LatticeEngine`, exceptions, `StreamIterator` (pip: `lattice-core`) |
| `lattice-bus` | TOML-based agent profiles, `Pipeline`, `AgentRunner`, JSON schema validation, fork execution, dry-run validation, hot reload, pub/sub event bus, microagent communication, inter-agent messaging |

### lattice-core module map

| Module | Purpose |
|--------|---------|
| `catalog` | Model catalog, aliases, provider defaults, `ApiProtocol`, `ResolvedModel` |
| `router` | `ModelRouter`: normalize model IDs, resolve aliases, select provider, resolve credentials |
| `provider` | `Provider` trait, `ChatRequest`/`ChatResponse`, shared HTTP client |
| `transport/` | Unified Transport trait, protocol adapters (ChatCompletions, Anthropic, Gemini, OpenAICompat), dispatcher |
| `streaming` | SSE parsers (OpenAI, Anthropic) via `sse_from_bytes_stream`, `StreamEvent`. Note: Gemini uses non-streaming path. |
| `retry` | `ErrorClassifier`, `RetryPolicy` with jittered exponential backoff |
| `tokens` | `TokenEstimator`: tiktoken for OpenAI models, char/4 estimation for others |
| `errors` | `LatticeError` enum (pure Rust, no PyO3), `ErrorClassifier` |
| `types` | `Role`, `Message`, `ToolDefinition`, `ToolCall`, `FunctionCall` |

### lattice-agent module map

| Module | Purpose |
|--------|---------|
| `lib.rs` / `state.rs` | `Agent` struct: async multi-turn conversation loop, conversation state |
| `tools.rs` / `tool_definitions.rs` / `tool_registry.rs` / `tool_error.rs` | `DefaultToolExecutor`, tool definitions, `ToolRegistry`, tool error types |
| `memory/` | `Memory` trait, `InMemoryMemory`, `SharedMemory` trait, `SqliteMemory` (FTS5) |
| `prompt/` | Prompt compilation, provider-specific formatting, template registry |
| `sandbox.rs` | Sandboxed execution environment for tool calls |
| `blob.rs` | Blob store for large tool outputs |
| `events_provider.rs` / `bus_event_collector.rs` | Event provider trait and bus-based event collection |

### lattice-plugin module map

| Module | Purpose |
|--------|---------|
| `lib.rs` | `Plugin` trait, `Behavior` trait (Strict/Yolo), `PluginRunner`, `PluginHooks`, `PluginConfig` |
| `erased.rs` / `erased_runner.rs` | `ErasedPlugin` trait + blanket impl, `ErasedPluginRunner`, `run_plugin_loop()` |
| `bundle.rs` | `PluginBundle`, `PluginMeta`, `BehaviorMode`, `to_behavior()` |
| `registry.rs` | `PluginRegistry`: register/get/list plugins by name |
| `orchestration.rs` | Plugin DAG config types plus canonical handoff rule parsing/evaluation |
| `dag_runner.rs` | `PluginDagRunner`: intra-agent DAG orchestration with slot/edge navigation |
| `builtin/` | 11 builtin plugins: code_review, refactor, test_gen, security_audit, doc_gen, pptx_gen, deep_research, image_gen, knowledge_base, plan_gen, verification, superpowers |

### lattice-bus module map

| Module | Purpose |
|--------|---------|
| `pipeline.rs` | `Pipeline`, `PipelineRun`, dry-run validation, handoff/fork control flow |
| `runner.rs` | `AgentRunner`: executes an `AgentProfile` through `lattice_agent::Agent` |
| `profile.rs` | TOML-backed `AgentProfile`, `PluginsConfig`, handoff config, memory/bus config |
| `assembly.rs` | Builds `Agent` instances from profiles, tool registry, system prompt, and credentials |
| `handoff_rule.rs` | Compatibility re-export of the canonical rule language from `lattice-plugin` |
| `registry.rs` / `watcher.rs` | Agent profile loading and hot reload |
| `events.rs` / `micro_agent.rs` / `lib.rs` | Pipeline events, microagent RPC/pub-sub, bus types |

### Model resolution flow

1. **Normalize** (`normalize_model_id`): lowercase, strip OpenRouter prefixes (`anthropic/`), strip Bedrock prefixes (`us.anthropic.`) and suffixes (`-v1:0`), convert Claude dots to hyphens
2. **Alias resolution**: check catalog aliases (`"sonnet"` → `"claude-sonnet-4-6"`)
3. **Catalog lookup**: find `ModelCatalogEntry` by canonical ID
4. **Provider selection**: sort providers by `priority`, pick the first one with a valid credential env var
5. **Permissive fallback**: if not in catalog and looks like `provider/model`, construct from provider defaults table

### Credential resolution

Credentials come from **environment variables by default**, with an optional
`with_credentials(HashMap)` override for programmatic injection (used in testing
and embedding scenarios). Provider credential map in `router.rs`.

### Handoff rule engine

Agent profiles use TOML `[[handoff.rules]]` for deterministic routing:

```toml
[[handoff.rules]]
condition = { field = "confidence", op = ">", value = "0.5" }
target = "refactor"

[[handoff.rules]]
condition = { field = "issues[any].severity", op = "==", value = "critical" }
target = "escalate"

[[handoff.rules]]
default = true
```

Evaluation: rules checked in order, first match wins. Supports `condition` (single), `all` (AND), `any` (OR), `default` (unconditional). `[any]` iterates array elements. Operators: `==`, `!=`, `<`, `>`, `<=`, `>=`, `contains`.

Targets can be a single agent (`"refactor"`) or a fork (`"fork:security,performance"`). Fork runs multiple agents in parallel via `tokio::spawn`, merges outputs as `{agent_name: output}` JSON, and feeds the merged result to the next agent in the chain. TOML syntax: `target = "fork:A,B"` → `HandoffTarget::Fork(["A","B"])`.

### Plugin DAG orchestration

Agent profiles can include a `[plugins]` section for intra-agent DAG execution:

```toml
[plugins]
entry = "review"
shared_tools = ["read_file", "grep"]

[[plugins.slots]]
name = "review"
plugin = "CodeReview"
max_turns = 3
behavior = { mode = "strict", confidence_threshold = 0.7, max_retries = 3 }

[[plugins.slots]]
name = "refactor"
plugin = "Refactor"
max_turns = 5

[[plugins.edges]]
from = "review"
rule = { condition = { field = "confidence", op = ">", value = "0.5" }, target = "refactor" }

[[plugins.edges]]
from = "review"
rule = { default = true }
```

Each slot runs a plugin via `ErasedPluginRunner`. the DAG runner accumulates context across slots (`context["<slot_name>"] = output`). Edges use the same handoff rule engine for routing.

### JSON schema validation

When `output_schema` is set in an agent profile, the runner validates LLM output against the schema (jsonschema crate). Invalid output triggers up to 2 retries with correction hints.

### Tool execution boundary

All tool execution is async via tokio:

```
lattice-agent: Agent::run().await → auto tool loop (LLM → ToolCall → execute → submit → continue)
               DefaultToolExecutor: tokio::fs, tokio::process, regex (grep), reqwest::Client
```

Default tool set (7 tools): read_file, grep (pure Rust regex), write_file, list_directory, bash, patch, web_search.

### Error taxonomy

`LatticeError` Rust enum in `lattice-core`. PyO3 exception hierarchy in `lattice-python/errors.rs`:

```
Exception
  └─ LatticeError
       ├─ RateLimitError (.retry_after, .provider)
       ├─ AuthenticationError (.provider)
       ├─ ModelNotFoundError (.model)
       ├─ ProviderUnavailableError (.provider, .reason)
       ├─ ContextWindowExceededError (.tokens, .limit)
       ├─ ToolExecutionError (.tool, .message)
       ├─ StreamingError (.message)
       ├─ ConfigError (.message)
       └─ NetworkError (.message, .status)
```

Additional error types in higher crates:
```
lattice-agent: ToolError (Execution, NotFound, InvalidArgs, McpUnreachable, Timeout)
lattice-plugin: PluginError (Parse, Validation, MissingTool, ContextExceeded, MaxTurnsExceeded, OutputTooLarge, Escalated)
lattice-plugin: DAGError (SlotNotFound, PluginNotFound, MaxSlotTransitionsExceeded, ForkNotSupportedInDag, etc.)
```

Retryable errors: `RateLimit` and `ProviderUnavailable`. Default policy: 3 retries, 1s base delay, 60s max, jittered exponential backoff.

### Streaming pipeline

Two SSE parsers (`OpenAiSseParser`, `AnthropicSseParser`) implement `SseParser` trait. Both are **stateful**: track tool call IDs across chunks. Raw HTTP `bytes_stream` + manual line-based parsing (no `reqwest-eventsource` dependency).

### Catalog

`src/catalog/data.json` is the built-in model catalog (manually maintained).

### Runtime model

LATTICE is fully async. No static runtimes in the Rust engine layer:

- **CLI path**: `#[tokio::main]` — the tokio runtime is created at main entry, all `.await` flows through it
- **Pipeline path**: `Pipeline::run()` is `async fn` — callers must `.await` it (no `block_on` wrapper)
- **MicroAgent path**: `MicroAgent::spawn()` is `async fn` — no `BUS_RT` static runtime
- **Embedded path**: callers inject their own `Handle` — LATTICE never creates a tokio Runtime internally
- **Python path**: `SHARED_RUNTIME` in `lattice-python` is the only exception — it bridges PyO3 sync→async and uses `block_in_place` for nested contexts. This is intentional and correct; it never calls Pipeline::run() or MicroAgent::spawn()
