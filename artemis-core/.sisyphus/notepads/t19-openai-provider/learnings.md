# T19: OpenAI Provider - Learnings

## Key Discovery: Actual Transport API vs Plan Pseudocode

The plan's pseudocode assumed `ChatCompletionsTransport` had `normalize_messages()`, `normalize_tools()`, and `denormalize_response()` as separate methods. The actual implementation has:
- `normalize_request(&ChatRequest) -> Result<serde_json::Value, TransportError>` — handles everything at once
- `denormalize_response(&serde_json::Value) -> Result<ChatResponse, TransportError>` — converts response JSON to ChatResponse

Both methods come from the `Transport` trait in `src/transport/chat_completions.rs` (not the `Transport` trait in `src/transport/mod.rs` — they're different traits with the same name).

## reqwest `json` feature

reqwest 0.12's `json()` method on `RequestBuilder` requires the `json` feature. We had to add `"json"` to the reqwest features list in Cargo.toml. The feature was not enabled by default when other features were already specified.

## Module structure

Created `src/providers/` directory with `mod.rs` and `openai.rs`. Added `pub mod providers;` to `src/lib.rs`.

## Test results

235 tests pass (193 unit + 42 integration), 0 failures.
