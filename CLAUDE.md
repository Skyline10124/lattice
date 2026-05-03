# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

**LATTICE** is a model-centric LLM engine split into eight Rust crates. The user specifies a model name or alias (e.g., `"sonnet"`), and the engine resolves which provider serves it, picks the best credential, formats the request for the correct API protocol, and handles streaming, tool calls, and retries. There is no `set_provider()` call — provider selection is automatic.

**Crate architecture** (one-way deps):

```
lattice-tui           Terminal UI (ratatui)
  → lattice-cli         CLI: resolve, run, print, validate, debug, models, doctor
    → lattice-harness     Pipeline / AgentRunner / PluginDagRunner, TOML profiles, handoff, events
      → lattice-agent       Async Agent::run(), tool execution (tokio), conversation state
        → lattice-core       resolve() + chat() — model routing + streaming inference
      → lattice-plugin       ErasedPlugin, PluginRegistry, PluginDagRunner, 9 builtin plugins
  → lattice-python       PyO3 bindings (pip package: lattice-core)
```

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
  → lattice_core::resolve("sonnet")           → ResolvedModel
  → lattice_core::chat(resolved, msgs)         → impl Stream<Item = StreamEvent>
  → lattice_agent::Agent::new(resolved)        → agent.run("prompt", 10).await (async, with tool loop)
  → lattice_harness::Pipeline::new()           → pipeline.run("start", "input").await (async, plugin DAG or agent runner)
```

### Crate map

| Crate | Purpose |
|-------|---------|
| `lattice-core` | Model resolution, streaming inference, retry, token estimation. **No PyO3.** |
| `lattice-agent` | `Agent` struct: async multi-turn conversation, tool execution (tokio), token budget, provider fallback, `Memory` trait + `InMemoryMemory` + `SharedMemory` types |
| `lattice-harness` | `Pipeline`, `AgentRunner`, `PluginDagRunner`, `SqliteMemory`, TOML-based agent profiles, handoff rule engine with fork parallelism, hot reload, JSON schema validation, WebSocket events, `ToolRegistry` |
| `lattice-plugin` | `Plugin` trait, `ErasedPlugin`, `PluginRegistry`, 9 builtin plugins (CodeReview, Refactor, TestGen, SecurityAudit, DocGen, PptxGen, DeepResearch, ImageGen, KnowledgeBase), `Behavior` (Strict/Yolo), `PluginRunner`/`ErasedPluginRunner` |
| `lattice-cli` | CLI binary: `resolve`, `models`, `doctor`, `run`, `print`, `debug`, `validate`, `new agent` |
| `lattice-tui` | Terminal UI (ratatui-based — early stage) |
| `lattice-python` | PyO3 bindings: `LatticeEngine`, exceptions, `StreamIterator` (pip: `lattice-core`) |
| `lattice-bus` | Pub/sub event bus for inter-agent communication |

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

### lattice-harness module map

| Module | Purpose |
|--------|---------|
| `pipeline` | `Pipeline`: multi-agent chain/fork execution, handoff rule evaluation, dry_run validation (async) |
| `runner` | `AgentRunner`: single-agent run loop with JSON schema validation + retry |
| `dag_runner` | `PluginDagRunner`: intra-agent DAG orchestration with slot/edge navigation |
| `profile` | `AgentProfile` + `PluginsConfig`: TOML-deserialized agent configuration, plugin slots/edges |
| `tools` | `ToolRegistry`, `ToolHandler`, `merge_tool_definitions()` |
| `handoff_rule` | `HandoffTarget` (Single/Fork), `HandoffRule`, `HandoffCondition`: TOML routing with AND/OR/default |
| `registry` | `AgentRegistry`: load agent profiles from directory, hot reload via `notify` |
| `events` | `PipelineEvent` + `EventBus`: broadcast channel for pipeline status events |
| `watcher` | File watcher for agent directory changes, triggers registry reload |
| `ws` | WebSocket endpoint for live pipeline events (feature-gated behind `axum`) |
| `memory` | `SharedMemory` trait + `SqliteMemory` (FTS5), re-exports agent memory types |

### lattice-plugin module map

| Module | Purpose |
|--------|---------|
| `erased` | `ErasedPlugin` trait + blanket impl over `Plugin` |
| `bundle` | `PluginBundle`, `PluginMeta`, `BehaviorMode`, `to_behavior()` |
| `registry` | `PluginRegistry`: register/get/list plugins by name |
| `erased_runner` | `run_plugin_loop()` shared loop + `ErasedPluginRunner` struct |
| `builtin/` | 9 builtin plugins: code_review, refactor, test_gen, security_audit, doc_gen, pptx_gen, deep_research, image_gen, knowledge_base |
| `lib.rs` | `Plugin` trait, `Behavior` trait (Strict/Yolo), `PluginRunner`, `PluginHooks`, `PluginConfig` |

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

Targets can be a single agent (`"refactor"`) or a fork (`"fork:security,performance"`). Fork runs multiple agents in parallel via `std::thread::spawn`, merges outputs as `{agent_name: output}` JSON, and feeds the merged result to the next agent in the chain. TOML syntax: `target = "fork:A,B"` → `HandoffTarget::Fork(["A","B"])`.

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
lattice-harness: DAGError (SlotNotFound, PluginNotFound, MaxSlotTransitionsExceeded, ForkNotSupportedInDag, etc.)
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
