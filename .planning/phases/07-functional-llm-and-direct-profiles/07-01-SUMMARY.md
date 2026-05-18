---
plan: "07-01"
phase: "07-functional-llm-and-direct-profiles"
status: complete
started: "2026-05-16T11:34:00Z"
completed: "2026-05-16T12:10:00Z"
tasks:
  - id: "1"
    name: "Add explicit model resolution types and fixed alias table"
    status: "complete"
  - id: "2"
    name: "Expand stream events and implement chat as stream collector"
    status: "complete"
  - id: "3"
    name: "Add base URL validation and bounded SSE framing"
    status: "complete"
key-files:
  created:
    - "crates/lattice-llm/src/router.rs"
    - "crates/lattice-llm/src/security.rs"
    - "crates/lattice-llm/src/sse.rs"
  modified:
    - "crates/lattice-llm/src/lib.rs"
    - "crates/lattice-llm/src/types.rs"
    - "crates/lattice-llm/src/stub.rs"
    - "crates/lattice-llm/Cargo.toml"
requirements_covered: ["LLM-01", "LLM-02", "LLM-03", "LLM-05"]
---

## Summary

Replaced the Phase 6 identity router and stub-only event contract with a real LLM core foundation.

### What was built

**ModelRouter with fixed alias table** (`router.rs`): Maps a small set of explicit aliases to provider metadata — ProviderKind (OpenAi, OpenAiCompatible, Anthropic, Gemini), provider_model, base_url, api_key_env, and CredentialStatus (Present/Missing). Unknown aliases return `LlmError::UnknownModel`. No catalog, no prefix parsing, no fallback.

**Rich LlmEvent vocabulary** (`types.rs`): Added Reasoning, ToolCallStart, ToolCallDelta, ToolCallEnd, and Done { finish_reason, usage }. TokenUsage struct for prompt/completion/total token counts.

**Stream-first chat** (`stub.rs`): `chat()` now calls `stream()` and collects tokens/reasoning/tool-call deltas into a single ChatResponse. `stream()` resolves the target model through ModelRouter and emits Done with finish_reason and usage.

**Base URL safety floor** (`security.rs`): `validate_base_url()` rejects bad schemes, userinfo, plain-HTTP remote hosts, and private/reserved IP targets. Allows `http://localhost:11434/v1` for local dev.

**Bounded SSE framing** (`sse.rs`): `parse_raw_sse()` with MAX_SSE_EVENTS (100K), MAX_SSE_DATA_SIZE (256KB), MAX_SSE_BUFFER_SIZE (2MB). Provider-agnostic RawSseEvent type ready for Plan 07-02 provider modules.

### Test results

```
cargo test -p lattice-llm: 55 passed; 0 failed
```

### Decisions

- Added `url = "2"` dependency for base URL validation.
- OpenAiCompatible base_url is read from `OPENAI_COMPATIBLE_BASE_URL` env var with localhost default.
- Removed unused `is_private_ip` helper — validation uses inline IP checks and DNS resolution.

### Known issues

- `cargo test` (workspace) fails because `lattice-api` references the old `LlmEvent::Done` unit variant. This is expected and will be fixed in Plan 07-03 when the LLM facade is wired into lattice-api.

## Self-Check: PASSED
