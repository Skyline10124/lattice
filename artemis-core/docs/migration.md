# Migrating from hermes to artemis-core

This guide covers moving Python code from the hermes provider-centric API to the artemis-core model-centric API.

## The big shift: provider-centric to model-centric

hermes asked you to think about *providers first*. You configured an Anthropic provider, an OpenAI provider, then picked a model within each one. artemis-core flips that around. You say which *model* you want, and the engine figures out the provider, credentials, and endpoint automatically.

What used to be a three-step dance (configure provider, set credentials, choose model) is now one call:

```python
engine.set_model("sonnet")
```

The engine resolves "sonnet" to the canonical model ID, finds the right provider, loads credentials from your environment, and you're ready to go.

## Side-by-side API comparison

| Concept | hermes | artemis-core |
|---------|--------|--------------|
| Initialize engine | `HermesClient(config_path)` | `ArtemisEngine()` |
| Select model | `client.set_provider("anthropic"); client.set_model("claude-3-sonnet")` | `engine.set_model("sonnet")` |
| Provider routing | Manual: user picks provider | Automatic: derived from model |
| Credentials | Configured per-provider in config.yaml | Auto-resolved from environment variables |
| Run conversation | `client.chat(messages)` | `engine.run_conversation(messages, tools)` |
| Streaming | `client.stream(messages)` | Iterate events from `run_conversation()` |
| Tool calls | `client.chat(messages, tools); client.submit_tool_results(...)` | `engine.run_conversation(messages, tools); engine.submit_tool_results(results)` |
| Custom model | Edit config.yaml + restart | `engine.register_model(entry)` at runtime |
| Model listing | `client.list_providers()` then drill down | `engine.list_models()` |
| Authenticated models | Check each provider separately | `engine.list_authenticated_models()` |
| Model resolution | Implicit (provider + model combo) | `engine.resolve_model("sonnet")` returns full details |
| Interrupt | Not supported | `engine.interrupt()` |
| Token usage | Per-provider tracking | `engine.get_token_usage()` |

## Key architectural differences

### 1. Model as the primary abstraction

In hermes, the provider was the top-level concept. You chose Anthropic, then chose a model within it. In artemis-core, the model is the top-level concept. The engine maintains a catalog that maps model names and aliases to providers, so you never need to specify a provider directly.

This means:

- Model aliases work everywhere. `"sonnet"`, `"claude-sonnet-4-6"`, and the canonical ID all resolve to the same model.
- Multi-provider models are supported. A single model can have multiple provider entries with different priority and weight values. The engine picks the best one based on availability and credentials.
- Switching models is trivial. Call `set_model()` again and the engine reroutes automatically.

### 2. No config.yaml required

hermes relied on a YAML configuration file that defined providers, credentials, and model mappings. artemis-core loads credentials from environment variables (like `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) and ships with a built-in model catalog. You can still register custom models at runtime with `register_model()`, but there's no static config file to maintain.

### 3. Event-based conversation flow

hermes returned complete responses. artemis-core returns events, which gives you more control:

- `"token"` events carry content fragments (for streaming display)
- `"tool_call_required"` events carry tool call requests (for the tool calling loop)
- `"done"` events signal completion with a finish reason

This means a single `run_conversation()` call can return both content and tool calls in one batch of events, rather than requiring separate method calls for each.

### 4. Stateful engine

The ArtemisEngine holds conversation state between turns. After a tool call, you don't need to reconstruct the full message history. Call `submit_tool_results()` and the engine continues from where it left off. This is a significant simplification compared to hermes, where you had to manage message arrays yourself.

## Step-by-step migration

### Step 1: Replace imports

```python
# Before (hermes)
from hermes import HermesClient

# After (artemis-core)
from artemis_core import ArtemisEngine
```

### Step 2: Replace initialization

```python
# Before (hermes)
client = HermesClient(config_path="config.yaml")

# After (artemis-core)
engine = ArtemisEngine()
```

No config file needed. The engine loads its catalog and credentials automatically.

### Step 3: Replace model selection

```python
# Before (hermes)
client.set_provider("anthropic")
client.set_model("claude-3-sonnet")

# After (artemis-core)
engine.set_model("sonnet")
```

One call instead of two. The alias "sonnet" resolves to the right provider and model automatically.

### Step 4: Replace conversation calls

```python
# Before (hermes)
response = client.chat(messages)

# After (artemis-core)
events = engine.run_conversation(messages, tools)
for event in events:
    if event.kind == "token":
        print(event.content)
    elif event.kind == "tool_call_required":
        # handle tool calls
    elif event.kind == "done":
        print(f"Finished: {event.finish_reason}")
```

Note: `run_conversation()` requires a `tools` argument. Pass an empty list `[]` if you're not using tools.

### Step 5: Replace tool result submission

```python
# Before (hermes)
client.submit_tool_results([
    {"tool_call_id": "call_1", "output": "result data"},
])

# After (artemis-core)
events = engine.submit_tool_results([
    ("call_1", "result data"),
])
```

Tool results are tuples of `(tool_call_id, output_string)` in artemis-core, not dictionaries.

### Step 6: Replace streaming

```python
# Before (hermes)
for chunk in client.stream(messages):
    print(chunk.content)

# After (artemis-core)
events = engine.run_conversation(messages, [])
for event in events:
    if event.kind == "token":
        print(event.content)
```

Streaming is baked into the event model. No separate stream method needed.

### Step 7: Replace custom model registration

```python
# Before (hermes)
# Edit config.yaml, add provider and model entries, restart

# After (artemis-core)
engine.register_model(
    canonical_id="my-custom-model",
    display_name="My Custom Model",
    provider_id="my-provider",
    api_model_id="custom-v1",
    base_url="https://api.example.com/v1",
    api_protocol_str="chat_completions",
)
```

No config file edits. No restart. Runtime registration works immediately.

## Breaking changes

### `set_provider()` is gone

There is no `set_provider()` method. Provider selection is automatic. If you're calling `set_provider()` anywhere, remove it and use `set_model()` instead.

### Tool results format changed

hermes used dictionaries: `{"tool_call_id": "...", "output": "..."}`. artemis-core uses tuples: `("call_id", "output")`. Update all `submit_tool_results()` calls.

### Response structure changed

hermes returned a single response object with `.content`, `.tool_calls`, etc. artemis-core returns a list of `Event` objects. You need to iterate events and check `event.kind` to extract content, tool calls, or completion status.

### Config.yaml no longer used

If your hermes workflow relied on config.yaml for provider setup, credential storage, or model definitions, you need to:

1. Set environment variables for credentials (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.)
2. Use `register_model()` for any custom models that were in config.yaml
3. Remove config.yaml from your project

### Message construction

Both APIs use `Message` objects, but the constructors differ slightly. In artemis-core:

```python
from artemis_core import Message, Role

msg = Message(role=Role.User, content="Hello")
```

In hermes, the Role enum had different casing. Make sure you're using `Role.User`, `Role.Assistant`, `Role.System`, `Role.Tool` (PascalCase in artemis-core).

### Tools require explicit argument

`run_conversation()` always requires a `tools` argument. In hermes, you could skip tools. In artemis-core, pass `[]` for a plain conversation without tools.

### Model override syntax

```python
# hermes: override provider and model per call
client.chat(messages, provider="openai", model="gpt-4")

# artemis-core: override model per call
events = engine.run_conversation(messages, [], model="gpt-4")
```

The `model` parameter overrides the default model for that call. There's no provider override, because the provider is derived from the model.

## FAQ

### Q: What happens if I don't call `set_model()` before `run_conversation()`?

A: The engine uses the first registered model. If no models are registered, it raises a RuntimeError. Best practice is to always call `set_model()` explicitly.

### Q: Can I use the same model name I used in hermes config.yaml?

A: Probably not exactly. artemis-core uses a different catalog with aliases. Common aliases like `"sonnet"`, `"gpt-4"`, `"opus"` work. Check `engine.list_models()` for available names, and `engine.resolve_model("your-name")` to see what a name resolves to.

### Q: How do I handle multiple providers for the same model?

A: You don't need to. The engine's catalog supports multi-provider entries with priority and weight. When you call `set_model()`, it picks the best available provider. If you want to override, use `resolve_model()` with `provider_override`.

### Q: Where do credentials come from if there's no config.yaml?

A: Environment variables. The engine looks for `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GOOGLE_API_KEY`, etc. based on the provider. Check `engine.list_authenticated_models()` to see which models have valid credentials.

### Q: What about my custom hermes providers?

A: Use `register_model()` to add them at runtime. You specify the provider ID, API model ID, base URL, and protocol string. No config file needed.

### Q: Does `interrupt()` work mid-stream?

A: Yes. Call `engine.interrupt()` during a running conversation and the engine stops processing. This is new functionality that hermes didn't support.

### Q: What's the `api_protocol_str` for `register_model()`?

A: Use one of these strings:
- `"chat_completions"` for OpenAI-compatible APIs
- `"anthropic_messages"` for Anthropic
- `"gemini_generate_content"` for Google Gemini
- `"bedrock_converse"` for AWS Bedrock
- `"codex_responses"` for Codex
- Any custom string for non-standard APIs

### Q: I relied on hermes retry logic. Does artemis-core handle retries?

A: The engine handles provider-level errors internally. Rate limit errors carry `retry_after` metadata. The retry strategy differs from hermes, so check your error handling code if you had custom retry logic.

### Q: How do I get token usage information?

A: Call `engine.get_token_usage()` after a conversation. It returns a dictionary with usage statistics for the current session.