
## T24: Provider Adapter Integration Tests

### Provider Names, Streaming, and Tools Matrix

| Provider | Struct | name() | streaming | tools |
|---|---|---|---|---|
| OpenAI | OpenAIProvider | "openai" | true | true |
| Anthropic | AnthropicProvider | "anthropic" | false | true |
| Gemini | GeminiProvider | "gemini" | true | true |
| Ollama | OllamaProvider | "ollama" | false | true |
| Groq | GroqProvider | "groq" | false | true |
| xAI | XAIProvider | "xai" | false | true |
| DeepSeek | DeepSeekProvider | "deepseek" | true | true |
| Mistral | MistralProvider | "mistral" | true | true |

### Key Observations

- All providers import from `artemis_core::providers::*` (e.g. `artemis_core::providers::openai::OpenAIProvider`)
- Provider trait is in `artemis_core::provider::{Provider, ChatRequest, ChatResponse, ProviderError}`
- `ChatRequest::new(messages, tools, resolved)` sets `model` from `resolved.api_model_id`, `stream` defaults to `false`
- ProviderError has 4 variants: General, Api, Stream, NotFound — all use `thiserror`
- All providers have `Default` impls; some have `with_transport()` constructors
- 38 integration tests cover existence, names, streaming/tools, ChatRequest building, error types, boxing, uniqueness

### Test Results
- 38/38 passed, 0 failed, 0 ignored
- Full suite: 336 passed, 9 ignored across all test binaries
