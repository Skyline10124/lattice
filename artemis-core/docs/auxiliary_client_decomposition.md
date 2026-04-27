# auxiliary_client.py — Decomposition Analysis

> **Source:** `/home/astrin/.hermes/hermes-agent/agent/auxiliary_client.py` (3,445 lines)
> **Date analyzed:** 2026-04-28
> **Purpose:** Shared auxiliary client router for side tasks (compression, vision, web extraction, session search, skills hub, etc.). Provides a single resolution chain so every consumer picks up the best available backend without duplicating fallback logic.

---

## 1. Overview

The file is the **central LLM-sidecar client manager** for Hermes Agent. It handles:

1. **Provider auto-detection** — a priority-ordered chain (OpenRouter → Nous → Custom → Codex → API-key providers) to find a working backend.
2. **Client construction** — builds OpenAI-compatible clients (sync and async) with correct auth, base URLs, and headers for each provider.
3. **Protocol adaptation** — wraps Codex Responses API and Anthropic Messages API behind a uniform `chat.completions.create()` interface.
4. **Client caching** — thread-safe LRU cache to avoid connection churn.
5. **Error recovery** — retries on auth failure (credential refresh), payment exhaustion (provider fallback), unsupported parameters (temperature, max_tokens).
6. **Vision routing** — separate auto-detection chain for multimodal tasks.
7. **Configuration** — reads per-task overrides from `config.yaml` (`auxiliary.<task>.provider|model|base_url|timeout`).

### External Dependencies

| Module | Symbols used | Where |
|--------|-------------|-------|
| `openai` | `OpenAI`, `AsyncOpenAI`, `APIConnectionError`, `APITimeoutError`, `AsyncHttpxClientWrapper` | top-level + `_to_async_client`, error detection, monkey-patch |
| `agent.credential_pool` | `load_pool` | `_select_pool_entry` |
| `agent.anthropic_adapter` | `build_anthropic_kwargs`, `build_anthropic_client`, `build_anthropic_bedrock_client`, `resolve_anthropic_token`, `_is_oauth_token`, `_forbids_sampling_params`, `read_claude_code_credentials`, `_refresh_oauth_token` | `_AnthropicCompletionsAdapter`, `_try_anthropic`, `_refresh_provider_credentials` |
| `agent.transports` | `get_transport` | `_AnthropicCompletionsAdapter.create()` |
| `agent.gemini_native_adapter` | `GeminiNativeClient`, `AsyncGeminiNativeClient`, `is_native_gemini_base_url` | `_resolve_api_key_provider`, `resolve_provider_client`, `_to_async_client` |
| `agent.copilot_acp_client` | `CopilotACPClient` | `resolve_provider_client` |
| `agent.bedrock_adapter` | `has_aws_credentials`, `resolve_bedrock_region` | `resolve_provider_client` |
| `agent.nous_rate_guard` | `nous_rate_limit_remaining` | `_try_nous` |
| `hermes_cli.config` | `load_config`, `get_hermes_home` | config reading |
| `hermes_cli.auth` | `PROVIDER_REGISTRY`, `resolve_api_key_provider_credentials`, `resolve_external_process_provider_credentials`, `resolve_nous_runtime_credentials`, `resolve_codex_runtime_credentials`, `_read_codex_tokens`, `is_provider_explicitly_configured` | credential resolution |
| `hermes_cli.models` | `copilot_default_headers`, `get_nous_recommended_aux_model`, `_should_use_copilot_responses_api` | provider-specific config |
| `hermes_cli.model_normalize` | `normalize_model_for_provider` | `_normalize_resolved_model` |
| `hermes_cli.runtime_provider` | `resolve_runtime_provider`, `_get_named_custom_provider` | custom endpoint resolution |
| `hermes_constants` | `OPENROUTER_BASE_URL` | top-level |
| `utils` | `base_url_host_matches`, `base_url_hostname`, `normalize_proxy_env_vars` | top-level |
| stdlib | `json`, `logging`, `os`, `threading`, `time`, `pathlib`, `types`, `typing`, `urllib.parse`, `base64`, `asyncio`, `inspect`, `re` | various |

---

## 2. Function Classification

Legend:
- **MUST** = Core business logic — must be reimplemented in Rust for `artemis-core`
- **CAN** = Python-specific infrastructure — stays in Python or has no Rust equivalent needed
- **NEEDS SPLIT** = Too large / mixes concerns — decompose before reimplementing

### 2.1 Utility / URL Helpers

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 1 | `_extract_url_query_params(url)` | 57–64 | **MUST** | Pure string/URL manipulation. Trivial to reimplement. |
| 2 | `_to_openai_base_url(base_url)` | 257–271 | **MUST** | URL normalization. Needed by the Rust provider router. |
| 3 | `_codex_cloudflare_headers(access_token)` | 218–254 | **MUST** | JWT parsing + header construction. Needed for Codex client. |
| 4 | `_validate_proxy_env_urls()` | 1112–1138 | **CAN** | Validates env vars at process start. Rust can do its own env validation but this is a CLI bootstrap concern. |
| 5 | `_validate_base_url(base_url)` | 1141–1156 | **CAN** | Pre-flight URL validation. Can be folded into Rust client construction error handling. |
| 6 | `_is_openrouter_client(client)` | 2527–2531 | **MUST** | Simple type introspection — Rust equivalent: trait check or config flag. |

### 2.2 Provider Name Normalization

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 7 | `_normalize_aux_provider(provider)` | 98–115 | **MUST** | Central alias map. Critical for routing. Pure string mapping. |
| 8 | `_normalize_main_runtime(main_runtime)` | 1281–1293 | **MUST** | Field extraction + sanitization. Needed when passing runtime overrides. |
| 9 | `_normalize_vision_provider(provider)` | 2159–2160 | **MUST** | Trivial wrapper — fold into `_normalize_aux_provider` in Rust. |
| 10 | `_normalize_resolved_model(model, provider)` | 1658–1667 | **MUST** | Model name normalization. Depends on `hermes_cli.model_normalize`. |

### 2.3 Model-Specific Logic

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 11 | `_is_kimi_model(model)` | 126–129 | **MUST** | Simple string check. |
| 12 | `_fixed_temperature_for_model(model, base_url)` | 132–149 | **MUST** | Model-specific temperature contract. Returns sentinel or value. |
| 13 | `_compat_model(client, model, cached_default)` | 2534–2541 | **MUST** | Drops OpenRouter-format slugs for non-OpenRouter clients. |

### 2.4 Credential Pool Helpers

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 14 | `_select_pool_entry(provider)` | 274–287 | **MUST** | Wraps `credential_pool.load_pool()`. Rust needs equivalent pool abstraction. |
| 15 | `_pool_runtime_api_key(entry)` | 290–296 | **MUST** | Extracts API key from pool entry. |
| 16 | `_pool_runtime_base_url(entry, fallback)` | 299–310 | **MUST** | Extracts base URL from pool entry. |

### 2.5 Provider Try Functions (Resolution Chain)

These are the individual try-functions that make up the auto-detection chain. Each attempts to construct a client for one provider and returns `(client, model)` or `(None, None)`.

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 17 | `_try_openrouter()` | 919–935 | **MUST** | Core provider detection. Rust needs this. |
| 18 | `_describe_openrouter_unavailable()` | 938–948 | **MUST** | Diagnostic helper. |
| 19 | `_try_nous(vision=False)` | 951–1014 | **MUST** | Nous detection with rate-limit guard, recommended-model lookup, runtime credential resolution. |
| 20 | `_try_custom_endpoint()` | 1159–1194 | **MUST** | Custom endpoint detection with api_mode dispatch (chat_completions, codex_responses, anthropic_messages). |
| 21 | `_try_codex()` | 1197–1219 | **MUST** | Codex OAuth detection with Cloudflare header construction. |
| 22 | `_try_anthropic()` | 1222–1267 | **MUST** | Anthropic native detection with OAuth/API key dispatch, config.yaml base_url override. |
| 23 | `_resolve_api_key_provider()` | 832–912 | **NEEDS SPLIT** | Iterates `PROVIDER_REGISTRY`. Mixes Gemini detection, Kimi/Copilot header logic. Split into per-provider resolvers + a generic iteration driver. |
| 24 | `_get_provider_chain()` | 1296–1308 | **MUST** | Returns ordered list of `(label, try_fn)` pairs. Single source of truth for priority. |

### 2.6 Auth / Credential Readers

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 25 | `_read_nous_auth()` | 714–749 | **MUST** | Reads Nous auth from pool or auth.json. Core credential lookup. |
| 26 | `_nous_api_key(provider)` | 752–754 | **MUST** | Trivial extractor. |
| 27 | `_nous_base_url()` | 757–759 | **MUST** | Env-based default resolution. |
| 28 | `_resolve_nous_runtime_api(*, force_refresh)` | 762–786 | **MUST** | Fresh Nous runtime credential minting. Critical for 401 recovery. |
| 29 | `_read_codex_access_token()` | 789–829 | **MUST** | Codex OAuth token reading with pool fallback + JWT expiry check. |
| 30 | `_read_main_model()` | 1017–1035 | **MUST** | Reads user's main model from config.yaml. |
| 31 | `_read_main_provider()` | 1038–1054 | **MUST** | Reads user's main provider from config.yaml. |
| 32 | `_resolve_custom_runtime()` | 1057–1104 | **MUST** | Resolves custom endpoint from config + env. Critical for "custom" provider. |
| 33 | `_current_custom_base_url()` | 1107–1109 | **MUST** | Trivial accessor. |

### 2.7 Per-Task Configuration

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 34 | `_get_auxiliary_task_config(task)` | 2694–2705 | **MUST** | Reads `auxiliary.<task>` from config.yaml. |
| 35 | `_get_task_timeout(task, default)` | 2708–2719 | **MUST** | Reads per-task timeout. |
| 36 | `_get_task_extra_body(task)` | 2722–2728 | **MUST** | Reads per-task extra_body. |
| 37 | `_resolve_task_provider_model(task, ...)` | 2638–2688 | **MUST** | Central priority: explicit args → config → auto. Determines which provider+model to use. |

### 2.8 Error Detection

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 38 | `_is_payment_error(exc)` | 1311–1328 | **MUST** | Detects HTTP 402 + credit exhaustion messages. Needs Rust error type matching. |
| 39 | `_is_connection_error(exc)` | 1331–1354 | **MUST** | Detects connection failures for provider fallback. |
| 40 | `_is_auth_error(exc)` | 1357–1363 | **MUST** | Detects HTTP 401 for credential refresh. |
| 41 | `_is_unsupported_parameter_error(exc, param)` | 1366–1397 | **MUST** | Generic unsupported-parameter detector (generalizes temperature, max_tokens, seed, top_p). |
| 42 | `_is_unsupported_temperature_error(exc)` | 1400–1406 | **MUST** | Back-compat wrapper. Fold into `_is_unsupported_parameter_error` in Rust. |

### 2.9 Auth Refresh

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 43 | `_evict_cached_clients(provider)` | 1409–1427 | **MUST** | Drops cached clients so fresh creds are used. Needs thread-safe cache manipulation. |
| 44 | `_refresh_provider_credentials(provider)` | 1430–1468 | **MUST** | Refreshes OAuth tokens for Codex/Nous/Anthropic. Core auth flow. |
| 45 | `_refresh_nous_auxiliary_client(...)` | 2386–2425 | **MUST** | Rebuilds Nous client with fresh runtime creds + replaces cache entry. |

### 2.10 Fallback

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 46 | `_try_payment_fallback(failed_provider, task, reason)` | 1471–1515 | **MUST** | Iterates provider chain skipping failed provider. Core resilience logic. |

### 2.11 Content Conversion

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 47 | `_convert_content_for_responses(content)` | 319–364 | **MUST** | Converts `text`/`image_url` → `input_text`/`input_image` for Codex Responses API. |
| 48 | `_convert_openai_images_to_anthropic(messages)` | 2752–2796 | **MUST** | Converts OpenAI image blocks to Anthropic format for MiniMax compat endpoints. |
| 49 | `_is_anthropic_compat_endpoint(provider, base_url)` | 2740–2749 | **MUST** | Detects Anthropic-compatible endpoints. |

### 2.12 Core Resolution

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 50 | `_resolve_auto(main_runtime)` | 1518–1606 | **NEEDS SPLIT** | 88 lines. Two-phase: (1) try main provider + model directly, (2) fall through aggregator chain. Stale `OPENAI_BASE_URL` warning mixed in. Split into: main-provider-first resolver + chain-driver + env-warning-side-effect. |
| 51 | `resolve_provider_client(provider, model, async_mode, ...)` | 1670–2104 | **NEEDS SPLIT** | 435 lines. Central router handling: auto, openrouter, nous, codex, custom, named custom, api_key, external_process, aws_sdk, oauth_device_code, oauth_external. Monolithic dispatch. Split per auth_type: `resolve_*_provider_client()` functions. |

### 2.13 Client Caching

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 52 | `_client_cache_key(provider, ...)` | 2358–2369 | **MUST** | Builds cache key from provider config. |
| 53 | `_store_cached_client(cache_key, client, model, *, bound_loop)` | 2372–2383 | **MUST** | Stores client in `_client_cache` with eviction of replaced entry. |
| 54 | `_get_cached_client(provider, model, async_mode, ...)` | 2544–2635 | **NEEDS SPLIT** | 91 lines. Cache lookup with async loop validation, staleness detection, cache-bust, and fallback to `resolve_provider_client()`. Split: cache lookup/validation (MUST) vs event-loop management (CAN). |
| 55 | `_force_close_async_httpx(client)` | 2461–2478 | **CAN** | Python-specific: manipulates `httpx._client.ClientState`. Not needed in Rust. |
| 56 | `neuter_async_httpx_del()` | 2428–2458 | **CAN** | Python monkey-patch of `AsyncHttpxClientWrapper.__del__`. Not needed in Rust. |
| 57 | `shutdown_cached_clients()` | 2481–2505 | **CAN** | Python-specific cleanup. Rust uses Drop trait naturally. |
| 58 | `cleanup_stale_async_clients()` | 2508–2524 | **CAN** | Python event-loop cleanup. Not needed in Rust. |

### 2.14 Public API

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 59 | `get_text_auxiliary_client(task, *, main_runtime)` | 2109–2131 | **MUST** | Public sync client getter. Delegates to `_resolve_task_provider_model` + `resolve_provider_client`. |
| 60 | `get_async_text_auxiliary_client(task, *, main_runtime)` | 2134–2150 | **MUST** | Public async client getter. Same delegate chain. |
| 61 | `get_available_vision_backends()` | 2182–2204 | **MUST** | Lists currently available vision backends. Used for tool gating and setup. |
| 62 | `resolve_vision_provider_client(provider, model, *, ...)` | 2207–2305 | **NEEDS SPLIT** | 98 lines. Vision-specific resolution with different auto-detection order. Has large `_finalize()` closure. Split: auto-detection logic vs per-provider vision resolvers. |
| 63 | `get_auxiliary_extra_body()` | 2308–2314 | **MUST** | Returns Nous Portal product tags when applicable. |
| 64 | `auxiliary_max_tokens_param(value)` | 2317–2332 | **MUST** | Returns correct max_tokens kwarg for current provider. |
| 65 | `_to_async_client(sync_client, model)` | 1620–1655 | **MUST** | Converts sync client to async. Handles Codex/Anthropic/Gemini/CopilotACP special cases. |

### 2.15 Centralized LLM Call

| # | Function | Lines | Category | Rationale |
|---|----------|-------|----------|-----------|
| 66 | `_build_call_kwargs(provider, model, messages, ...)` | 2800–2858 | **MUST** | Builds kwargs for `chat.completions.create()`. Temperature/model-specific adjustments, max_tokens dispatch, extra_body merging. |
| 67 | `_validate_llm_response(response, task)` | 2861–2889 | **MUST** | Validates response has `.choices[0].message`. Fails fast on malformed payloads. |
| 68 | `call_llm(task, *, provider, model, messages, ...)` | 2892–3166 | **NEEDS SPLIT** | 274 lines. The main sync call API. Orchestrates: provider resolution, client get, kwargs build, vision routing, error retry chains (temperature, max_tokens, auth refresh, payment fallback). Split into: orchestrator + error-recovery strategies. |
| 69 | `async_call_llm(task, *, provider, model, messages, ...)` | 3225–3445 | **NEEDS SPLIT** | 220 lines. Mirror of `call_llm` with `await`. Same split recommendation — error recovery should be a shared strategy, not duplicated. |
| 70 | `extract_content_or_reasoning(response)` | 3169–3222 | **MUST** | Extracts content from response, falling back through reasoning fields. Handles inline think blocks, structured reasoning, OpenRouter reasoning_details. |

### 2.16 Adapter Classes (MUST reimplement)

All 12 adapter classes are **MUST** reimplement — they implement the core protocol translation layer:

| # | Class | Lines | Purpose |
|---|-------|-------|---------|
| 71 | `_CodexCompletionsAdapter` | 367–527 | Codex Responses API → chat.completions adapter (sync). Stream collection, output backfill, tool call extraction, usage mapping. |
| 72 | `_CodexChatShim` | 530–534 | Shim layer: `.chat.completions.create()`. |
| 73 | `CodexAuxiliaryClient` | 537–553 | Public sync client wrapper with `.chat.completions`, `.api_key`, `.base_url`, `.close()`. |
| 74 | `_AsyncCodexCompletionsAdapter` | 555–567 | Async version via `asyncio.to_thread()`. |
| 75 | `_AsyncCodexChatShim` | 570–572 | Async shim. |
| 76 | `AsyncCodexAuxiliaryClient` | 575–583 | Public async client wrapper. |
| 77 | `_AnthropicCompletionsAdapter` | 586–667 | Anthropic Messages API → chat.completions adapter. Handles tool_choice normalization, temperature gating for Opus 4.7+, transport-based response normalization, usage extraction. |
| 78 | `_AnthropicChatShim` | 670–672 | Shim layer. |
| 79 | `AnthropicAuxiliaryClient` | 675–688 | Public sync client with `.close()`. |
| 80 | `_AsyncAnthropicCompletionsAdapter` | 691–697 | Async version via `asyncio.to_thread()`. |
| 81 | `_AsyncAnthropicChatShim` | 700–702 | Async shim. |
| 82 | `AsyncAnthropicAuxiliaryClient` | 705–711 | Public async client. |

### 2.17 Module-Level Constants (MUST carry forward)

| Constant | Lines | Purpose |
|----------|-------|---------|
| `OMIT_TEMPERATURE` (sentinel) | 123 | Signal to strip temperature kwarg entirely. |
| `_PROVIDER_ALIASES` | 70–95 | Provider name normalization map. |
| `_API_KEY_PROVIDER_AUX_MODELS` | 152–166 | Default auxiliary models per direct-API-key provider. |
| `_PROVIDER_VISION_MODELS` | 172–175 | Per-provider vision model overrides. |
| `_OR_HEADERS` | 178–182 | OpenRouter attribution headers. |
| `_AI_GATEWAY_HEADERS` | 188–192 | Vercel AI Gateway attribution headers. |
| `NOUS_EXTRA_BODY` | 197 | Nous Portal product tags. |
| `auxiliary_is_nous` (global) | 200 | Set `True` when Nous is the resolved backend. |
| `_OPENROUTER_MODEL` / `_NOUS_MODEL` / etc. | 203–215 | Default model slugs. |
| `_AUTO_PROVIDER_LABELS` | 1270–1276 | Human-readable labels for chain providers. |
| `_VISION_AUTO_PROVIDER_ORDER` | 2153–2156 | Vision auto-detection priority. |
| `_ANTHROPIC_COMPAT_PROVIDERS` | 2737 | Providers using Anthropic-compat endpoints. |
| `_DEFAULT_AUX_TIMEOUT` | 2691 | Default 30s timeout. |
| `_CLIENT_CACHE_MAX_SIZE` | 2355 | Cache eviction threshold (64). |

---

## 3. Dependency Graph

### 3.1 Call Graph (simplified)

```
call_llm() / async_call_llm()
  ├── _resolve_task_provider_model()
  │     └── _get_auxiliary_task_config()
  │           └── hermes_cli.config.load_config()
  ├── resolve_vision_provider_client()  [if task=="vision"]
  │     ├── _resolve_task_provider_model()
  │     ├── _normalize_vision_provider()
  │     │     └── _normalize_aux_provider()
  │     │           └── _read_main_provider()
  │     ├── resolve_provider_client()
  │     ├── _resolve_strict_vision_backend()
  │     │     ├── _try_openrouter()
  │     │     ├── _try_nous()
  │     │     ├── _try_codex()
  │     │     ├── _try_anthropic()
  │     │     └── _try_custom_endpoint()
  │     └── _get_cached_client()
  ├── _get_cached_client()  [non-vision]
  │     ├── _client_cache_key()
  │     └── resolve_provider_client()
  │           ├── _resolve_auto()
  │           │     ├── _read_main_provider()
  │           │     ├── _read_main_model()
  │           │     ├── resolve_provider_client()  [recursive — Step 1]
  │           │     └── _get_provider_chain()
  │           │           ├── _try_openrouter()
  │           │           ├── _try_nous()
  │           │           ├── _try_custom_endpoint()
  │           │           ├── _try_codex()
  │           │           └── _resolve_api_key_provider()
  │           ├── _try_openrouter() → pool + env
  │           ├── _try_nous()
  │           │     ├── _read_nous_auth()
  │           │     └── _resolve_nous_runtime_api()
  │           ├── _try_custom_endpoint()
  │           │     └── _resolve_custom_runtime()
  │           ├── _try_codex()
  │           │     └── _read_codex_access_token()
  │           ├── _try_anthropic()
  │           │     └── agent.anthropic_adapter.*
  │           ├── _resolve_api_key_provider()
  │           │     └── hermes_cli.auth.PROVIDER_REGISTRY
  │           └── [auth_type-specific branches]
  ├── _build_call_kwargs()
  │     ├── _fixed_temperature_for_model()
  │     ├── _is_kimi_model()
  │     └── agent.anthropic_adapter._forbids_sampling_params()
  ├── _convert_openai_images_to_anthropic()  [if MiniMax compat]
  ├── _validate_llm_response()
  ├── [error recovery — temperature retry]
  │     └── _is_unsupported_temperature_error()
  │           └── _is_unsupported_parameter_error()
  ├── [error recovery — max_tokens retry]
  ├── [error recovery — Nous 401 refresh]
  │     └── _refresh_nous_auxiliary_client()
  ├── [error recovery — auth refresh]
  │     └── _refresh_provider_credentials()
  │           ├── _evict_cached_clients()
  │           └── [provider-specific OAuth refresh]
  └── [error recovery — payment/connection fallback]
        ├── _is_payment_error()
        ├── _is_connection_error()
        └── _try_payment_fallback()
              └── _get_provider_chain()
```

### 3.2 Client Construction Tree

```
resolve_provider_client(provider, model)
  ├── _normalize_aux_provider(provider)
  ├── [provider == "auto"]
  │     └── _resolve_auto()
  ├── [provider == "openrouter"]
  │     └── _try_openrouter()
  ├── [provider == "nous"]
  │     └── _try_nous()
  ├── [provider == "openai-codex"]
  │     └── _try_codex()
  │           └── _read_codex_access_token()
  ├── [provider == "custom"]
  │     ├── _try_custom_endpoint()
  │     ├── _try_codex()
  │     └── _resolve_api_key_provider()
  ├── [named custom provider]
  │     └── hermes_cli.runtime_provider._get_named_custom_provider()
  ├── [API-key provider via PROVIDER_REGISTRY]
  │     ├── hermes_cli.auth.resolve_api_key_provider_credentials()
  │     ├── _try_anthropic()  [if anthropic]
  │     ├── GeminiNativeClient  [if gemini + native URL]
  │     └── OpenAI(...)
  ├── [external_process — copilot-acp]
  │     └── copilot_acp_client.CopilotACPClient
  ├── [aws_sdk — bedrock]
  │     └── agent.bedrock_adapter.* + build_anthropic_bedrock_client
  └── [oauth_device_code / oauth_external]
        └── resolve_provider_client()  [recursive]
```

### 3.3 Adapter Wrapping

```
OpenAI client  ──►  _wrap_if_needed()  ──►  CodexAuxiliaryClient  [if Responses API]
                                        ──►  AnthropicAuxiliaryClient  [if anthropic_messages api_mode]
                                        ──►  GeminiNativeClient  [if gemini native]

_to_async_client(sync_client)
  ├── isinstance(CodexAuxiliaryClient)   → AsyncCodexAuxiliaryClient
  ├── isinstance(AnthropicAuxiliaryClient) → AsyncAnthropicAuxiliaryClient
  ├── isinstance(GeminiNativeClient)      → AsyncGeminiNativeClient
  ├── isinstance(CopilotACPClient)        → return as-is
  └── default                             → AsyncOpenAI with preserved headers
```

---

## 4. Architecture Notes for Rust Reimplementation

### 4.1 What Maps Naturally to Rust

- **Provider trait**: Each `_try_*` function becomes a struct implementing a `ProviderResolver` trait with `fn try_resolve(&self) -> Option<(Client, String)>`.
- **Resolution chain**: `_get_provider_chain()` becomes a `Vec<Box<dyn ProviderResolver>>` iterated in priority order.
- **Client enum**: `AuxiliaryClient::OpenAI(OpenAiClient) | Codex(CodexClient) | Anthropic(AnthropicClient) | Gemini(GeminiClient)` with a `ChatCompletions` trait.
- **Error classification**: `_is_payment_error` etc. become methods on a custom error enum (`AuxError::PaymentRequired`, `AuxError::ConnectionFailed`, `AuxError::AuthFailed`).
- **Client cache**: `HashMap<CacheKey, CachedClient>` behind `RwLock` — maps directly from `_client_cache` + `_client_cache_lock`.
- **Async**: Tokio tasks replace `asyncio.to_thread()`. No event-loop binding complexity.
- **Module-level globals**: `OnceLock` or `LazyLock` for constants that need runtime initialization.

### 4.2 What Is Fundamentally Different in Rust

- **No monkey-patching**: `neuter_async_httpx_del`, `_force_close_async_httpx` are unnecessary — Rust's ownership model handles cleanup via `Drop`.
- **No `asyncio` event-loop binding**: The async client cache validation in `_get_cached_client` (lines 2590–2606) is pure Python complexity. Tokio's task model eliminates stale-loop concerns.
- **Error types** are richer: Use `thiserror` for `ProviderError` enum matching instead of string-based error detection (`_is_payment_error` scans exception messages).
- **Configuration**: `config.yaml` parsing becomes serde deserialization into strongly-typed structs instead of dict access with `isinstance` guards.
- **Typing**: `Any`/`Optional`/`Dict[str, Any]` become concrete types — no more dynamic duck-typing of response objects.

### 4.3 Recommended Split Strategy

The file should decompose into at least these Rust modules:

| Module | Contains | From Python |
|--------|----------|-------------|
| `auxiliary::providers::openrouter` | `_try_openrouter` | Lines 919–948 |
| `auxiliary::providers::nous` | `_try_nous`, `_read_nous_auth`, `_resolve_nous_runtime_api`, `_refresh_nous_auxiliary_client` | Lines 714–786, 951–1014, 2386–2425 |
| `auxiliary::providers::codex` | `_try_codex`, `_read_codex_access_token`, `_codex_cloudflare_headers`, adapter classes | Lines 218–254, 367–583, 789–829, 1197–1219 |
| `auxiliary::providers::anthropic` | `_try_anthropic`, adapter classes | Lines 586–711, 1222–1267 |
| `auxiliary::providers::custom` | `_try_custom_endpoint`, `_resolve_custom_runtime` | Lines 1057–1194 |
| `auxiliary::providers::api_key` | `_resolve_api_key_provider`, `_get_named_custom_provider` wrapper | Lines 832–912, 1868–2056 |
| `auxiliary::providers::bedrock` | Bedrock-specific branch of `resolve_provider_client` | Lines 2058–2089 |
| `auxiliary::resolution` | `_resolve_auto`, `resolve_provider_client`, `_get_provider_chain` | Lines 1296–2104 |
| `auxiliary::cache` | `_client_cache_key`, `_store_cached_client`, `_get_cached_client`, eviction | Lines 2353–2635 |
| `auxiliary::call` | `call_llm`, `async_call_llm`, `_build_call_kwargs`, `_validate_llm_response` | Lines 2691–3166, 3225–3445 |
| `auxiliary::error` | All `_is_*_error` functions, `_try_payment_fallback`, `_refresh_provider_credentials` | Lines 1311–1515 |
| `auxiliary::config` | `_get_auxiliary_task_config`, `_resolve_task_provider_model`, `_read_main_*` | Lines 1017–1104, 2638–2728 |
| `auxiliary::adapters` | `_convert_content_for_responses`, `_convert_openai_images_to_anthropic`, `_is_anthropic_compat_endpoint` | Lines 319–364, 2740–2796 |
| `auxiliary::public` | `get_text_auxiliary_client`, `get_async_text_auxiliary_client`, vision APIs, `extract_content_or_reasoning` | Lines 2109–2332, 3169–3222 |
| `auxiliary::models` | `_is_kimi_model`, `_fixed_temperature_for_model`, `_compat_model`, `_normalize_resolved_model` | Lines 126–149, 1658–1667, 2534–2541 |

### 4.4 Functions That Need the Most Rethinking

These Python patterns don't translate directly:

1. **`resolve_provider_client`** (435 lines): Use a `match` on `auth_type` enum with per-variant handler functions. The Python if-elif chain becomes a dispatch table.

2. **`call_llm` / `async_call_llm`** (duplicated 274+220 lines): The Rust version should have a single implementation generic over async runtime. Error recovery should be a chain of `ErrorRecoveryStrategy` trait objects.

3. **Error detection via string scanning**: Python uses `"credits" in err_lower` — Rust should use proper error variant matching (`AuxError::PaymentRequired { provider }`) rather than substring search on formatted error messages.

4. **`_client_cache` global + lock**: Python uses module-level dict + `threading.Lock()`. Rust should use `Arc<RwLock<LruCache>>` or a proper concurrent cache crate.

---

## 5. Summary Statistics

| Category | Count |
|----------|-------|
| **MUST reimplement** | 59 functions/classes |
| **CAN stay in Python** | 8 functions |
| **NEEDS SPLIT** | 6 functions |
| **Module-level constants** | 16 |
| **Adapter classes** | 12 |
| **Recommended Rust modules** | 13 |
| **External Python dependencies** | 17 modules |
| **Total lines** | 3,445 |

---

## 6. Verification

```bash
test -f docs/auxiliary_client_decomposition.md && echo "OK"
```
