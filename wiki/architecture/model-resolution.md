# Model Resolution

Model resolution maps a user-facing model name to a provider-specific `ResolvedModel`.

## Flow

```text
"sonnet"
  → normalize_model_id
  → alias lookup
  → catalog entry
  → provider candidates sorted by priority
  → credential lookup
  → validate base URL
  → ResolvedModel
```

## Provider Selection

Provider entries are evaluated by priority. A provider with available credentials wins over a credentialless provider at the same priority. Credentialless providers are valid for local or unauthenticated endpoints.

If the model is not in the catalog but uses `provider/model` form, Runtime can resolve it through provider defaults when the provider is known.

## Security

`ModelRouter::resolve()` validates provider base URLs on the Rust path. The validation rejects malformed URLs and private or reserved network targets where they are not explicitly allowed. This keeps all Rust callers under the same security contract.

## Credential Sources

Credentials come from environment variables or caller-provided credential maps. Common variables include:

| Provider | Variable |
| --- | --- |
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| MiniMax | `MINIMAX_API_KEY` |
| Gemini | `GEMINI_API_KEY` |
| DashScope | `DASHSCOPE_API_KEY` |
| Moonshot | `MOONSHOT_API_KEY` |
