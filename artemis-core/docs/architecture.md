# Artemis Core Architecture

## Overview

Artemis Core is a model-centric LLM engine. The user specifies a model, and the engine resolves which provider serves it, picks the best credential, formats the request for the right API protocol, and handles streaming, tool calls, and retries. There is no `set_provider()` call. Provider selection is automatic.

## Component Diagram

```
User Code (Python)
     │
     ▼
ArtemisEngine (PyO3)
     │
     ▼
ModelRouter ─────────── Catalog (98 models, 37 aliases)
     │                        │
     ▼                        ▼
ResolvedModel              Provider Defaults
(provider, api_key,         (base_url, protocol,
 base_url, protocol,         credential_keys)
 api_model_id)
     │
     ▼
TransportDispatcher
  ┌─────┼──────────┼──────────┐
  │     │          │          │
ChatComp  Anthropic  Gemini   OpenAICompat
Transport  Transport Transport  (Ollama/Groq/
  │        │          │        xAI/DeepSeek/
  ▼        ▼          ▼        Mistral/...)
HTTP (reqwest)
     │
     ▼
SseParser → StreamEvent → Event → Python
```

## Model Resolution Data Flow

When a user calls `set_model("sonnet")` or `run_conversation(..., model="sonnet")`:

1. **Input**: `model_name = "sonnet"` (alias or canonical ID)

2. **Normalize**: `normalize_model_id("sonnet")` → `"sonnet"` (lowercase, strip prefixes like `anthropic/`, strip Bedrock suffixes like `-v1:0`, convert Claude dots to hyphens)

3. **Alias resolution**: `ModelRouter.resolve_alias("sonnet")` checks catalog aliases → `"claude-sonnet-4-6"`

4. **Catalog lookup**: `Catalog.get_model("claude-sonnet-4-6")` → `ModelCatalogEntry` with provider list

5. **Provider selection**: Sort providers by `priority` (ascending). For each provider:
   - Check `resolve_credentials()` → scan `credential_keys` env vars
   - If env var exists and is non-empty, that provider wins
   - If no credentials found for any provider, return the highest-priority provider with `api_key = None`

6. **Return `ResolvedModel`**: contains `provider`, `api_key`, `base_url`, `api_protocol`, `api_model_id`, `context_length`

7. **Permissive fallback**: If the model is not in the catalog and looks like `provider/model` (e.g. `"anthropic/claude-new-model"`), `resolve_permissive()` checks provider defaults and constructs a ResolvedModel from the defaults table.

## Module Map

| Module | Purpose |
|--------|---------|
| `catalog` | Model catalog, aliases, provider defaults, types |
| `router` | ModelRouter: normalize, resolve alias, select provider, resolve credentials |
| `engine` | ArtemisEngine PyClass, Event, ToolCallInfo, PyResolvedModel |
| `provider` | Provider trait, ChatRequest/ChatResponse, ModelRegistry |
| `providers` | Concrete provider implementations (OpenAI, Anthropic, etc.) |
| `transport` | Transport trait, ChatCompletions, Anthropic, Gemini, OpenAICompat, Dispatcher |
| `streaming` | SSE parsers (OpenAI, Anthropic), SseStream, EventStream, StreamEvent |
| `streaming_bridge` | StreamIterator (PyO3 PyIterator over LoopEvent) |
| `agent_loop` | AgentLoop: run with fallback, interrupt, budget tracking |
| `tool_boundary` | ToolCallRequest/ToolCallResult: Rust yields, Python executes |
| `retry` | ErrorClassifier, RetryPolicy with jittered exponential backoff |
| `tokens` | TokenEstimator: rough count, context window check |
| `errors` | ArtemisError enum, Python exception hierarchy, ErrorClassifier |
| `types` | Role, Message, ToolDefinition, ToolCall, FunctionCall, TransportType |
| `mock` | MockProvider for testing |

## Key Types and Relationships

```
ModelCatalogEntry
  ├── canonical_id: String
  ├── display_name: String
  ├── context_length: u32
  ├── aliases: Vec<String>
  └── providers: Vec<CatalogProviderEntry>
        ├── provider_id: String
        ├── api_model_id: String
        ├── priority: u32
        ├── credential_keys: HashMap<String, String>
        ├── base_url: Option<String>
        └── api_protocol: ApiProtocol

ResolvedModel
  ├── canonical_id: String
  ├── provider: String
  ├── api_key: Option<String>
  ├── base_url: String
  ├── api_protocol: ApiProtocol
  ├── api_model_id: String
  └── context_length: u32

ApiProtocol (enum)
  ├── OpenAiChat
  ├── AnthropicMessages
  ├── GeminiGenerateContent
  ├── BedrockConverse
  ├── CodexResponses
  └── Custom(String)
```

## Credential Resolution

Credentials come from environment variables only. No config files, no keychains.

For each `CatalogProviderEntry`, `resolve_credentials()` checks:
1. The entry's `credential_keys` map (field_name → env_var_name)
2. The global `_PROVIDER_CREDENTIALS` table (provider_slug → env_var list)

If the first matching env var is set and non-empty, it returns the value. Otherwise returns `None`.

Key env vars: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `GITHUB_TOKEN` (for Copilot).

## Streaming Pipeline

```
HTTP SSE response
     │
     ▼
reqwest_eventsource::EventSource
     │
     ▼
SseStream<Parser>
  ├── OpenAiSseParser  (for chat_completions protocol)
  ├── AnthropicSseParser (for anthropic_messages protocol)
     │
     ▼
StreamEvent (Token / ToolCallStart / ToolCallDelta / ToolCallEnd / Done / Error)
     │
     ▼
EventStream (implements futures::Stream)
     │
     ▼
StreamIterator (PyO3 PyIterator) → Python for-loop
```

Both parsers are stateful: they track tool call IDs across chunks because OpenAI omits the `id` field from delta chunks after the first one, and Anthropic uses index-based tracking.

## Tool Execution Boundary

Rust and Python split tool execution across a process boundary:

```
Rust: ArtemisEngine.run_conversation()
  → sends request to provider
  → receives response with tool_calls
  → yields Event(kind="tool_call_required", tool_calls=[...])
  → PAUSES

Python: receives Event, executes tools locally
  → calls e.submit_tool_results([(call_id, result), ...])

Rust: resumes conversation
  → appends tool results to message history
  → sends follow-up request
  → yields final events
```

This design keeps Python in control of tool execution (file access, network calls, sandboxed code) while Rust handles API communication, streaming, and retries.

## Error Classification and Retry

```
HTTP response (error status)
     │
     ▼
ErrorClassifier.classify(status_code, body, provider)
  ├── 429     → RateLimit (retryable)
  ├── 401/403 → Authentication (fatal)
  ├── 404     → ModelNotFound (fatal)
  ├── 500/502/503 → ProviderUnavailable (retryable)
  ├── 400 + "context_length_exceeded" → ContextWindowExceeded (fatal)
  └── other   → Network (fatal by default)
     │
     ▼
ErrorClassifier.is_retryable(error)
  → true for RateLimit and ProviderUnavailable
     │
     ▼
RetryPolicy (defaults: max_retries=3, base_delay=1s, max_delay=60s)
  → jittered_backoff(attempt) = min(base * 2^attempt + jitter, max_delay)
     │
     ▼
AgentLoop.run_with_fallback()
  → tries providers in priority order
  → sleeps with jittered backoff between attempts
  → returns first successful result or final error
```

Python exception hierarchy mirrors the Rust enum:

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