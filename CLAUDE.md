# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

**Artemis Core** is a model-centric LLM engine written in Rust with Python bindings via PyO3 / maturin. The user specifies a model name or alias (e.g., `"sonnet"`), and the engine resolves which provider serves it, picks the best credential, formats the request for the correct API protocol, and handles streaming, tool calls, and retries. There is no `set_provider()` call — provider selection is automatic.

- **Rust crate**: `artemis-core` (cdylib + rlib) at `artemis-core/`
- **Python package**: `artemis-core` built via [maturin](https://github.com/PyO3/maturin), pyproject.toml at `artemis-core/pyproject.toml`

## Build, test, lint

All commands run from the `artemis-core/` directory.

```bash
# Build the Rust crate
cargo build

# Release build
cargo build --release

# Build and install Python bindings into the active venv
maturin develop

# Run all Rust unit tests (no Python runtime required)
cargo test --no-default-features

# Run with Python runtime (for PyErr roundtrip tests)
cargo test

# Run a single test
cargo test --no-default-features <test_name>

# Run benchmarks
cargo bench

# Lint (treat warnings as errors, per CI)
cargo clippy --no-default-features -- -D warnings

# Format check
cargo fmt --check

# Format code
cargo fmt
```

**CI** (`.github/workflows/ci.yml`): runs `cargo test --no-default-features`, `cargo clippy`, `cargo fmt --check`, and Python smoke tests (3.12, 3.13, 3.14) via `maturin develop` + import check.

**Why `--no-default-features`**: the default features pull in `pyo3/auto-initialize` and `pyo3/extension-module`, which require a linked Python runtime. Most unit tests don't need Python. In CI and local dev, always prefer `--no-default-features` for faster test runs.

## Architecture

The engine has a 7-layer pipeline:

```
User Code (Python)
  → ArtemisEngine (PyO3 pyclass, engine.rs)
    → ModelRouter (router.rs) → Catalog (catalog/) — 98+ models, 37 aliases, provider defaults
      → ResolvedModel (provider, api_key, base_url, api_protocol, api_model_id, context_length)
        → TransportDispatcher (transport/dispatcher.rs)
          → ChatCompletions | Anthropic | Gemini | OpenAICompat transports
            → HTTP (reqwest)
              → SseParser (OpenAiSseParser | AnthropicSseParser)
                → StreamEvent → Event → Python
```

### Module map

| Module | Purpose |
|--------|---------|
| `catalog` | Model catalog, aliases, provider defaults, `ApiProtocol`, `ResolvedModel` types |
| `router` | `ModelRouter`: normalize model IDs, resolve aliases, select provider by priority, resolve credentials from env vars |
| `engine` | `ArtemisEngine` PyClass, `Event`, `ToolCallInfo`, `PyResolvedModel` — the Python-facing API |
| `provider` | `Provider` async trait, `ChatRequest`/`ChatResponse`, `ModelRegistry` |
| `providers/` | Concrete providers: `openai`, `anthropic`, `deepseek`, `gemini`, `groq`, `mistral`, `ollama`, `xai` |
| `transport/` | `Transport` trait + `TransportDispatcher`, format conversion for each API protocol |
| `streaming` | SSE parsers (OpenAI, Anthropic), `SseStream`, `EventStream`, `StreamEvent` enum |
| `streaming_bridge` | `StreamIterator` — PyO3 `PyIterator` over `LoopEvent` for Python `for` loops |
| `agent_loop` | `AgentLoop`: multi-turn conversation loop with interruption and provider fallback |
| `tool_boundary` | `ToolCallRequest`/`ToolCallResult`: Rust yields tool calls, Python executes them |
| `retry` | `ErrorClassifier`, `RetryPolicy` with jittered exponential backoff |
| `tokens` | `TokenEstimator`: tiktoken for OpenAI models, rough char/4 estimation for others |
| `errors` | `ArtemisError` Rust enum + Python exception hierarchy (9 subclasses) |
| `types` | `Role`, `Message`, `ToolDefinition`, `ToolCall`, `FunctionCall`, deprecated `TransportType` |
| `mock` | `MockProvider` for tests |

### Model resolution flow

1. **Normalize** (`normalize_model_id`): lowercase, strip OpenRouter prefixes (`anthropic/`), strip Bedrock prefixes (`us.anthropic.`) and suffixes (`-v1:0`), convert Claude dots to hyphens (`claude-sonnet-4.6` → `claude-sonnet-4-6`)
2. **Alias resolution**: check catalog aliases (`"sonnet"` → `"claude-sonnet-4-6"`)
3. **Catalog lookup**: find `ModelCatalogEntry` by canonical ID
4. **Provider selection**: sort providers by `priority`, pick the first one with a valid credential env var
5. **Permissive fallback**: if not in catalog and looks like `provider/model`, construct from provider defaults table

### Credential resolution

Credentials come from **environment variables only**. `_PROVIDER_CREDENTIALS` table in `router.rs` maps provider slugs to env var names (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `GITHUB_TOKEN`, etc.).

### Tool execution boundary

Rust and Python split tool execution across the process boundary:

```
Rust: run_conversation() → yields Event(kind="tool_call_required", ...) → PAUSES
Python: receives event, executes tool locally → calls e.submit_tool_results(...)
Rust: resumes conversation with tool results appended to message history
```

### Error taxonomy

`ArtemisError` Rust enum maps to a Python exception hierarchy via `From<ArtemisError> for PyErr`:

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

Retryable errors: `RateLimit` and `ProviderUnavailable`. Default policy: 3 retries, 1s base delay, 60s max, with jittered exponential backoff.

### Streaming pipeline

Two SSE parsers (`OpenAiSseParser`, `AnthropicSseParser`) both implement `SseParser` trait. Both are **stateful**: they track tool call IDs across chunks because OpenAI omits the `id` from delta chunks after the first one, and Anthropic uses index-based tracking.

### Catalog

`src/catalog/data.json` is generated from the [hermes-agent](https://github.com/astrin/hermes-agent) `model-centric` branch by `scripts/generate_catalog.py`. The catalog contains 98+ model entries, 37 aliases, and provider defaults for 20+ providers.

## Important conventions

- `#![allow(deprecated)]` at the top of many files suppresses pyo3 deprecation warnings. This is intentional during early development.
- `ApiProtocol` (in `catalog/types.rs`) is the canonical protocol enum. `TransportType` (in `types.rs`) is deprecated and will be removed.
- The repository structure is non-standard: `artemis-core/` is the project root for Cargo/pyproject, but the git root is `artemis/`. All cargo/maturin commands must run from `artemis-core/`, while git commands run from `artemis/`.
- `.sisyphus/notepads/` contains task history and context from prior development waves. These are reference material, not active code.
