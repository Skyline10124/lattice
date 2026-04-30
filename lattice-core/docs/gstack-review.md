# Gstack Code Review — Artemis

**Date**: 2026-04-30
**Reviewer**: gstack /review
**Scope**: Full Rust workspace (lattice-core, lattice-agent, lattice-memory, lattice-token-pool, lattice-plugin, lattice-harness, lattice-cli, lattice-tui, lattice-python)
**Baseline**: `code-review-report.md` (2026-04-29, 10 items remaining)
**Methodology**: Huawei RCA 5-Why + Blue Army self-attack

---

## Executive Summary

The codebase has matured significantly since the last review. The monolith was decomposed into 9 crates with one-way deps, ~5K lines of dead code were removed, and HTTPS enforcement landed. However, **3 e2e tests are currently failing** and **clippy rejects the build** — the workspace does not pass CI. Beyond the red CI, this review found **14 new issues** (3 P0, 4 P1, 4 P2, 3 P3) plus carried-forward items from the prior review.

**CI status**: `cargo test` = 3 failures, `cargo clippy` = 1 error, `cargo fmt` = clean.

---

## CI Blockers (Must Fix Before Anything Else)

### C1. Clippy: identity_op in sandbox.rs

```
error: this operation has no effect
  --> lattice-agent/src/sandbox.rs:47:29
   |
47 |             max_write_size: 1 * 1024 * 1024,
```

**Fix**: `1 * 1024 * 1024` → `1024 * 1024` (or add `#[allow(clippy::identity_op)]`).

### C2. E2E test: `regress_missing_credential_errors` panics

```
sonnet should resolve via fallback, not fail
```

The test assumes `sonnet` resolves without `ANTHROPIC_API_KEY`, but the current router returns `ConfigError` because no provider has credentials. Test expectations are stale after the provider-priority refactor.

### C3. E2E test: `test_permissive_fallback_deepseek_model` panics

```
deepseek/model format should resolve via permissive fallback
```

`resolve_permissive` lowercases the provider part but doesn't find `deepseek` in provider_defaults — the defaults map may have changed.

### C4. E2E test: `fallback_errors_on_missing_provider_metadata`

```
called `Result::unwrap()` on an `Err` value: Config { message: "provider 'nous' requires one of: NOUS_API_KEY" }
```

Test expects `nous` (no credentials needed via OpenRouter) but `nous` actually requires `NOUS_API_KEY` in the current catalog.

---

## New Findings

### P0 — Critical

#### N1. `Agent.run()` truncates UTF-8 in memory auto-save

**File**: `lattice-agent/src/lib.rs:199-200`

```rust
let prompt_summary = if content.len() > 200 {
    format!("{}...", &content[..200])  // BUG: may slice mid-codepoint
} else {
    content.to_string()
};
```

**5-Why**:
1. Why does this panic? → Slicing `&str` at byte offset 200 may land inside a multi-byte UTF-8 character.
2. Why was it written this way? → Quick length check without UTF-8 boundary awareness.
3. Why wasn't it caught? → No test with non-ASCII content in `Agent::run()`.
4. Why no test? → `Agent::run()` requires a full async runtime, tests only cover `AgentState`.
5. **Root cause**: No UTF-8-safe truncation utility shared across the codebase. The same pattern was fixed in `state.rs:push_tool_result` but not here.

**Fix**: Use `content.char_indices().take_while(|(i, _)| *i < 200).last()` to find the boundary, same pattern as `truncate_body` in `errors.rs`.

---

#### N2. `DefaultToolExecutor` — command injection via `bash`/`run_command` tools

**File**: `lattice-agent/src/tools.rs:478-493`

The `bash` tool passes `cmd` directly to `sh -c` after only checking against `command_allowlist` via prefix matching:

```rust
if let Err(e) = self.sandbox.check_command(cmd) {
    return e;
}
let output = std::process::Command::new("sh").args(["-c", cmd]).output();
```

**5-Why**:
1. Why is this dangerous? → `cmd = "cargo test; rm -rf /"` passes the `starts_with("cargo test")` allowlist check.
2. Why prefix matching? → Simplicity; assumes the model won't craft malicious commands.
3. Why is that assumption wrong? → In an agent loop, model output IS the tool input. Prompt injection → malicious tool call.
4. Why isn't the sandbox stricter? → `command_allowlist` defaults to `["cargo test", "cargo clippy", ...]` but uses `starts_with`, which is too permissive.
5. **Root cause**: The sandbox treats the command as a string prefix match instead of tokenizing and validating the command structure.

**Fix**: Parse the command into `(program, args)`, check `program` against allowlist, reject if `;`, `|`, `&&`, `$()`, or backticks appear in the raw command string. Or: use `exec` directly instead of `sh -c`.

---

#### N3. `InMemoryMemory` uses a global `LazyLock<Mutex<>>` — all instances share state

**File**: `lattice-memory/src/lib.rs:104-155`

```rust
static GLOBAL_STORE: LazyLock<Mutex<GlobalStore>> =
    LazyLock::new(|| Mutex::new(GlobalStore { entries: vec![] }));
```

Every `InMemoryMemory::new()` instance silently shares the same global store. This means:
- Test A saves an entry → Test B reads it (test pollution)
- `clone_arc()` and `clone_box()` return **fresh empty** `InMemoryMemory` instances but they still write to the same global → confusing semantics
- No way to clear between tests (no `clear()` method exposed)

**5-Why**:
1. Why global? → `InMemoryMemory` struct has no fields, so state must live elsewhere.
2. Why no fields? → It was designed as a trivial stub before `async_trait` was understood.
3. Why not add a `Vec<MemoryEntry>` field? → `&mut self` doesn't work with `async_trait` + `Send + Sync` without interior mutability.
4. Why not `RwLock<Vec<>>`? → Wasn't considered during initial implementation.
5. **Root cause**: `InMemoryMemory` was designed as a quick default, not as a production-grade store. The global state is a leaky abstraction.

**Fix**: Replace with `RwLock<Vec<MemoryEntry>>` inside the struct. Remove `GLOBAL_STORE`.

---

### P1 — High

#### N4. `logging.rs`: `init_debug_logging` panics on file open failure

**File**: `lattice-core/src/logging.rs:56`

```rust
.expect("cannot open debug.log");
```

If the log path is unwritable (e.g., read-only filesystem, no disk space), this panics instead of returning an error. The `init_logging()` function is safe (no `expect`), but `init_debug_logging` is not.

**Fix**: Return `Result<(), std::io::Error>` or gracefully fall back to stdout-only logging.

---

#### N5. `logging.rs`: `init_logging` double-init is silently ignored

**File**: `lattice-core/src/logging.rs:16-37`

Calling `init_logging()` after a subscriber is already set (e.g., after `init_debug_logging()`) silently does nothing — `tracing_subscriber::fmt().init()` won't panic, it just won't register. This is documented as "safe to call multiple times" but the behavior is misleading: the second call's `verbose` flag has no effect.

**Fix**: Either check `tracing::subscriber::is_set()` and warn, or document that only the first call takes effect.

---

#### N6. `GeminiTransport` generates random `call_id` for non-streaming tool calls — breaks idempotency

**File**: `lattice-core/src/transport/gemini.rs:254,355`

```rust
id: Self::generate_call_id(),  // UUID-based
```

Every call to `denormalize_response` or `denormalize_stream_chunk` generates a new random ID for each tool call. This means:
- Two calls with the same response produce different `ToolCall` IDs
- Round-trip tests become non-deterministic
- The Agent's `tool_names` map can't correlate tool_call_id from request to response

**5-Why**:
1. Why random IDs? → Gemini API doesn't return tool call IDs in the response.
2. Why is this a problem? → The Agent uses `tool_call_id` to map tool results back to their calls. Random IDs mean the agent can't match results to the original calls.
3. Why wasn't this caught? → The Gemini transport is not end-to-end tested with tool calls.
4. Why not use index-based IDs? → Would be deterministic but still not match the request IDs.
5. **Root cause**: Gemini's API fundamentally doesn't support tool call IDs. The transport needs a coordination layer.

**Fix**: Use `format!("tc_{}", name)` or `format!("tc_{}", index)` as a deterministic pseudo-ID. Document the limitation.

---

#### N7. `chat()` in `lib.rs` doesn't use `ErrorClassifier` for HTTP errors from the SSE connection

**File**: `lattice-core/src/lib.rs:148-151`

When `eventsource()` fails (network error, 401, 429), the error is wrapped as a generic `ArtemisError::Network` without classification:

```rust
let event_source = req.eventsource().map_err(|e| ArtemisError::Network {
    message: format!("Failed to create event source: {}", e),
    status: None,
})?;
```

A 401 response during SSE setup should be `Authentication`, 429 should be `RateLimit`, etc. The `ErrorClassifier` exists but is never called on SSE connection errors.

**Why**: The SSE connection error happens before any response body is available, so `ErrorClassifier::classify()` can't be called (it needs `status_code` + `response_body`). But the HTTP status IS available from `reqwest`.

**Fix**: Check the HTTP status from the connection error and classify accordingly before wrapping.

---

### P2 — Medium

#### N8. `Agent::run()` doesn't retry on streaming errors

**File**: `lattice-agent/src/lib.rs:152-195`

`Agent::run()` calls `run_chat()` in a loop for tool execution, but if `run_chat()` returns a `LoopEvent::Error`, the loop continues to the next turn without any retry. The `chat_with_retry()` method only retries the initial connection — if the stream starts successfully but then errors mid-way, the partial response is silently discarded.

**Fix**: Add retry logic for mid-stream errors, or at minimum, propagate the error to the caller.

---

#### N9. `Agent::chat_with_retry()` clones the entire conversation on every attempt

**File**: `lattice-agent/src/lib.rs:354-358`

```rust
let resolved = self.state.resolved.clone();
let messages = self.state.messages.clone();
let tools = self.tools.clone();
```

Each retry attempt clones all messages and tool definitions. For long conversations with many tools, this is O(n) per attempt.

**Fix**: Pass references via `run_async` (which already handles `Send + 'static` by moving data into the closure). Or cache the clone outside the loop.

---

#### N10. `Transport::denormalize_stream_chunk` is deprecated but still implemented

**File**: `lattice-core/src/transport/mod.rs:127-130`

The method is marked `#[deprecated]` with a note to "Use SseParser via chat() instead," but `AnthropicTransport` still implements it with full logic (~75 lines). This dead code increases maintenance burden and confuses readers.

**Fix**: Remove the implementation from `AnthropicTransport` and return `vec![]` via the default method.

---

#### N11. `Memory` trait uses `async_trait` — unnecessary overhead for sync implementations

**File**: `lattice-memory/src/lib.rs:51`

```rust
#[async_trait]
pub trait Memory: Send + Sync {
```

`InMemoryMemory`'s methods are entirely synchronous (they just lock a Mutex). The `async_trait` proc macro adds a `Pin<Box<dyn Future>>` allocation per call. For the SQLite backend, async makes sense; for in-memory, it's pure overhead.

**Fix**: Consider making the trait use `-> impl Future<Output = ...>` or splitting into sync/async variants. Low priority but worth noting for the trait design.

---

### P3 — Low

#### N12. `resolve_permissive` doesn't lowercase `model_part`

**File**: `lattice-core/src/router.rs:438`

Already flagged in the previous review as L3. Still unfixed. `deepseek/DeepSeek-V4-Pro` would produce `api_model_id: "DeepSeek-V4-Pro"` instead of `"deepseek-v4-pro"`, which may cause API errors.

---

#### N13. HTTP timeout (30s default from `reqwest`) kills long streaming responses

**File**: `lattice-core/src/provider.rs:8-13`

The shared HTTP client has `connect_timeout(Duration::from_secs(10))` but no explicit read timeout. `reqwest`'s default timeout is effectively infinite, but the SSE layer may time out depending on the event source configuration. Already flagged as L7 in previous review.

---

#### N14. `ErrorClassifier` only handles `context_length_exceeded` for status 400

**File**: `lattice-core/src/errors.rs:164`

Anthropic returns `400` with `"type": "error"` and various error types (overloaded, etc.) that could be better classified. The current code only checks for `context_length_exceeded` in the 400 body.

---

## Carried Forward from Prior Review

| # | Level | Description | Status |
|---|-------|-------------|--------|
| M11 | M | Agent creates new tokio runtime per send | **Fixed** — `SHARED_RUNTIME` + `run_async()` with `spawn_blocking` fallback |
| M12 | M | chat() conversation clone per call | **Partially fixed** — still clones in `chat_with_retry` (N9 above) |
| L1 | L | Anthropic SSE parser doesn't rewrite finish reason | **Fixed** — `map_stop_reason()` now translates `end_turn→stop`, `tool_use→tool_calls`, `max_tokens→length` |
| L3 | L | resolve_permissive model_part not lowercased | **Unfixed** (N12) |
| L4 | L | Empty provider list panics | **Unfixed** — `sorted_providers[0]` at `router.rs:255` still panics if `entry.providers` is empty |
| L5 | L | Anthropic `stop_reason: "error"` unhandled | **Unfixed** — `map_stop_reason` returns `"stop"` for unknown reasons, which is misleading for `"error"` |
| L6 | L | `model_id.to_lowercase()` called twice | **Fixed** — `normalize_model_id` now called once in `resolve_alias` |
| L7 | L | 30s HTTP timeout kills long streams | **Unfixed** (N13) |
| L8 | L | Error response body no size limit | **Fixed** — `MAX_ERROR_BODY_LENGTH = 8192` + `truncate_body()` |

---

## Architecture Assessment

### Crate Decomposition — Good

The one-way dependency chain is clean:

```
lattice-python → lattice-agent → lattice-memory → lattice-token-pool → lattice-core
                                     ↘ lattice-plugin ↗
lattice-harness → lattice-agent + lattice-memory
lattice-cli → lattice-agent + lattice-core
lattice-tui → lattice-agent
```

No circular deps. Each crate has a clear responsibility.

### Concern: `lattice-core::chat()` is doing too much

The `chat()` function in `lib.rs` is ~155 lines of protocol-specific HTTP logic (URL construction, header injection, SSE creation). This should be in the `Transport` trait — each transport should know how to build and execute its own HTTP request. The current design forces `chat()` to know about `ApiProtocol` variants and `provider_specific` keys like `"auth_type"` and `"header:"`.

### Concern: `lattice-agent::tools` mixes tool definitions with tool execution

The `DefaultToolExecutor` is a 770-line monolith that defines AND executes 17 tools. Tool definitions should be separate from execution so callers can provide custom implementations for individual tools.

---

## Test Coverage Assessment

| Crate | Unit Tests | E2E Tests | Coverage Quality |
|-------|-----------|-----------|-----------------|
| lattice-core | Good (errors, streaming, transport, router) | 3 failing | Solid, but no test for `chat()` itself |
| lattice-agent | State tests only | None | Gaps: no test for `run()`, `chat_with_retry()`, tool execution |
| lattice-memory | InMemory + SQLite basic | None | No concurrency test for global store |
| lattice-harness | None | None | No test coverage at all |
| lattice-cli | None | None | No test coverage |

**Major gap**: No integration test that exercises `resolve() → chat() → stream events → chat_complete()` end-to-end with a mock HTTP server.

---

## Security Assessment

| Area | Risk | Status |
|------|------|--------|
| HTTPS enforcement | Base URL validation | **Good** — `validate_base_url()` rejects non-localhost HTTP |
| API key in logs | `tracing::info!` with key preview | **Good** — only first 4 chars logged |
| Command injection | `bash`/`run_command` tools | **P0** — prefix matching is insufficient (N2) |
| Path traversal | `..` in file paths | **Good** — `sandbox.check_read` blocks `..` |
| Sensitive file access | `.env`, `credentials.json` | **Good** — `sensitive_files` list |
| Tool result size | Unbounded output | **Good** — 1MB default limit |
| Temp file cleanup | `execute_code` leaves files in `/tmp` | **P3** — no cleanup of `lattice_code_*` dirs |

---

## Summary of Findings

| ID | Level | Component | Description |
|----|-------|-----------|-------------|
| C1 | Blocker | lattice-agent | Clippy: `identity_op` in sandbox.rs |
| C2 | Blocker | e2e tests | `regress_missing_credential_errors` stale |
| C3 | Blocker | e2e tests | `test_permissive_fallback_deepseek_model` stale |
| C4 | Blocker | e2e tests | `fallback_errors_on_missing_provider_metadata` stale |
| N1 | P0 | lattice-agent | UTF-8 panic in `Agent::run()` memory save |
| N2 | P0 | lattice-agent | Command injection via prefix-matched allowlist |
| N3 | P0 | lattice-memory | Global state in `InMemoryMemory` causes test pollution |
| N4 | P1 | lattice-core | `init_debug_logging` panics on file open |
| N5 | P1 | lattice-core | `init_logging` silently ignores second call |
| N6 | P1 | lattice-core | Gemini random tool call IDs break idempotency |
| N7 | P1 | lattice-core | SSE connection errors not classified |
| N8 | P2 | lattice-agent | Mid-stream errors not retried |
| N9 | P2 | lattice-agent | `chat_with_retry` clones messages per attempt |
| N10 | P2 | lattice-core | Deprecated `denormalize_stream_chunk` still implemented |
| N11 | P2 | lattice-memory | `async_trait` overhead for sync implementations |
| N12 | P3 | lattice-core | `resolve_permissive` doesn't lowercase model_part |
| N13 | P3 | lattice-core | HTTP read timeout unclear for SSE |
| N14 | P3 | lattice-core | `ErrorClassifier` only checks one 400 pattern |

**Priority action**: Fix C1-C4 (CI blockers), then N1 (UTF-8 panic), then N2 (security).

---

*以奋斗者为本。力出一孔，先修 CI 再论其他。*
