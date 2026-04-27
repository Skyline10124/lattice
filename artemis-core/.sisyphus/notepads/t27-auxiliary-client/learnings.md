## T27: auxiliary_client Analysis + Rust Reimplementation Plan

### Key Findings

- File is 3,445 lines at `/home/astrin/.hermes/hermes-agent/agent/auxiliary_client.py`
- 59 functions/classes classified as MUST reimplement in Rust
- 6 functions classified as NEEDS SPLIT (too large / mixed concerns)
- 8 functions classified as CAN stay in Python (async event-loop management, monkey-patching)
- 12 adapter classes for Codex → chat.completions and Anthropic → chat.completions protocol translation
- 17 external Python dependencies across hermes_cli, agent submodules, openai SDK, utils

### Recommended Rust Module Structure
13 modules: providers/{openrouter,nous,codex,anthropic,custom,api_key,bedrock}, resolution, cache, call, error, config, adapters, public, models

### Architecture Notes
- Provider resolution chain maps naturally to `Vec<Box<dyn ProviderResolver>>` pattern
- Client adapters map to `enum AuxiliaryClient` with `ChatCompletions` trait
- Error detection via string scanning should become proper enum matching in Rust
- Python's async event-loop binding complexity disappears in Tokio model
- `neuter_async_httpx_del` / `_force_close_async_httpx` have no Rust equivalent — Drop handles cleanup

### Biggest Reimplementation Challenges
1. `resolve_provider_client` (435 lines) — needs per-auth_type dispatch refactoring
2. `call_llm` / `async_call_llm` (494 lines duplicated) — should be generic over async runtime
3. Error detection via substring search — needs proper error type design
