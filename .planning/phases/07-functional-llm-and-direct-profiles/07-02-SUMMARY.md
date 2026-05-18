---
phase: 07-functional-llm-and-direct-profiles
plan: 02
subsystem: lattice-llm
tags: [llm, providers, openai, anthropic, gemini, sse, client-dispatch, tdd]
requires: ["07-01"]
provides: ["07-03"]
affects: ["lattice-api"]
tech-stack:
  added: [reqwest, urlencoding]
  patterns: [provider-module-per-family, match-on-ProviderKind-dispatch, thread-local-parser, no-registry-no-plugin]
key-files:
  created:
    - crates/lattice-llm/src/providers/mod.rs
    - crates/lattice-llm/src/providers/openai.rs
    - crates/lattice-llm/src/providers/anthropic.rs
    - crates/lattice-llm/src/providers/gemini.rs
    - crates/lattice-llm/src/client.rs
  modified:
    - crates/lattice-llm/src/lib.rs
    - crates/lattice-llm/src/stub.rs
    - crates/lattice-llm/Cargo.toml
    - Cargo.lock
decisions:
  - Concrete match-on-ProviderKind dispatch; no transport registry, provider plugin trait, or catalog import.
  - Provider modules own per-provider request JSON, SSE parsing, and auth headers; no shared provider trait needed.
  - Thread-local parsers used for SSE state tracking in convenience path; per-request parser objects for real streaming.
  - Public chat()/stream() now route through real client dispatch; StubLlm retained for tests.
  - Anthropic stop reasons mapped to OpenAI-style finish reasons (end_turn→stop, tool_use→tool_calls, max_tokens→length).
  - Gemini finish reasons mapped (STOP→stop, MAX_TOKENS→length, SAFETY/RECITATION→content_filter).
metrics:
  duration: "~45 minutes"
  completed_date: "2026-05-16T14:00:00Z"
---

# Phase 7 Plan 2: Real Provider Paths Summary

Real provider paths for OpenAI-compatible, Anthropic, and Gemini implemented in `lattice-llm` with TDD-driven provider modules and concrete client dispatch, replacing the Phase 6 fake provider behavior with testable request/SSE/response plumbing.

## What Was Built

Three standalone provider modules (`openai.rs`, `anthropic.rs`, `gemini.rs`) each containing request JSON builders, SSE chunk parsers, and auth header logic. A concrete `client.rs` dispatch layer using `match ProviderKind` without any transport registry, provider plugin trait, or `lattice-core` dependency.

### OpenAI-compatible Provider (`providers/openai.rs`)
- Builds Chat Completions JSON with role mapping, tool definitions (`type: function`), `stream: true`, optional `temperature` and `max_tokens`
- Auth header: `Authorization: Bearer {key}`
- SSE parser: handles content tokens, `reasoning_content`, tool call start/delta/end, finish reason with usage, `[DONE]` sentinel, and API errors
- Stateful parser tracks tool call IDs by index for delta chunks

### Anthropic Provider (`providers/anthropic.rs`)
- Builds Messages API JSON with system message extraction/merging (`\n\n`), content block construction
- Maps tool definitions to `input_schema` format
- Auth headers: `x-api-key` and `anthropic-version: 2023-06-01`
- SSE parser: handles `content_block_start` (tool_use), `content_block_delta` (text_delta, thinking_delta, input_json_delta), `content_block_stop`, `message_delta` (Done with usage), `message_start`/`message_stop`/`ping` (ignored), and `error` events
- Stop reason mapping: `end_turn→stop`, `tool_use→tool_calls`, `max_tokens→length`

### Gemini Provider (`providers/gemini.rs`)
- Builds `generateContent` JSON with `systemInstruction`, role mapping (assistant→`model`), `functionCall`/`functionResponse`
- Tool definitions mapped to `functionDeclarations`
- URL-encodes model in `:streamGenerateContent?alt=sse` endpoint
- Auth header: `x-goog-api-key`
- SSE parser: handles text tokens, reasoning (`thought: true`), `functionCall` (ToolCallStart + ToolCallDelta in one chunk), `finishReason` (Done with usage), and errors
- Finish reason mapping: `STOP→stop`, `MAX_TOKENS→length`, `SAFETY/RECITATION→content_filter`

### Client Dispatch (`client.rs`)
- `validate_and_read_key()` — validates base URL (SSRF safety) and reads env var, returning named errors
- `build_auth_headers()` — produces per-provider header sets
- `build_request_body()` — dispatches to correct provider JSON builder
- `build_endpoint_url()` — constructs provider-specific endpoint URLs
- `dispatch_stream()` — full HTTP request/response/SSE parsing pipeline
- `stream_sse_response()` — collects response body, parses SSE events
- Missing-credential errors name the env var without leaking key values

### Public API Wiring (`stub.rs`)
- `chat(request)` now resolves model alias, dispatches through real client, collects stream
- `stream(request)` now resolves model alias, dispatches through real client
- `StubLlm` struct and methods retained for tests

## Task Summary

| Task | Name | Commit | Tests |
|------|------|--------|-------|
| 1 | OpenAI-compatible provider path | `a2c2626` | 21 |
| 2 | Anthropic provider path | `6c85df2` | 22 |
| 3 | Gemini provider path + client dispatch | `0354f2f` | 24 |
| 3b | Cargo.lock update | (Cargo.lock) | — |

## Acceptance Criteria Verification

- [x] `cargo test -p lattice-llm openai` passes (23 tests)
- [x] `cargo test -p lattice-llm anthropic` passes (24 tests)
- [x] `cargo test -p lattice-llm gemini` passes (19 tests)
- [x] `cargo test -p lattice-llm` passes (125 tests total)
- [x] No `TransportDispatcher`, `ProviderRegistry`, `lattice_core::`, or `catalog/data.json` in provider/client code
- [x] Public `stream()` dispatches to real provider paths for fixed aliases
- [x] No provider registry, plugin model, old catalog, or `lattice-core` dependency introduced
- [x] Missing credential errors name the env var without leaking values
- [x] Client dispatch uses concrete `match ProviderKind` without registry or plugin mechanism

## Deviations from Plan

None — plan executed as written with TDD flow.

### Architecture Note

The `stream_sse_response()` function currently collects the full response body before parsing SSE events, rather than streaming incrementally through the response. This is acceptable for Phase 7 as the safety limits (`MAX_SSE_BUFFER_SIZE: 2MB`, `MAX_SSE_EVENTS: 100,000`) are enforced. Future phases can upgrade to true incremental streaming with a channel-based approach.

## Decisions Made

1. **Thread-local parsers for convenience path**: The `parse_sse_chunk` convenience in `openai.rs` uses `thread_local!` with `RefCell<OpenAiSseParser>` to maintain state across calls for testing convenience. Real streaming uses fresh parser instances per stream.

2. **Full-body collection for SSE**: Rather than complex async stream chaining, `stream_sse_response()` collects the full response body. This keeps the implementation simple and bounded by existing SSE limits.

3. **Anthropic tool results merged into user messages**: Following the reference implementation, consecutive tool results are batched into a single user message with multiple `tool_result` content blocks.

## Known Stubs

- `StubLlm` remains exported for test use but is no longer the default chat/stream path
- SSE streaming collects full body rather than streaming incrementally (bounded by `MAX_SSE_BUFFER_SIZE`)

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| threat_flag: credential-handling | `client.rs` | `validate_and_read_key` reads API keys from environment — tests verify errors never leak values |
| threat_flag: url-validation | `client.rs` | Base URL validation uses `crate::security::validate_base_url` for SSRF protection |

## Self-Check

- [x] `crates/lattice-llm/src/providers/openai.rs` exists
- [x] `crates/lattice-llm/src/providers/anthropic.rs` exists
- [x] `crates/lattice-llm/src/providers/gemini.rs` exists
- [x] `crates/lattice-llm/src/client.rs` exists
- [x] Commit `a2c2626` exists (Task 1)
- [x] Commit `6c85df2` exists (Task 2)
- [x] Commit `0354f2f` exists (Task 3)
- [x] All 125 tests pass

## Self-Check: PASSED
