<!-- refreshed: 2026-05-17 -->
# External Integrations

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## Snapshot

LATTICE has three integration surfaces: (1) LLM provider APIs via transport adapters in `lattice-llm`, (2) local SQLite persistence for sessions in `lattice-session`, and (3) CLI to API wiring with config and credential injection. The `SessionApi` facade in `lattice-api` composes model routing, agent state, and session storage.

---

## LLM Provider Integration

### Transport Architecture

All provider communication flows through a unified transport layer at `crates/lattice-llm/src/transport/`. The core execution path is at `crates/lattice-llm/src/invocation.rs`:

1. `ModelRouter::resolve()` resolves a model name/alias to a `ResolvedModel` with provider, API protocol, base URL, and API key.
2. The `TransportDispatcher` maps the `ApiProtocol` to a concrete `Transport` implementation.
3. The transport `normalize_request()` converts the internal `ChatRequest` to provider-native JSON.
4. `send_streaming_request()` sends the HTTP POST with authentication, extra headers, and SSRF checks.
5. The response is parsed through the transport's `create_sse_parser()` into a unified `StreamEvent` stream.

### Supported API Protocols

| Protocol | Transport | API Format | Auth Method |
|----------|-----------|------------|-------------|
| `OpenAiChat` | `ChatCompletionsTransport` | OpenAI Chat Completions | `Authorization: Bearer {key}` |
| `AnthropicMessages` | `AnthropicTransport` | Anthropic Messages | `x-api-key: {key}` plus `anthropic-version: 2023-06-01` |
| `GeminiGenerateContent` | `GeminiTransport` | Gemini generateContent | `x-goog-api-key: {key}` |

### Provider Catalog

The model catalog is embedded at compile time in `crates/lattice-llm/src/catalog/data.json`. It defines 26+ provider defaults and 100+ model entries.

### Model Resolution And Routing

`ModelRouter` implements priority-based routing: normalize model name, resolve alias via catalog, look up model entry, scan providers by priority with credential fallback.

### Credential Resolution

Two-tier lookup: (1) `CredentialStore` explicit credentials from config, (2) `PROVIDER_CREDENTIALS_RAW` hardcoded env var mappings. External credentials take priority over environment variables.

---

## Configuration Files And Environment Variables

### Config File (`crates/lattice-cli/src/config.rs`)

- Primary location: `$XDG_CONFIG_HOME/lattice/config.toml`.
- Fallback: `./lattice.toml`.
- TOML format with sections: `core`, `ui`, `providers`.

### Environment Variables

`.env` files loaded via `dotenvy`. Known credential env vars include `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `GEMINI_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `OPENROUTER_API_KEY`, and others.

---

## CLI Integration

### Entry Point

`lattice-cli/src/main.rs` defines the `lattice` binary using `clap` with derive macros.

### Subcommand Summary

| Command | Description |
|---------|-------------|
| `run` | Execute a one-shot prompt with model routing |
| `resolve` | Resolve a model name and display connection details |
| `models` | List available/authenticated models |
| `session` | Manage sessions (status, log, checkout, branch, diff, reset) |
| `doctor` | Diagnostic health check |

---

## Session And SQLite Integration

### Session Graph

`lattice-session` implements a git-style conversation graph:
- **Storage:** SQLite via `rusqlite` v0.32 (`bundled` feature).
- **Concepts:** Branches, commits, HEAD pointers, turn manifests, profile snapshots.
- **`SessionGraphStore` trait:** Abstraction with `SqliteSessionGraphStore` as primary implementation.
- **`SessionApi`:** Facade for session operations (status, log, checkout, branch, diff, reset).

---

## Inter-Crate Integration Points

### Dependency Flow

```
lattice-hooks -> lattice-llm -> lattice-session / lattice-profile / lattice-tools
                                              |
                                              v
                                       lattice-agent
                                              |
                                              v
                                        lattice-api
                                              |
                                              v
                                        lattice-cli
```

Each crate depends on the layers below -- no circular dependencies.

### Key Cross-Crate Wiring

| Integration | From | To | What flows |
|-------------|------|----|-----------|
| Model routing | `lattice-llm` | `lattice-api`, `lattice-cli` | `ModelRouter`, `ResolvedModel` |
| Transport dispatch | `lattice-llm` (internal) | HTTP | `ChatRequest` -> provider APIs |
| Credential injection | `lattice-cli` | `lattice-llm` | `CredentialStore::to_hashmap()` -> `ModelRouter::with_credentials()` |
| Session persistence | `lattice-session` | `lattice-api` | `SessionGraphStore` trait implementation |
| Agent assembly | `lattice-profile` | `lattice-api` | Profile loading to session/turn assembly |
| Tool execution | `lattice-tools` | `lattice-agent` | `ToolExecutor` trait, enabled tools |
| Hook interception | `lattice-hooks` | `lattice-cli` | `Hook` trait, `CompositeHook` |

---

## Current Notes

- `.planning/` is ignored by `.gitignore`; codebase map files need `git add -f` when committing.
- `ApiProtocol::CodexResponses` exists at the type level but is not registered by default.
- `copilot-acp` and `ai-gateway` provider defaults have empty base URLs (placeholders for future use).
- `azure-foundry` has a templated base_url that callers must override before use.

---

*Integration audit: 2026-05-17 (post-Phase 11 deletion)*
