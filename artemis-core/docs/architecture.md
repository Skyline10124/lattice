# Artemis Core Architecture

## Overview

Artemis Core is a model-centric LLM engine. The user specifies a model, and the engine resolves which provider serves it, picks the best credential, formats the request for the right API protocol, and handles streaming, tool calls, and retries. There is no `set_provider()` call. Provider selection is automatic.

## Component Diagram

```
User Code (Python / Rust)
     │
     ▼
resolve(model_name) → chat(resolved, messages)
     │
     ▼
ModelRouter ─────────── Catalog (98+ models, 37 aliases)
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
SseParser → StreamEvent
```

## Model Resolution Data Flow

When a user calls `resolve("sonnet")`:

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
| `catalog` | Model catalog, aliases, provider defaults, `ApiProtocol`, `ResolvedModel` types |
| `router` | `ModelRouter`: normalize model IDs, resolve aliases, select provider by priority, resolve credentials from env vars |
| `provider` | `ChatRequest`/`ChatResponse` types, shared HTTP client factory |
| `transport` | `Transport` trait, `TransportDispatcher`, ChatCompletions, Anthropic, Gemini, OpenAICompat transports |
| `streaming` | SSE parsers (OpenAI, Anthropic), `SseStream`, `EventStream`, `StreamEvent` enum |
| `retry` | `ErrorClassifier`, `RetryPolicy` with jittered exponential backoff |
| `tokens` | `TokenEstimator`: tiktoken for OpenAI models, rough char/4 estimation for others |
| `errors` | `ArtemisError` Rust enum + error classification |
| `types` | `Role`, `Message`, `ToolDefinition`, `ToolCall`, `FunctionCall` |

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
EventStream (implements futures::Stream<Item = StreamEvent>)
     │
     ▼
Consumer code (e.g., chat_complete() accumulator, or Python via PyO3)
```

Both parsers are stateful: they track tool call IDs across chunks because OpenAI omits the `id` field from delta chunks after the first one, and Anthropic uses index-based tracking.

## Tool Execution Boundary

Tool execution is a higher-level concern handled by consumer code (e.g., the `artemis-python` crate or an agent loop). The core library provides the building blocks:

```
Consumer code:
  → calls chat(&resolved, &messages, &tools)
  → receives StreamEvent::ToolCallStart/Delta/End
  → executes tool locally
  → builds new Message with tool results
  → calls chat() again to continue the conversation
```

This design keeps the consumer in control of tool execution (file access, network calls, sandboxed code) while artemis-core handles API communication, streaming, and retries.

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
```

Retryable errors: `RateLimit` and `ProviderUnavailable`. Provider-level fallback is handled by consumer code (e.g., try provider A, if RateLimit → try provider B).