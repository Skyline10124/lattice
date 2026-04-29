# artemis

A fast, model-centric LLM inference engine. Rust core, Python bindings.

**Not** an agent framework. Not a SaaS. Not a visual workflow builder. Just model routing + inference, with a plugin system for building your own agents.

## Design

```
artemis/ (Cargo workspace)
├── artemis-core/       Pure Rust: resolve + chat + chat_complete
├── artemis-agent/      Agent state, tool boundary, retry
├── artemis-memory/     Memory trait + InMemoryMemory
├── artemis-token-pool/ TokenPool trait + UnlimitedPool
└── artemis-python/     PyO3 bindings (resolver only, for now)
```

### Four principles

| | |
|---|---|
| **Fast** | Rust hot path, zero-cost abstractions |
| **Minimal** | No DB, no external services. Just Rust library + catalog.json. |
| **Pluggable** | Overlay pattern for providers, tools, routing rules |
| **Focused** | One thing well: given model + messages -> return response |

### Plugin model **(Design vision, not yet implemented)**

Every plugin is a typed function: `Input -> to_prompt() -> LLM.invoke() -> from_output() -> Output`

The code controls the flow. The LLM is just the inference step -- it doesn't decide what to do, when to stop, or where to hand off.

```python
class CodeReviewPlugin(Plugin):
    def build_input(self, ctx): ...     # what to feed the LLM
    def to_prompt(self, input): ...     # format the prompt
    def from_output(self, raw): ...     # parse + validate
    def should_handoff(self, output): ... # deterministic routing
```

## Quick start

### Rust

```rust
use artemis_core;

// Resolve a model alias to a specific provider + credentials
let resolved = artemis_core::resolve("sonnet")?;
// -> ResolvedModel { provider: "anthropic", api_model_id: "claude-sonnet-4-6", ... }

// Non-streaming: get the full response
let messages = vec![Message { role: Role::User, content: "Hello".into() }];
let response = artemis_core::chat_complete(&resolved, &messages, &[])?;
// -> ChatResponse { content: "...", finish_reason: "stop" }

// Streaming: get tokens as they arrive
let stream = artemis_core::chat(&resolved, &messages, &[])?;
// -> impl Stream<Item = StreamEvent> (Token, ToolCallStart, ToolCallDelta, ToolCallEnd, Done, Error)
```

### Python

```bash
cd artemis-python && maturin develop
```

```python
import artemis_core

engine = artemis_core.ArtemisEngine()
engine.resolve_model("sonnet")
# -> PyResolvedModel(provider="anthropic", api_model_id="claude-sonnet-4-6", ...)

engine.list_authenticated_models()
# -> lists all models with valid credentials in your environment
```

Note: the Python binding currently exposes resolver + model listing only. Chat and streaming are available in Rust. See [ROADMAP.md](ROADMAP.md) for status.

## Current status

Artemis is in **alpha / dogfooding** stage. What works:

- **Model resolution**: 98 models, 37 aliases, 27 provider defaults (23 with base_url)
- **Rust inference**: `resolve()` + `chat()` + `chat_complete()` for OpenAI (chat_completions) and Anthropic (messages) protocols
- **Thinking mode**: DeepSeek v4-pro (OpenAI reasoning_content), MiniMax M2.7 (Anthropic thinking_delta)
- **HTTPS enforced**: non-localhost HTTP base URLs rejected at the engine level
- **Tested providers**: deepseek, minimax (Anthropic protocol), opencode-go (14 models all pass)
- **409+ tests pass, 0 fail**, clippy + fmt clean

What's not yet done: Python binding is resolver-only (chat/streaming not yet exposed), Gemini main path, error/retry贯通, production hardening. Not yet production ready.

## Project structure

```
artemis/                 Git root (Cargo workspace)
├── artemis-core/        Pure Rust: resolve, chat, chat_complete
│   ├── src/
│   │   ├── catalog/     Model catalog, aliases, provider defaults (98+ models)
│   │   ├── router.rs    Model resolution, credential resolution
│   │   ├── provider.rs  Shared HTTP client, ChatRequest/ChatResponse types
│   │   ├── transport/   Unified Transport trait, dispatcher, per-protocol transports
│   │   ├── streaming.rs SSE parsers (OpenAI format, Anthropic format)
│   │   ├── retry.rs     Error classification, jittered exponential backoff
│   │   ├── tokens.rs    tiktoken integration + token estimation
│   │   ├── errors.rs    ArtemisError enum
│   │   └── types.rs     Role, Message, ToolDefinition, ToolCall, FunctionCall
│   ├── tests/e2e/       End-to-end + regression tests
│   ├── docs/            Architecture, ideas, code review
│   ├── benches/         Criterion benchmarks
│   └── examples/        Usage examples
├── artemis-agent/       Agent state, tool boundary, retry
├── artemis-memory/      Memory trait + InMemoryMemory
├── artemis-token-pool/  TokenPool trait + UnlimitedPool
└── artemis-python/      PyO3 bindings (resolver only)
```

## Why not X?

- **OpenRouter / LiteLLM**: They're SaaS/model gateways. artemis is a library you embed.
- **LangGraph / CrewAI**: Heavy multi-agent frameworks. artemis gives you primitives, not orchestration.
- **n8n / Dify**: Visual workflow builders for non-developers. artemis is for developers.
- **MCP**: Model-to-tool protocol. Complementary -- plugins reference tools via MCP internally.
- **A2A**: Agent-to-agent protocol. Reference for artemis's handoff layer.

## Requirements

- Rust 1.80+
- Python 3.12+ (optional, for Python bindings)
- Credentials in environment variables: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, etc.

## Development

```bash
cd artemis-core

cargo build                    # Debug build
cargo test --no-default-features  # Unit tests (no Python needed)
cargo test                        # With Python runtime
cargo clippy --no-default-features -- -D warnings
cargo fmt --check
cargo bench

# Python bindings (from artemis-python/)
cd artemis-python && maturin develop
```

See [CLAUDE.md](CLAUDE.md) for detailed development guide.

## License

MIT
