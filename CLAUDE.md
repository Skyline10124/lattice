# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

**Artemis** is a model-centric LLM engine split into nine Rust crates. The user specifies a model name or alias (e.g., `"sonnet"`), and the engine resolves which provider serves it, picks the best credential, formats the request for the correct API protocol, and handles streaming, tool calls, and retries. There is no `set_provider()` call — provider selection is automatic.

**Crate architecture** (one-way deps):

```
artemis-tui           Terminal UI (ratatui)
  → artemis-cli         CLI: resolve, run, print, validate, debug, models, doctor
    → artemis-harness     Pipeline runner, TOML profiles, hot reload, WebSocket events
      → artemis-agent       AgentLoop, conversation state, tool boundary
        → artemis-memory     Memory trait + InMemoryMemory
        → artemis-token-pool TokenPool trait + UnlimitedPool
          → artemis-core       resolve() + chat() — model routing + inference
      → artemis-plugin       Plugin trait (placeholder)
  → artemis-python       PyO3 bindings (pip package: artemis-core)
```

- **Rust workspace**: Cargo workspace at repo root, all crates under `artemis-*/`
- **Python package**: `artemis-core` (from `artemis-python/`) via [maturin](https://github.com/PyO3/maturin)

## Build, test, lint

All commands run from the **repo root** (Cargo workspace).

```bash
# Build everything
cargo build

# Release build
cargo build --release

# Build and install Python bindings into the active venv
cd artemis-python && maturin develop

# Run all Rust unit tests (no Python runtime required)
cargo test

# Run a single crate's tests
cargo test -p artemis-core

# Run a single test
cargo test -p artemis-core <test_name>

# Run benchmarks
cargo bench -p artemis-core

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
  → artemis_core::resolve("sonnet")      → ResolvedModel
  → artemis_core::chat(resolved, msgs)   → impl Stream<Item = StreamEvent>
  → artemis_agent::Agent::new(resolved)  → send(), submit_tools()
  → artemis_harness::Pipeline::new()     → run() → multi-agent TOML pipeline (sequential + fork parallel)
```

### Crate map

| Crate | Purpose |
|-------|---------|
| `artemis-core` | Model resolution, streaming inference, retry, token estimation. **No PyO3.** |
| `artemis-agent` | `Agent` struct: multi-turn conversation, tool execution, token budget, provider fallback, async API |
| `artemis-memory` | `Memory` trait (`save`/`history`/`search`) + `InMemoryMemory` default impl |
| `artemis-token-pool` | `TokenPool` trait (`acquire`/`release`/`remaining`) + `UnlimitedPool` default impl |
| `artemis-harness` | `Pipeline`, `AgentRunner`, TOML-based agent profiles, handoff rule engine with fork parallelism, hot reload, JSON schema validation, WebSocket events |
| `artemis-plugin` | Plugin trait (placeholder — not yet functional) |
| `artemis-cli` | CLI binary: `resolve`, `models`, `doctor`, `run`, `print`, `debug`, `validate`, `new agent` |
| `artemis-tui` | Terminal UI (ratatui-based — early stage) |
| `artemis-python` | PyO3 bindings: `ArtemisEngine`, exceptions, `StreamIterator` (pip: `artemis-core`) |

### artemis-core module map

| Module | Purpose |
|--------|---------|
| `catalog` | Model catalog, aliases, provider defaults, `ApiProtocol`, `ResolvedModel` |
| `router` | `ModelRouter`: normalize model IDs, resolve aliases, select provider, resolve credentials |
| `provider` | `Provider` trait, `ChatRequest`/`ChatResponse`, shared HTTP client |
| `providers/` | Concrete providers: `openai`, `anthropic`, `deepseek`, `gemini`, `groq`, `mistral`, `ollama`, `xai` |
| `transport/` | Unified `Transport` trait, `TransportDispatcher`, protocol adapters |
| `streaming` | SSE parsers (OpenAI, Anthropic) via `sse_from_bytes_stream`, `StreamEvent` |
| `retry` | `ErrorClassifier`, `RetryPolicy` with jittered exponential backoff |
| `tokens` | `TokenEstimator`: tiktoken for OpenAI models, char/4 estimation for others |
| `errors` | `ArtemisError` enum (pure Rust, no PyO3), `ErrorClassifier` |
| `types` | `Role`, `Message`, `ToolDefinition`, `ToolCall`, `FunctionCall` |

### artemis-harness module map

| Module | Purpose |
|--------|---------|
| `pipeline` | `Pipeline`: multi-agent chain/fork execution, handoff rule evaluation, dry_run validation |
| `runner` | `AgentRunner`: single-agent run loop with JSON schema validation + retry, shared `MEMORY_RT` |
| `profile` | `AgentProfile` + `HandoffConfig`: TOML-deserialized agent configuration |
| `handoff_rule` | `HandoffTarget` (Single/Fork), `HandoffRule`, `HandoffCondition`: TOML routing with AND/OR/default + `[any]` array matching + `fork:A,B` parallel syntax |
| `registry` | `AgentRegistry`: load agent profiles from directory, hot reload via `notify` |
| `events` | `PipelineEvent` + `EventBus`: broadcast channel for pipeline status events |
| `watcher` | File watcher for agent directory changes, triggers registry reload |
| `ws` | WebSocket endpoint for live pipeline events (feature-gated behind `axum`) |
| `dispatch` | Pipeline dispatch: resolve agent model, create AgentRunner |

### Model resolution flow

1. **Normalize** (`normalize_model_id`): lowercase, strip OpenRouter prefixes (`anthropic/`), strip Bedrock prefixes (`us.anthropic.`) and suffixes (`-v1:0`), convert Claude dots to hyphens
2. **Alias resolution**: check catalog aliases (`"sonnet"` → `"claude-sonnet-4-6"`)
3. **Catalog lookup**: find `ModelCatalogEntry` by canonical ID
4. **Provider selection**: sort providers by `priority`, pick the first one with a valid credential env var
5. **Permissive fallback**: if not in catalog and looks like `provider/model`, construct from provider defaults table

### Credential resolution

Credentials come from **environment variables only**. Provider credential map in `router.rs`.

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

### JSON schema validation

When `output_schema` is set in an agent profile, the runner validates LLM output against the schema (jsonschema crate). Invalid output triggers up to 2 retries with correction hints.

### Tool execution boundary

Rust and Python split tool execution across crates:

```
artemis-agent: Agent.send() → yields ToolCallRequired → caller executes tools
               Agent.submit_tools(results) → resumes conversation
```

### Error taxonomy

`ArtemisError` Rust enum in `artemis-core`. PyO3 exception hierarchy in `artemis-python/errors.rs`:

```
Exception
  └─ ArtemisError
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

Retryable errors: `RateLimit` and `ProviderUnavailable`. Default policy: 3 retries, 1s base delay, 60s max, jittered exponential backoff.

### Streaming pipeline

Two SSE parsers (`OpenAiSseParser`, `AnthropicSseParser`) implement `SseParser` trait. Both are **stateful**: track tool call IDs across chunks. Raw HTTP `bytes_stream` + manual line-based parsing (no `reqwest-eventsource` dependency).

### Catalog

`src/catalog/data.json` is the built-in model catalog (manually maintained).

## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore