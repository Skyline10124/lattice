# artemis

A fast, single-binary LLM inference library. Rust core, Python glue, plugin extendable.

**Not** an agent framework. Not a SaaS. Not a visual workflow builder. Just model routing + inference, with a plugin system for building your own agents.

## Design

```
artemis-core (Rust)
  model resolution → credential → HTTP → SSE streaming → retry
  single binary, ~10MB

Python glue layer
  plugin loading → agent composition → handoff routing
  pip install artemis-code-review-plugin
```

### Four principles

| | |
|---|---|
| **Fast** | Rust hot path, zero-cost abstractions |
| **Minimal** | Single binary + catalog.json, no DB, no external services |
| **Pluggable** | Overlay pattern for providers, tools, routing rules |
| **Focused** | One thing well: given model + messages → return response |

### Plugin model

Every plugin is a typed function: `Input → to_prompt() → LLM.invoke() → from_output() → Output`

The code controls the flow. The LLM is just the inference step — it doesn't decide what to do, when to stop, or where to hand off.

```python
class CodeReviewPlugin(Plugin):
    def build_input(self, ctx): ...     # what to feed the LLM
    def to_prompt(self, input): ...     # format the prompt
    def from_output(self, raw): ...     # parse + validate
    def should_handoff(self, output): ... # deterministic routing
```

## Quick start

```bash
# Build
cd artemis-core && cargo build --release

# Python
pip install maturin
cd artemis-core && maturin develop
```

```python
import artemis_core

engine = artemis_core.ArtemisEngine()
engine.resolve_model("sonnet")
# → ResolvedModel(provider="anthropic", model="claude-sonnet-4-6", ...)

events = engine.run_conversation(
    model="sonnet",
    messages=[{"role": "user", "content": "Hello"}]
)
```

## Project structure

```
artemis-core/           Rust crate (cdylib + rlib)
├── src/
│   ├── catalog/        Model catalog, aliases, provider defaults (98+ models)
│   ├── router/         Model resolution, credential resolution
│   ├── engine/         Python-facing API (PyO3)
│   ├── provider/       Provider trait, shared HTTP client
│   ├── providers/      OpenAI, Anthropic, Gemini, DeepSeek, Groq, Mistral, Ollama, xAI
│   ├── transport/      Unified Transport trait, format conversion, dispatcher
│   ├── streaming/      SSE parsers (OpenAI format, Anthropic format)
│   ├── agent_loop/     Multi-turn conversation with fallback
│   ├── tool_boundary/  Rust yields tool calls, Python executes them
│   ├── retry/          Jittered exponential backoff
│   ├── tokens/         tiktoken integration + token estimation
│   ├── errors/         Rust enum → Python exception hierarchy (9 subclasses)
│   └── types/          Role, Message, ToolDefinition, ToolCall, FunctionCall
├── tests/e2e/          End-to-end + regression tests (714 tests)
├── docs/               Architecture, ideas, code review
├── benches/            Criterion benchmarks
└── examples/           Usage examples
```

## Why not X?

- **OpenRouter / LiteLLM**: They're SaaS/model gateways. artemis is a library you embed.
- **LangGraph / CrewAI**: Heavy multi-agent frameworks. artemis gives you primitives, not orchestration.
- **n8n / Dify**: Visual workflow builders for non-developers. artemis is for developers.
- **MCP**: Model-to-tool protocol. Complementary — plugins reference tools via MCP internally.
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

maturin develop                # Python bindings
```

See [CLAUDE.md](CLAUDE.md) for detailed development guide.

## License

MIT
