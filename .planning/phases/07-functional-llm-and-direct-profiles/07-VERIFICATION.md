---
phase: 07-functional-llm-and-direct-profiles
verified: 2026-05-16T18:30:00Z
status: passed
score: 7/7 must-haves verified
overrides_applied: 0
---

# Phase 07: Functional LLM and Direct Profiles — Verification Report

**Phase Goal:** Functional LLM and direct profiles — replace stubs with real provider paths, wire the LLM facade into lattice-api, make profile consumption direct, and add pure TOML templates.
**Verified:** 2026-05-16T18:30:00Z
**Status:** passed
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths

| # | Requirement | Truth | Status | Evidence |
|---|-------------|-------|--------|----------|
| 1 | LLM-01 | `lattice-llm` exposes `chat(messages, tools, options)` for non-streaming model calls | VERIFIED | `stub.rs` has `pub async fn chat(request: ChatRequest) -> Result<ChatResponse, LlmError>`, resolves model via ModelRouter, dispatches to real client dispatch_stream, collects stream events into ChatResponse. `lib.rs` exports `chat`. `turn.rs` calls `lattice_llm::chat(chat_request)`. |
| 2 | LLM-02 | `lattice-llm` exposes `stream(messages, tools, options)` for token/tool/usage event streams | VERIFIED | `stub.rs` has `pub async fn stream(request: ChatRequest) -> Result<LlmStream, LlmError>`, dispatches to real client. `lib.rs` exports `stream`. `turn.rs` calls `lattice_llm::stream(chat_request)`. `LlmEvent` enum includes Token, Reasoning, ToolCallStart, ToolCallDelta, ToolCallEnd, Done { finish_reason, usage }, Error. |
| 3 | LLM-03 | Model aliases resolve to provider, provider model name, optional base URL, and API key environment variable | VERIFIED | `router.rs` `ModelRouter::resolve()` returns `ResolvedModel` with `alias`, `provider` (ProviderKind), `provider_model`, `base_url`, `api_key_env`, `credential_status`. Fixed alias table covers gpt-4o, gpt-4o-mini, sonnet, haiku, gemini, openai-compatible. Tests assert all fields populated correctly. |
| 4 | LLM-04 | Supported provider classes are OpenAI, Anthropic, Gemini, and OpenAI-compatible services | VERIFIED | `ProviderKind` enum: `OpenAi`, `OpenAiCompatible`, `Anthropic`, `Gemini`. Three concrete provider modules (`openai.rs` 545 lines, `anthropic.rs` 631 lines, `gemini.rs` 593 lines) with request builders, SSE parsers, and auth headers. `client.rs` dispatches by `match ProviderKind`. |
| 5 | LLM-05 | The router does not contain broad catalog discovery, automatic model selection, or hidden fallback policy | VERIFIED | No catalog JSON. Unknown aliases return `LlmError::UnknownModel`. No prefix parsing. No fallback. Grep for `lattice_core::`, `TransportDispatcher`, `catalog/data.json`, `ProviderRegistry`, `automatic fallback` in `lattice-llm/src` — matches only in descriptive comments stating they are NOT present. Cargo metadata: `lattice-llm` has zero forbidden crate dependencies. |
| 6 | PROF-01 | `agent.toml` parses directly into `AgentProfile` without a separate lowering pipeline | VERIFIED | Main path (`turn.rs`) uses `LoadedAgentProfile` directly: `lp.profile.agent.model.clone()` and `lp.system_prompt.clone()`. No `lower_profile_to_spec()` or `GenericAgentSpec` calls in `lattice-api/src` or `run_v11.rs`. `load_agent_profile()` parses TOML directly to `AgentProfile` with strict allowlist validation. The `lower_profile_to_spec()` function exists for legacy compatibility but is NOT consumed by the v1.1 main path. |
| 7 | PROF-02 | Default profile templates exist for reader, editor, and dev modes with explicit tool/safety intent | VERIFIED | `defaults.rs` has `default_reader_template()`, `default_editor_template()`, `default_dev_template()` (plus `default_agent_template()`). Each produces parseable TOML with `[tools]`, `[security]`, `sandbox_mode`, `max_turns`, `reasoning_effort`. Reader: workspace+filesystem, no shell/editing/web. Editor: workspace+filesystem+editing+shell. Dev: all six bundles, strict sandbox. All exported from `lib.rs`. Tests parse each with `toml::from_str::<AgentProfile>()`. |
| - | PROF-03 | Profile drift fingerprints, source provenance warnings, and orchestrator-style profile indirection are removed from the v1.1 main path | VERIFIED | Grep for `drift`, `provenance`, `fingerprint`, `orchestrat.*indirection` in `lattice-profile/src` — zero matches. No hidden built-in profile loading. No automatic template fallback. Templates are explicit pure TOML strings. |

**Score:** 7/7 requirements verified

### Required Artifacts

| Artifact | Expected | Status | Lines | Details |
|----------|----------|--------|-------|---------|
| `crates/lattice-llm/src/router.rs` | Fixed alias table, ResolvedModel, ModelRouter::resolve | VERIFIED | 338 | Contains ProviderKind, CredentialStatus, ResolvedModel, ModelRouter::resolve. No catalog, prefix, fallback. |
| `crates/lattice-llm/src/security.rs` | Base URL validation, SSRF floor | VERIFIED | 194 | `validate_base_url()` rejects bad schemes, userinfo, plain HTTP remote, private/reserved IPs. Allows http://localhost. |
| `crates/lattice-llm/src/sse.rs` | Bounded SSE framing | VERIFIED | 189 | MAX_SSE_EVENTS=100K, MAX_SSE_DATA_SIZE=256KB, MAX_SSE_BUFFER_SIZE=2MB. `parse_raw_sse()` with limit enforcement. |
| `crates/lattice-llm/src/types.rs` | Rich LlmEvent vocabulary | VERIFIED | 392 | Reasoning, ToolCallStart, ToolCallDelta, ToolCallEnd, Done { finish_reason, usage }, Error. TokenUsage struct. ChatRequest, ChatResponse, Message, ModelTarget. |
| `crates/lattice-llm/src/providers/openai.rs` | OpenAI-compatible request/response/SSE mapping | VERIFIED | 545 | chat/completions JSON, Authorization: Bearer, tool_calls, SSE parser with state tracking. |
| `crates/lattice-llm/src/providers/anthropic.rs` | Anthropic request/response/SSE mapping | VERIFIED | 631 | v1/messages, x-api-key, anthropic-version, tool_use, input_schema, SSE parser. |
| `crates/lattice-llm/src/providers/gemini.rs` | Gemini request/response/SSE mapping | VERIFIED | 593 | streamGenerateContent, x-goog-api-key, systemInstruction, functionCall, SSE parser. |
| `crates/lattice-llm/src/client.rs` | Concrete provider dispatch | VERIFIED | 468 | match ProviderKind dispatch, validate_and_read_key, build_auth_headers, build_request_body, build_endpoint_url, dispatch_stream. No registry or plugin. |
| `crates/lattice-llm/src/stub.rs` | Public chat/stream convenience + StubLlm for tests | VERIFIED | 367 | `chat()` collects stream via real client. `stream()` dispatches to real client. StubLlm retained for tests. |
| `crates/lattice-api/src/turn.rs` | LLM facade integration | VERIFIED | 604 | Uses `lattice_llm::chat` and `lattice_llm::stream` (production). LoadedAgentProfile consumed directly. commit_id: None. TurnEvent mirrors LlmEvent. Tests use StubLlm (credential-free CI). |
| `crates/lattice-profile/src/defaults.rs` | Reader/editor/dev pure TOML templates | VERIFIED | 439 | Four template functions. Each parseable as AgentProfile. Each has [tools], [security], sandbox_mode. No enum API, no hidden fallback. |
| `crates/lattice-profile/src/schema.rs` | Strict v1 AgentProfile schema | VERIFIED | 511 | deny_unknown_fields on all sections. Rejects forbidden sections (capabilities, handoff, orchestrator, etc.). |
| `crates/lattice-profile/src/load.rs` | Direct TOML-to-AgentProfile loading | VERIFIED | 501 | load_agent_profile() with forbidden section pre-scan, schema_version check, prompt resolution. |
| `crates/lattice-cli/src/commands/run_v11.rs` | v1.1 skeleton CLI handler | VERIFIED | 130 | Calls lattice_api::run_turn. loaded_profile: None. No lattice_runtime/lattice_core references. |

### Key Link Verification

| From | To | Via | Status | Evidence |
|------|----|-----|--------|----------|
| `ModelRouter::resolve` | `client.rs` provider dispatch | `ResolvedModel.provider` | WIRED | `client.rs` `match resolved.provider` dispatches to per-provider functions. |
| Provider SSE parsers | chat collector | LlmEvent stream | WIRED | `dispatch_stream()` parses SSE into LlmEvent; `chat()` collects the stream. |
| `run_v11` optional profile path | `lattice-profile::load_agent_profile` | LoadedAgentProfile | WIRED | `run_v11.rs` accepts `LoadedAgentProfile` in RunTurnRequest. `turn.rs` extracts model and system_prompt directly. |
| `LoadedAgentProfile` | `RunTurnRequest` / `ChatRequest` | profile.agent.model, system_prompt | WIRED | `turn.rs` lines 164-192: extracts `lp.profile.agent.model.clone()` and `lp.system_prompt.clone()`, builds ChatRequest. |

### Data-Flow Trace (Level 4)

| Artifact | Data Variable | Source | Produces Real Data | Status |
|----------|--------------|--------|--------------------|--------|
| `turn.rs` `run_turn()` | `response.content` | `lattice_llm::chat()` → `dispatch_stream()` → real HTTP provider call | Yes (requires credentials) | FLOWING |
| `turn.rs` `stream_turn()` | `llm_stream` | `lattice_llm::stream()` → `dispatch_stream()` → real HTTP provider call | Yes (requires credentials) | FLOWING |
| `turn.rs` profile consumption | `lp.profile.agent.model` | `LoadedAgentProfile` from `load_agent_profile()` | Yes | FLOWING |
| `defaults.rs` templates | TOML string | Inline template constants | Yes (parseable TOML) | FLOWING |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| lattice-llm tests pass | `cargo test -p lattice-llm` | 125 passed, 0 failed | PASS |
| lattice-api tests pass | `cargo test -p lattice-api` | 20 passed, 0 failed | PASS |
| lattice-profile tests pass | `cargo test -p lattice-profile` | 65 passed, 0 failed | PASS |
| Workspace compiles | `cargo check --workspace` | 0 errors | PASS |
| No forbidden deps in lattice-llm | `cargo metadata` analysis | zero forbidden crate deps | PASS |
| No old arch in lattice-llm/src | grep for lattice_core:: etc. | only in descriptive comments | PASS |
| No lowering in v1.1 API/CLI path | grep for lower_profile_to_spec/GenericAgentSpec | zero matches in api/src, run_v11.rs | PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| LLM-01 | 07-01, 07-02, 07-03, 07-04 | `lattice-llm` exposes `chat(messages, tools, options)` for non-streaming model calls | SATISFIED | `stub.rs` `pub async fn chat()`, wired into `turn.rs`, collects real client stream |
| LLM-02 | 07-01, 07-02, 07-03, 07-04 | `lattice-llm` exposes `stream(messages, tools, options)` for token/tool/usage event streams | SATISFIED | `stub.rs` `pub async fn stream()`, dispatches to real client, LlmEvent vocabulary |
| LLM-03 | 07-01, 07-04 | Model aliases resolve to provider, provider model name, optional base URL, and API key environment variable | SATISFIED | `router.rs` ResolvedModel has all fields, tests assert each |
| LLM-04 | 07-02, 07-04 | Supported provider classes are OpenAI, Anthropic, Gemini, and OpenAI-compatible services | SATISFIED | ProviderKind enum, three provider modules all substantial |
| LLM-05 | 07-01, 07-04 | Router does not contain broad catalog discovery, automatic model selection, or hidden fallback policy | SATISFIED | No catalog, no prefix, no fallback — boundary greps clean |
| PROF-01 | 07-03, 07-04 | `agent.toml` parses directly into `AgentProfile` without a separate lowering pipeline | SATISFIED | Main path uses `LoadedAgentProfile` directly; `load_agent_profile()` does direct TOML→AgentProfile |
| PROF-02 | 07-03, 07-04 | Default profile templates exist for reader, editor, and dev modes with explicit tool/safety intent | SATISFIED | Four templates in `defaults.rs`, all parseable, all with explicit tool/safety |
| PROF-03 | 07-03, 07-04 | Profile drift fingerprints, source provenance warnings, and orchestrator-style profile indirection are removed from the v1.1 main path | SATISFIED | Zero matches for drift/provenance/fingerprint/orchestrator indirection in profile src |

### Anti-Patterns Found

No anti-patterns detected in Phase 7 files:
- Zero TBD/FIXME/XXX markers
- Zero TODO/HACK/PLACEHOLDER markers
- Zero empty implementations (return null/{}/[])
- Zero console.log-only handlers
- Zero hardcoded empty data in production paths

### Human Verification Required

#### 1. Live Provider Integration

**Test:** Run `cargo run -p lattice-cli -- run "hello" -m sonnet` with valid `ANTHROPIC_API_KEY` set.
**Expected:** The CLI should produce a real assistant response (not "stub response").
**Why human:** Requires live API credentials and network access; cannot verify programmatically.

#### 2. Missing Credential Error Clarity

**Test:** Unset all API key env vars and run `cargo run -p lattice-cli -- run "hello"`.
**Expected:** A clear error message naming the missing environment variable (e.g., "environment variable 'ANTHROPIC_API_KEY' is not set") without leaking any key value.
**Why human:** Requires orchestrating environment variable state; error message formatting is a UX concern.

#### 3. Provider Switching

**Test:** With appropriate credentials set, run through each provider alias (`-m sonnet`, `-m gpt-4o`, `-m gemini`).
**Expected:** Each produces a response matching the respective provider's model name in the response.
**Why human:** Requires multiple sets of live credentials and provider accounts.

### Minor Compliance Notes

1. **Deletion-bound comment missing from `lowering.rs`**: Plan 07-03 Task 3 acceptance criteria requires "`spec.rs` or `lowering.rs` contains a deletion-bound comment mentioning Phase 10 or Phase 11." The `spec.rs` file has a weak reference ("crosses into later runtime phases") but `lowering.rs` has no deletion/Phase 10/Phase 11 marker. This is a documentation gap only — the behavioral outcome (PROF-01: main path does not use lowering) is fully achieved. The `lower_profile_to_spec` function is exported for legacy compatibility but the v1.1 API/CLI path never calls it.

2. **StubLlm in lattice-api test code**: `turn.rs` test modules (`#[cfg(test)]`) import `lattice_llm::StubLlm` for credential-free CI testing. Production code (`run_turn`, `stream_turn`) uses `lattice_llm::chat` and `lattice_llm::stream`. This is an intentional design choice documented in 07-03-SUMMARY.md.

---

_Verified: 2026-05-16T18:30:00Z_
_Verifier: Claude (gsd-verifier)_
