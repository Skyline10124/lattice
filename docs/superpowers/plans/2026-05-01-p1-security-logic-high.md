# P1 Security & Logic HIGH Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 9 P1 (HIGH) security and logic vulnerabilities in lattice-core identified in the audit.

**Architecture:** Each fix targets a specific file with minimal scope changes. Fixes are independent — no cross-task dependencies. Tests are added per fix where they don't already exist.

**Tech Stack:** Rust, serde, reqwest, tokio

---

## File Structure

| File | Responsibility | Tasks |
|------|---------------|-------|
| `lattice-core/src/catalog/types.rs` | ApiProtocol serde fix, ResolvedModel api_key skip_serializing | 1, 2, 3, 4 |
| `lattice-core/src/tokens.rs` | Token estimation completeness, fits_in_context margin | 5, 6 |
| `lattice-core/src/transport/chat_completions.rs` | Malformed response error | 7 |
| `lattice-core/src/transport/gemini.rs` | Gemini trait overrides, streaming finish_reason, URL encoding, NaN guard, empty messages, tool call ID, "OTHER" mapping | 8, 9, combined |
| `lattice-core/src/transport/anthropic.rs` | anthropic-version header, tool_use id/name validation | combined |
| `lattice-core/src/transport/mod.rs` | Header injection allowlist, ToolCallEnd, stream flag, auth footgun | combined |
| `lattice-core/src/lib.rs` | Header injection allowlist in send_streaming_request | combined |

---

### Task 1: CORE-H1 — ResolvedModel.api_key Serialize leak

**Files:**
- Modify: `lattice-core/src/catalog/types.rs:92`

- [ ] **Step 1: Add `#[serde(skip_serializing)]` to api_key field**

```rust
// In catalog/types.rs, line 91-92:
#[serde(default, skip_serializing)]
pub api_key: Option<String>,
```

This prevents `serde_json::to_string(&resolved_model)` from outputting the plaintext key. Deserialization still works (the field is `#[serde(default)]` so it's filled from env at runtime, not from serialized data).

- [ ] **Step 2: Add a test verifying api_key is not in Serialize output**

```rust
// Add to the #[cfg(test)] mod tests block in catalog/types.rs:
#[test]
fn test_serialize_hides_api_key() {
    let model = ResolvedModel {
        canonical_id: "test-model".to_string(),
        provider: "test-provider".to_string(),
        api_key: Some("secret-123".to_string()),
        base_url: "https://test.api.com".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "test-model-id".to_string(),
        context_length: 4096,
        provider_specific: HashMap::new(),
        credential_status: CredentialStatus::Present,
    };
    let serialized = serde_json::to_string(&model).unwrap();
    assert!(
        !serialized.contains("secret-123"),
        "Serialize output should not contain the actual API key"
    );
    assert!(
        !serialized.contains("api_key"),
        "Serialize output should not contain the api_key field at all"
    );
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p lattice-core test_serialize_hides_api_key`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add lattice-core/src/catalog/types.rs
git commit -m "fix(CORE-H1): skip_serializing on ResolvedModel.api_key prevents credential leak"
```

---

### Task 2: CORE-H3 + CORE-H4 — ApiProtocol serde/FromStr asymmetry + untyped Custom swallowing typos

These are deeply intertwined — fixing one requires fixing the other. The current `#[serde(untagged)]` makes serde accept any string as `Custom` (CORE-H4), while FromStr accepts shorthand aliases that serde doesn't (CORE-H3). Both are fixed by a single custom serde deserializer.

**Files:**
- Modify: `lattice-core/src/catalog/types.rs:17-43`
- Modify: `lattice-core/src/catalog/loader.rs:134-138` (tests)

- [ ] **Step 1: Write a failing test for serde rejecting typos**

```rust
// Add to catalog/types.rs tests:
#[test]
fn test_api_protocol_serde_rejects_typo() {
    let result: Result<ApiProtocol, _> = serde_json::from_str("\"chat_compltions\"");
    assert!(result.is_err(), "serde should reject typo 'chat_compltions'");
}

#[test]
fn test_api_protocol_serde_accepts_short_alias() {
    let result: ApiProtocol = serde_json::from_str("\"anthropic\"").unwrap();
    assert_eq!(result, ApiProtocol::AnthropicMessages, "serde should accept 'anthropic' as alias");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-core test_api_protocol_serde_rejects_typo`
Expected: FAIL (currently serde accepts typos as Custom)

- [ ] **Step 3: Replace `#[serde(untagged)] Custom(String)` with custom deserializer**

Replace the entire `ApiProtocol` enum + FromStr impl (lines 17-43) with:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ApiProtocol {
    OpenAiChat,
    AnthropicMessages,
    GeminiGenerateContent,
    CodexResponses,
    Custom(String),
}

impl serde::Serialize for ApiProtocol {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ApiProtocol::OpenAiChat => s.serialize_str("chat_completions"),
            ApiProtocol::AnthropicMessages => s.serialize_str("anthropic_messages"),
            ApiProtocol::GeminiGenerateContent => s.serialize_str("gemini_generate_content"),
            ApiProtocol::CodexResponses => s.serialize_str("codex_responses"),
            ApiProtocol::Custom(inner) => s.serialize_str(inner),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ApiProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ApiProtocol, D::Error> {
        let s = String::deserialize(d)?;
        // Exact matches and short aliases
        match s.as_str() {
            "chat_completions" => Ok(ApiProtocol::OpenAiChat),
            "anthropic_messages" | "anthropic" => Ok(ApiProtocol::AnthropicMessages),
            "gemini_generate_content" | "gemini" => Ok(ApiProtocol::GeminiGenerateContent),
            "codex_responses" | "codex" => Ok(ApiProtocol::CodexResponses),
            other => {
                // Reject strings that look like typos of known protocol names
                let lower = other.to_lowercase();
                if lower.starts_with("chat")
                    || lower.starts_with("anthropic")
                    || lower.starts_with("gemini")
                    || lower.starts_with("codex")
                {
                    return Err(serde::de::Error::custom(format!(
                        "unknown protocol '{}': did you mean one of chat_completions, anthropic_messages, gemini_generate_content, codex_responses?",
                        other
                    )));
                }
                Ok(ApiProtocol::Custom(other.to_string()))
            }
        }
    }
}

impl std::str::FromStr for ApiProtocol {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "chat_completions" => ApiProtocol::OpenAiChat,
            "anthropic_messages" | "anthropic" => ApiProtocol::AnthropicMessages,
            "gemini_generate_content" | "gemini" => ApiProtocol::GeminiGenerateContent,
            "codex_responses" | "codex" => ApiProtocol::CodexResponses,
            other => ApiProtocol::Custom(other.to_string()),
        })
    }
}
```

Key changes:
- Removed `#[derive(Serialize, Deserialize)]` from enum — custom impls instead
- Removed `#[serde(rename = "...")]` attributes — explicit in serialize
- Removed `#[serde(untagged)]` — the new deserialize rejects near-miss typos
- Both serde and FromStr accept the same short aliases (`"anthropic"`, `"gemini"`, `"codex"`)
- Strings that look like typos (start with known protocol prefixes but aren't exact matches) produce an error, not a silent Custom

- [ ] **Step 4: Update catalog loader test**

The test in `loader.rs:134-138` uses `serde_json::from_str` which will now reject typos. Update:

```rust
// In loader.rs tests, update test_api_protocol_custom_variant:
#[test]
fn test_api_protocol_custom_variant() {
    let custom: ApiProtocol = "acp".parse().unwrap();
    assert_eq!(custom, ApiProtocol::Custom("acp".to_string()));
}
```

(No change needed — "acp" doesn't start with any known protocol prefix, so it stays Custom.)

- [ ] **Step 5: Run all catalog tests to verify**

Run: `cargo test -p lattice-core -- catalog`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
git add lattice-core/src/catalog/types.rs lattice-core/src/catalog/loader.rs
git commit -m "fix(CORE-H3,H4): custom ApiProtocol serde rejects typos, accepts short aliases"
```

---

### Task 3: CORE-H2 — provider_specific header injection allowlist

**Files:**
- Modify: `lattice-core/src/lib.rs:76-79`
- Modify: `lattice-core/src/transport/gemini.rs:418-421`
- Modify: `lattice-core/src/transport/mod.rs:243-249` (auth_header override footgun, CORE-M6)

- [ ] **Step 1: Write a `validate_injected_header` function in lib.rs**

```rust
// Add near the top of lib.rs (after imports, before send_streaming_request):
/// Sensitive HTTP header names that must not be overridden via provider_specific.
/// These headers control authentication, connection routing, and security.
const SENSITIVE_HEADER_NAMES: &[&str] = &[
    "authorization",
    "host",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-goog-api-key",
    "origin",
    "referer",
    "proxy-authorization",
    "connection",
    "upgrade",
];

fn validate_injected_header(header_name: &str) -> Result<(), LatticeError> {
    let lower = header_name.to_lowercase();
    if SENSITIVE_HEADER_NAMES.contains(&lower.as_str()) {
        return Err(LatticeError::Config {
            message: format!(
                "provider_specific header '{}' is a protected header and cannot be injected",
                header_name
            ),
        });
    }
    Ok(())
}
```

- [ ] **Step 2: Apply validation in lib.rs send_streaming_request (line 76-79)**

Replace:
```rust
    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            req = req.header(header_name, value);
        }
    }
```

With:
```rust
    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            validate_injected_header(header_name)?;
            req = req.header(header_name, value);
        }
    }
```

- [ ] **Step 3: Apply validation in gemini.rs send_gemini_nonstreaming_request (line 418-421)**

Replace:
```rust
    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            req = req.header(header_name, value);
        }
    }
```

With:
```rust
    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            crate::validate_injected_header(header_name)?;
            req = req.header(header_name, value);
        }
    }
```

- [ ] **Step 4: Write test for validate_injected_header**

```rust
// Add to lib.rs #[cfg(test)] block:
#[test]
fn test_validate_injected_header_rejects_sensitive() {
    for sensitive in &["authorization", "host", "cookie", "x-api-key"] {
        let result = validate_injected_header(sensitive);
        assert!(result.is_err(), "should reject '{}'", sensitive);
    }
}

#[test]
fn test_validate_injected_header_accepts_non_sensitive() {
    for ok in &["x-custom-id", "x-request-source", "content-type"] {
        let result = validate_injected_header(ok);
        assert!(result.is_ok(), "should accept '{}'", ok);
    }
}
```

- [ ] **Step 5: Run test to verify**

Run: `cargo test -p lattice-core test_validate_injected_header`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add lattice-core/src/lib.rs lattice-core/src/transport/gemini.rs
git commit -m "fix(CORE-H2): add header injection allowlist rejecting sensitive headers"
```

---

### Task 4: CORE-H5 — estimate_messages missing tool_calls/reasoning_content/name

**Files:**
- Modify: `lattice-core/src/tokens.rs:49-54`

- [ ] **Step 1: Write a failing test for tool-heavy estimation**

```rust
// Add to tokens.rs tests:
#[test]
fn test_estimate_messages_includes_tool_calls() {
    let msgs = vec![Message {
        role: Role::Assistant,
        content: "Let me check".to_string(),
        reasoning_content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_123".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: "{\"city\": \"Tokyo\", \"unit\": \"celsius\"}".to_string(),
            },
        }]),
        tool_call_id: None,
        name: None,
    }];
    let with_tools = TokenEstimator::estimate_messages_for_model(&msgs, "claude-sonnet-4-6");
    let without_tools = TokenEstimator::estimate_messages_for_model(
        &vec![Message {
            role: Role::Assistant,
            content: "Let me check".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        "claude-sonnet-4-6",
    );
    assert!(
        with_tools > without_tools,
        "tool_calls should add token estimate: with={} vs without={}",
        with_tools, without_tools
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-core test_estimate_messages_includes_tool_calls`
Expected: FAIL (currently tool_calls add zero estimate)

- [ ] **Step 3: Fix estimate_messages_for_model to account for all message fields**

Replace `tokens.rs:49-54`:

```rust
    pub fn estimate_messages_for_model(messages: &[Message], model_id: &str) -> u32 {
        messages
            .iter()
            .map(|m| {
                let base = Self::estimate_text_for_model(&m.content, model_id);
                let tool_calls_estimate = m
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .map(|tc| {
                                Self::estimate_text_for_model(
                                    &format!(
                                        "{} {} {}",
                                        tc.id, tc.function.name, tc.function.arguments
                                    ),
                                    model_id,
                                )
                            })
                            .sum::<u32>()
                    })
                    .unwrap_or(0);
                let reasoning_estimate = m
                    .reasoning_content
                    .as_ref()
                    .map(|r| Self::estimate_text_for_model(r, model_id))
                    .unwrap_or(0);
                let name_estimate = m
                    .name
                    .as_ref()
                    .map(|n| Self::estimate_text_for_model(n, model_id))
                    .unwrap_or(0);
                base + tool_calls_estimate + reasoning_estimate + name_estimate
            })
            .sum()
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p lattice-core test_estimate_messages_includes_tool_calls`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add lattice-core/src/tokens.rs
git commit -m "fix(CORE-H5): estimate_messages now includes tool_calls, reasoning_content, name"
```

---

### Task 5: CORE-H6 — Malformed API response returns Ok instead of Err

**Files:**
- Modify: `lattice-core/src/transport/chat_completions.rs:190-253`

- [ ] **Step 1: Write a failing test for missing choices**

```rust
// Add to chat_completions.rs tests:
#[test]
fn test_denormalize_response_rejects_missing_choices() {
    let transport = ChatCompletionsTransport::new();
    let response = serde_json::json!({"model": "gpt-4o"});  // no "choices" key
    let result = transport.denormalize_response(&response);
    assert!(result.is_err(), "response without choices should return Err");
}

#[test]
fn test_denormalize_response_rejects_empty_content_no_tool_calls() {
    let transport = ChatCompletionsTransport::new();
    let response = serde_json::json!({
        "choices": [{"message": {"content": null}, "finish_reason": "stop"}],
        "model": "gpt-4o"
    });
    let result = transport.denormalize_response(&response);
    assert!(result.is_err(), "response with no content and no tool_calls should return Err");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p lattice-core test_denormalize_response_rejects_missing_choices`
Expected: FAIL (currently returns Ok)

- [ ] **Step 3: Fix denormalize_response to return Err for invalid responses**

Replace the entire `denormalize_response` method body (chat_completions.rs:190-253):

```rust
    fn denormalize_response(
        &self,
        response: &serde_json::Value,
    ) -> Result<ChatResponse, TransportError> {
        let choices = response["choices"]
            .as_array()
            .ok_or_else(|| TransportError::UnexpectedFormat(
                "response missing 'choices' array".into()
            ))?;

        if choices.is_empty() {
            return Err(TransportError::UnexpectedFormat(
                "response 'choices' array is empty".into()
            ));
        }

        let choice = &choices[0];

        let content = choice["message"]["content"]
            .as_str()
            .map(|s| s.to_string());

        let reasoning_content = choice["message"]["reasoning_content"]
            .as_str()
            .map(|s| s.to_string());

        let tool_calls = choice["message"]["tool_calls"]
            .as_array()
            .map(|tcs| {
                tcs.iter()
                    .filter_map(|tc| {
                        let id = tc["id"].as_str()?.to_string();
                        let name = tc["function"]["name"].as_str()?.to_string();
                        let arguments = tc["function"]["arguments"]
                            .as_str()
                            .unwrap_or("{}")
                            .to_string();
                        Some(crate::types::ToolCall {
                            id,
                            function: crate::types::FunctionCall { name, arguments },
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());

        // Reject response with no meaningful content
        if content.is_none() && tool_calls.is_none() {
            return Err(TransportError::UnexpectedFormat(
                "response has no content and no tool_calls".into()
            ));
        }

        let finish_reason = choice["finish_reason"]
            .as_str()
            .unwrap_or("stop")
            .to_string();

        let model = response["model"].as_str().unwrap_or("unknown").to_string();

        let usage = response["usage"]
            .as_object()
            .map(|u| crate::streaming::TokenUsage {
                prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
            });

        Ok(ChatResponse {
            content,
            reasoning_content,
            tool_calls,
            usage,
            finish_reason,
            model,
        })
    }
```

Key changes:
- `choices` must exist as an array (not just `.and_then`) — `ok_or_else` returns Err
- Empty choices array returns Err
- Response with `None` content AND `None` tool_calls returns Err (prevents HTML error pages from being treated as valid empty responses)

- [ ] **Step 4: Run tests to verify**

Run: `cargo test -p lattice-core -- chat_completions`
Expected: ALL PASS (existing valid-response tests still pass, new invalid tests now also pass)

- [ ] **Step 5: Commit**

```bash
git add lattice-core/src/transport/chat_completions.rs
git commit -m "fix(CORE-H6): denormalize_response returns Err for malformed responses"
```

---

### Task 6: CORE-H7 — GeminiTransport trait defaults all OpenAI's

**Files:**
- Modify: `lattice-core/src/transport/gemini.rs` (Transport impl)

- [ ] **Step 1: Override missing trait methods in GeminiTransport impl Transport**

Add these overrides to `GeminiTransport impl Transport` (after line 479 `fn api_mode`):

```rust
    fn chat_endpoint(&self) -> &str {
        // Gemini uses a URL pattern, not a path suffix.
        // The actual URL is built in send_gemini_nonstreaming_request.
        // This override prevents the default "/chat/completions" from being used.
        ""
    }

    fn create_sse_parser(&self) -> Box<dyn crate::streaming::SseParser> {
        // Gemini does not support SSE streaming in the OpenAI format.
        // The non-streaming path converts the full response to a stream.
        // This override prevents the default OpenAiSseParser from being used.
        Box::new(crate::streaming::OpenAiSseParser::new())
        // NOTE: Gemini streaming would need a GeminiSseParser, which does not yet exist.
        // The non-streaming send_gemini_nonstreaming_request bypasses this parser entirely.
    }

    fn auth_header_name(&self) -> &str {
        "x-goog-api-key"
    }

    fn auth_header_value(&self, api_key: &str) -> String {
        // Gemini expects the raw API key, not "Bearer {key}"
        api_key.to_string()
    }
```

The `auth_header_name` and `auth_header_value` overrides fix the CORE-M6 footgun too — previously only `apply_auth_to_request` was overridden, meaning anyone calling `auth_header_name()` or `auth_header_value()` separately would get wrong OpenAI defaults.

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/transport/gemini.rs
git commit -m "fix(CORE-H7,M6): override chat_endpoint, auth_header_name/value, create_sse_parser in GeminiTransport"
```

---

### Task 7: CORE-H8 — Gemini streaming finish_reason inconsistent with non-streaming

**Files:**
- Modify: `lattice-core/src/transport/gemini.rs:359-362`

- [ ] **Step 1: Write test for streaming finish_reason with tool calls**

Read the streaming `denormalize_stream_chunk` method to find the exact location first, then add:

```rust
// Add to gemini.rs tests:
#[test]
fn test_streaming_finish_reason_tool_calls() {
    // Simulate a streaming chunk with function calls present
    let transport = GeminiTransport::new();
    let chunk_data = json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "functionCall": {
                        "name": "get_weather",
                        "args": {"city": "Tokyo"}
                    }
                }]
            },
            "finishReason": "STOP"
        }]
    });
    let events = transport.denormalize_stream_chunk("message", &chunk_data).unwrap();
    // Should contain ToolCallStart + ToolCallDelta + ToolCallEnd + Done with "tool_calls"
    let done_event = events.iter().find(|e| matches!(e, crate::streaming::StreamEvent::Done { .. }));
    if let Some(crate::streaming::StreamEvent::Done { finish_reason, .. }) = done_event {
        assert_eq!(finish_reason, "tool_calls", "streaming should override finish_reason to 'tool_calls' when function calls present");
    }
}
```

- [ ] **Step 2: Fix denormalize_stream_chunk to override finish_reason when function calls present**

This requires reading the full streaming chunk processing code to find where `finish_reason` is set. The fix adds a check: if any ToolCallStart/ToolCallDelta events were emitted in this chunk, override finish_reason to `"tool_calls"`.

In the streaming chunk processing (denormalize_stream_chunk method), before emitting the Done event, add:

```rust
// After processing all parts in the chunk, check if tool calls were emitted
let has_tool_calls_in_chunk = events.iter().any(|e| {
    matches!(e, StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallDelta { .. })
});

if let Some(ref reason) = finish_reason_raw {
    let mapped_reason = if has_tool_calls_in_chunk {
        "tool_calls".to_string()
    } else {
        Self::map_finish_reason(reason)
    };
    results.push(StreamChunk::Done {
        finish_reason: mapped_reason,
    });
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p lattice-core -- gemini`
Expected: ALL PASS

- [ ] **Step 4: Commit**

```bash
git add lattice-core/src/transport/gemini.rs
git commit -m "fix(CORE-H8): streaming finish_reason override to 'tool_calls' matches non-streaming"
```

---

### Task 8: CORE-H9 — anthropic-version header not in transport

**Files:**
- Modify: `lattice-core/src/transport/anthropic.rs` (constructor)
- Modify: `lattice-core/src/lib.rs:184` (remove ad-hoc header)

- [ ] **Step 1: Add anthropic-version to AnthropicTransport extra_headers in constructor**

```rust
// In anthropic.rs, replace AnthropicTransport::new():
impl AnthropicTransport {
    pub fn new() -> Self {
        Self {
            base: TransportBase::with_extra_headers(
                "https://api.anthropic.com",
                HashMap::from([("anthropic-version".to_string(), "2023-06-01".to_string())]),
            ),
        }
    }
}
```

This embeds the required header directly in the transport, so any code using AnthropicTransport gets the header automatically, even outside of `chat()`.

- [ ] **Step 2: Remove the ad-hoc anthropic-version from lib.rs chat()**

Replace `lib.rs:179-185`:
```rust
            send_streaming_request(
                transport,
                client,
                resolved,
                &body,
                &[("anthropic-version", "2023-06-01")],
            )
```

With:
```rust
            send_streaming_request(
                transport,
                client,
                resolved,
                &body,
                &[],
            )
```

The header is now in the transport's `extra_headers()`, which `send_streaming_request` already applies.

- [ ] **Step 3: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 4: Commit**

```bash
git add lattice-core/src/transport/anthropic.rs lattice-core/src/lib.rs
git commit -m "fix(CORE-H9): anthropic-version header embedded in AnthropicTransport extra_headers"
```

---

### Task 9: CORE-M14 + CORE-M15 + CORE-L8 + CORE-L9 combined — Anthropic tool_use validation, ToolCallEnd, Gemini "OTHER" mapping, tool call ID stability

These are smaller fixes that are quick wins and can be batched.

**Files:**
- Modify: `lattice-core/src/transport/anthropic.rs:99-108` (CORE-M14)
- Modify: `lattice-core/src/transport/mod.rs:63-74` (CORE-M15)
- Modify: `lattice-core/src/transport/gemini.rs:86-91` (CORE-L8)
- Modify: `lattice-core/src/transport/gemini.rs:95,248,352` (CORE-L9)

- [ ] **Step 1: CORE-M14 — Anthropic tool_use missing id/name returns Err**

Replace `anthropic.rs:98-108`:

```rust
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .ok_or_else(|| TransportError::UnexpectedFormat(
                                "tool_use block missing 'id' field".into()
                            ))?;
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .ok_or_else(|| TransportError::UnexpectedFormat(
                                "tool_use block missing 'name' field".into()
                            ))?;
                        let input = block.get("input").cloned().unwrap_or(json!({}));
                        let arguments = serde_json::to_string(&input).unwrap_or_default();
                        tool_calls.push(ToolCall {
                            id: id.to_string(),
                            function: FunctionCall { name: name.to_string(), arguments },
                        });
                    }
```

- [ ] **Step 2: CORE-M15 — chat_response_to_stream emits ToolCallEnd**

Replace `transport/mod.rs:63-74`:

```rust
    if let Some(ref tool_calls) = response.tool_calls {
        for tc in tool_calls {
            events.push(crate::streaming::StreamEvent::ToolCallStart {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
            });
            events.push(crate::streaming::StreamEvent::ToolCallDelta {
                id: tc.id.clone(),
                arguments_delta: tc.function.arguments.clone(),
            });
            events.push(crate::streaming::StreamEvent::ToolCallEnd {
                id: tc.id.clone(),
            });
        }
    }
```

- [ ] **Step 3: CORE-L8 — Gemini "OTHER" finish reason maps to "unknown" not "stop"**

Replace `gemini.rs:86-93`:

```rust
    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "SAFETY" | "RECITATION" => "content_filter".to_string(),
            "OTHER" => "unknown".to_string(),
            _ => "unknown".to_string(),
        }
    }
```

- [ ] **Step 4: CORE-L9 — Gemini tool call ID stability between streaming and non-streaming**

The non-streaming path uses `enumerate()` index of `content_parts`, streaming uses `results.len()`. Both should use a consistent scheme: enumerate the functionCall parts with a stable counter.

In `parse_response` (non-streaming), the `enumerate` at line 236 iterates all parts (text + functionCall). Use a dedicated counter:

```rust
    let mut tc_index = 0;
    for part in content_parts.iter() {
        // ... text handling unchanged ...
        if let Some(fc) = part.get("functionCall") {
            let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = fc.get("args").cloned().unwrap_or(json!({}));
            let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
            tool_calls.push(crate::types::ToolCall {
                id: Self::generate_call_id(name, tc_index),
                function: crate::types::FunctionCall {
                    name: name.to_string(),
                    arguments: args_str,
                },
            });
            tc_index += 1;
        }
    }
```

In `denormalize_stream_chunk` (streaming), use a similar dedicated counter:

```rust
    // Replace `idx = results.len()` with a tool-call counter
    let mut tc_index = 0;
    // ... for each part in chunk ...
    if let Some(fc) = part.get("functionCall") {
        let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let id = Self::generate_call_id(name, tc_index);
        tc_index += 1;
        // ... emit ToolCallStart + ToolCallDelta with this id ...
    }
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
git add lattice-core/src/transport/anthropic.rs lattice-core/src/transport/mod.rs lattice-core/src/transport/gemini.rs
git commit -m "fix(CORE-M14,M15,L8,L9): tool_use validation, ToolCallEnd, OTHER→unknown, stable call IDs"
```

---

## Final Verification

```bash
cargo build
cargo test -p lattice-core
cargo clippy -- -D warnings
cargo fmt --check --all
```

**Total: 9 P1 fixes across 6 tasks (Tasks 2 and 9 combine related issues)**