# Artemis Core

[![CI]][ci-url] [![crates.io]][crates-url] [![PyPI]][pypi-url]

Model-centric LLM engine in Rust with Python bindings.

Artemis Core lets you pick a model, not a provider. You call `set_model("sonnet")`
and the engine resolves the alias, finds the best provider with valid credentials,
and routes the request automatically. 98+ models, 37 aliases, 8 MVP providers.

## Quick Start

```python
pip install artemis-core
```

```python
from artemis_core import ArtemisEngine

e = ArtemisEngine()
e.set_model("sonnet")   # resolves to claude-sonnet-4-6 → Anthropic

# Simple chat
events = e.run_conversation(
    messages=[Message(role=Role.User, content="What is Rust?")],
    tools=[]
)
for ev in events:
    if ev.kind == "token":
        print(ev.content)

# Streaming (via SSE under the hood)
# Events arrive as: token → token → tool_call_required → done

# Tool calling
events = e.run_conversation(messages, tools=[my_tool_def])
if events[0].kind == "tool_call_required":
    results = [("call_1", "tool output here")]
    final = e.submit_tool_results(results)

# Model fallback: same model, different providers
# If Anthropic key is missing, engine picks Copilot, Nous, etc.
e.list_authenticated_models()  # shows only models you can reach
```

## Installation

### pip (recommended)

```bash
pip install artemis-core
```

### From source

```bash
cd artemis-core
pip install maturin
maturin develop
```

Requires Rust toolchain (1.75+) and Python 3.10+.

## Supported Models

98+ models across 8 MVP providers: OpenAI, Anthropic, Gemini, Ollama, Groq, xAI, DeepSeek, Mistral.

Aliases let you type short names:

| Alias | Resolves to |
|-------|-------------|
| `sonnet` | `claude-sonnet-4-6` |
| `opus` | `claude-opus-4-7` |
| `gpt5` | `gpt-5.4` |
| `deepseek` | `deepseek-v4-pro` |

Set credentials via environment variables: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`, etc.

## API Reference

### ArtemisEngine

| Method | Description |
|--------|-------------|
| `set_model(model_id)` | Set the active model (alias or canonical ID) |
| `get_model()` | Get the current model ID |
| `list_models()` | List all known model IDs |
| `list_authenticated_models()` | List models with valid credentials |
| `resolve_model(name, provider_override=None)` | Inspect resolution details |
| `register_model(canonical_id, display_name, provider_id, api_model_id, base_url, api_protocol_str)` | Add a custom model |
| `run_conversation(messages, tools, model=None)` | Send a chat request, get events |
| `submit_tool_results(results)` | Continue after tool calls |
| `interrupt()` | Cancel an in-progress request |

### Event

| Field | Type | Description |
|-------|------|-------------|
| `kind` | `str` | `"token"`, `"tool_call_required"`, `"done"` |
| `content` | `Optional[str]` | Text content for token events |
| `tool_calls` | `Optional[List[ToolCallInfo]]` | Tool calls to execute |
| `finish_reason` | `Optional[str]` | `"stop"`, `"tool_calls"`, `"length"` |

### PyResolvedModel

| Field | Description |
|-------|-------------|
| `canonical_id` | Resolved model ID |
| `provider` | Provider that will serve it |
| `api_key` | Credential (masked in repr) |
| `base_url` | API endpoint |
| `api_protocol` | Protocol name |
| `api_model_id` | Provider-specific model ID |
| `context_length` | Max context tokens |

### Exceptions

All inherit from `ArtemisError`:

`RateLimitError`, `AuthenticationError`, `ModelNotFoundError`,
`ProviderUnavailableError`, `ContextWindowExceededError`,
`ToolExecutionError`, `StreamingError`, `ConfigError`, `NetworkError`

## Architecture

Artemis is model-centric. The user picks a model name, and the engine handles everything else: alias resolution, catalog lookup, provider selection, credential discovery, and protocol matching. You never call `set_provider()`. The provider is derived from the model.

See [docs/architecture.md](docs/architecture.md) for the full architecture document.

## License

[Pending]