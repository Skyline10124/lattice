# P3 Logic/Documentation/Quality LOW Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 13 P3 (LOW) code quality and documentation issues in lattice-core identified in the audit.

**Architecture:** Low-risk changes: dead code removal, minor logic corrections, doc fixes. No behavioral changes for end users.

**Tech Stack:** Rust, serde

---

### Task 1: CORE-L1 — stream:false not written to body

**Files:**
- Modify: `lattice-core/src/transport/mod.rs:287-291`

- [ ] **Step 1: Write stream:false explicitly**

Replace `transport/mod.rs:287-291`:

```rust
    fn set_stream_flag(&self, body: &mut Value, stream: bool) {
        body["stream"] = Value::Bool(stream);
    }
```

Now both `true` and `false` are explicitly written. Providers like DeepSeek that default to streaming will see `"stream": false` and honor it.

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/transport/mod.rs
git commit -m "fix(CORE-L1): set_stream_flag writes explicit stream:false for non-streaming requests"
```

---

### Task 2: CORE-L2 — temperature from_f64 fallback to 0

**Files:**
- Modify: `lattice-core/src/transport/mod.rs:276-278`

- [ ] **Step 1: Change fallback to omit the field instead of using 0**

Replace `transport/mod.rs:276-278`:

```rust
                body["temperature"] = Value::Number(
                    serde_json::Number::from_f64(temp)
                        .unwrap_or_else(|| {
                            tracing::warn!("temperature value {} cannot be represented as JSON number, omitting", temp);
                            // Return a sentinel; we'll remove the field below
                            serde_json::Number::from(0)
                        }),
                );
                // If from_f64 failed (value exceeds f64 precision), remove the field
                if body["temperature"] == Value::Number(serde_json::Number::from(0))
                    && temp != 0.0
                {
                    body.as_object_mut().map(|o| o.remove("temperature"));
                }
```

Wait, this is overly complex. Simpler approach: just omit the field when from_f64 fails:

```rust
            if temp.is_nan() || temp.is_infinite() {
                tracing::warn!(
                    "temperature value {} is NaN or infinite, omitting temperature field",
                    temp
                );
            } else if let Some(num) = serde_json::Number::from_f64(temp) {
                body["temperature"] = Value::Number(num);
            } else {
                tracing::warn!(
                    "temperature value {} exceeds JSON number precision, omitting temperature field",
                    temp
                );
            }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/transport/mod.rs
git commit -m "fix(CORE-L2): temperature from_f64 failure omits field with warning instead of defaulting to 0"
```

---

### Task 3: CORE-L3 + CORE-L4 — is_openai_model dead code + false positives

**Files:**
- Modify: `lattice-core/src/tokens.rs:7-14`

- [ ] **Step 1: Remove dead code and fix false positives**

Replace `tokens.rs:7-14`:

```rust
    fn is_openai_model(model_id: &str) -> bool {
        let lower = model_id.to_lowercase();
        lower.starts_with("gpt-")
            || lower.starts_with("gpt-4o")
            || lower.starts_with("gpt-5")
            || lower == "o1"
            || lower.starts_with("o1-")
            || lower == "o3"
            || lower.starts_with("o3-")
            || lower == "o4"
            || lower.starts_with("o4-")
    }
```

Changes:
- Removed `lower.contains("gpt-4o")` — covered by `starts_with("gpt-")` (CORE-L3)
- Added explicit `gpt-4o` and `gpt-5` checks (these are OpenAI models)
- Changed `starts_with("o1")` to exact match `"o1"` OR `starts_with("o1-")` — prevents false positive for `"o1000-custom"` (CORE-L4)
- Same for `"o3"` and `"o4"` — exact match or with hyphen suffix

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/tokens.rs
git commit -m "fix(CORE-L3,L4): remove dead gpt-4o branch, fix o3/o4 false positives"
```

---

### Task 4: CORE-L5 — retry jitter ineffective when base >= max_delay

**Files:**
- Modify: `lattice-core/src/retry.rs:21-29`

- [ ] **Step 1: Add jitter before clamping to max_delay**

Replace `retry.rs:21-29`:

```rust
    pub fn jittered_backoff(&self, attempt: u32) -> Duration {
        let base = self.base_delay * 2u32.saturating_pow(attempt);
        let capped = std::cmp::min(base, self.max_delay);
        // Add jitter before clamping: random +/- 50% of capped value
        // When capped == max_delay, jitter still applies by subtracting
        // instead of always returning max_delay exactly.
        let jitter_range = capped.as_secs_f64() * 0.5;
        let jittered = capped.as_secs_f64() + (rand::random::<f64>() - 0.5) * jitter_range;
        let jittered = std::cmp::max(jittered, 0.0);  // Never negative
        std::cmp::min(
            Duration::from_secs_f64(jittered),
            self.max_delay,
        )
    }
```

Key change: jitter is now centered around `capped` (+/- 50%), not always additive. When `capped == max_delay`, jitter subtracts up to 50% (so result varies between 50%-100% of max_delay). Collision avoidance works even when base >= max_delay.

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS (jitter test still works — jittered value is within bounds)

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/retry.rs
git commit -m "fix(CORE-L5): centered jitter ensures collision avoidance even when base >= max_delay"
```

---

### Task 5: CORE-L6 — from_data silently drops duplicate canonical_id

**Files:**
- Modify: `lattice-core/src/catalog/loader.rs:34-39`

- [ ] **Step 1: Warn on duplicate canonical_id**

Replace `loader.rs:34-39`:

```rust
    fn from_data(data: CatalogData) -> Self {
        let mut models: HashMap<String, ModelCatalogEntry> = HashMap::new();
        for m in data.models {
            if models.contains_key(&m.canonical_id) {
                tracing::warn!(
                    "catalog: duplicate canonical_id '{}', later entry overwrites earlier",
                    m.canonical_id
                );
            }
            models.insert(m.canonical_id.clone(), m);
        }
        Catalog {
            models,
            aliases: data.aliases,
            provider_defaults: data.provider_defaults,
        }
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/catalog/loader.rs
git commit -m "fix(CORE-L6): warn on duplicate canonical_id in catalog data instead of silently dropping"
```

---

### Task 6: CORE-L7 — Gemini tool result heuristic JSON parse

**Files:**
- Modify: `lattice-core/src/transport/gemini.rs:151-158`

- [ ] **Step 1: Parse as JSON only for valid JSON, warn on heuristic mismatch**

Replace `gemini.rs:151-158`:

```rust
                    let response: Value = if msg.content.trim().starts_with('{')
                        || msg.content.trim().starts_with('[')
                    {
                        match serde_json::from_str(&msg.content) {
                            Ok(v) => v,
                            Err(_) => {
                                tracing::warn!(
                                    "Gemini tool result starts with JSON delimiter but failed to parse, wrapping as output"
                                );
                                json!({"output": msg.content})
                            }
                        }
                    } else {
                        json!({"output": msg.content})
                    };
```

Change: replaced `unwrap_or_else` with explicit `match` + `tracing::warn`, making the fallback visible in logs.

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/transport/gemini.rs
git commit -m "fix(CORE-L7): warn when Gemini tool result heuristic JSON parse fails"
```

---

### Task 7: CORE-L10 — SSE buffer O(n^2) reallocation

**Files:**
- Modify: `lattice-core/src/streaming.rs:454`

- [ ] **Step 1: Use String::truncate + String::replace_range instead of reallocating**

Replace the buffer handling in `sse_from_bytes_stream`. Instead of:

```rust
            buf = buf[pos + 2..].to_string();
```

Use:

```rust
            // Drain processed portion without reallocating the entire String.
            // Shift remaining bytes to the front and truncate.
            let remaining = buf[pos + 2..].to_string();
            buf.clear();
            buf.push_str(&remaining);
```

Wait, this still copies. Better: use `buf.drain(..pos + 2)` which is O(remaining) not O(total):

```rust
            buf.drain(..pos + 2);
```

`String::drain(..n)` removes the first n bytes in-place, shifting the rest to the front. This is O(remaining) not O(original_size), eliminating the O(n^2) pattern.

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/src/streaming.rs
git commit -m "perf(CORE-L10): use String::drain for SSE buffer, eliminating O(n^2) reallocation"
```

---

### Task 8: CORE-L11 — OpenAiSseParser ToolCallEnd order uncertain

**Files:**
- Modify: `lattice-core/src/streaming.rs` (OpenAiSseParser struct)

- [ ] **Step 1: Change tool_call_ids from HashMap to IndexMap for deterministic order**

The `tool_call_ids` field in `OpenAiSseParser` is currently `HashMap<u32, String>`. Replace with `IndexMap<u32, String>` from the `indexmap` crate, which preserves insertion order.

1. Add `indexmap = "2"` to `lattice-core/Cargo.toml` dependencies.
2. In streaming.rs, change the `OpenAiSseParser` struct:

```rust
pub struct OpenAiSseParser {
    tool_call_ids: indexmap::IndexMap<u32, String>,
}
```

3. Update constructor:

```rust
impl OpenAiSseParser {
    pub fn new() -> Self {
        Self {
            tool_call_ids: indexmap::IndexMap::new(),
        }
    }
}
```

4. `drain()` on IndexMap preserves insertion order, so `for id in self.tool_call_ids.drain().map(|(_, id)| id)` now iterates in the same order as ToolCallStart events.

- [ ] **Step 2: Run tests**

Run: `cargo test -p lattice-core`
Expected: ALL PASS

- [ ] **Step 3: Commit**

```bash
git add lattice-core/Cargo.toml lattice-core/src/streaming.rs
git commit -m "fix(CORE-L11): use IndexMap for deterministic ToolCallEnd ordering"
```

---

### Task 9: CORE-L12 — Multiple documentation mismatches

**Files:**
- Modify: `CLAUDE.md:95-98` (module map)
- Modify: `lattice-core/src/transport/mod.rs` (deprecated docstring)

- [ ] **Step 1: Fix CLAUDE.md module map**

Replace the lattice-core module map entry for `providers/`:

```
| `providers/` | Concrete providers: `openai`, `anthropic`, `deepseek`, `gemini`, `groq`, `mistral`, `ollama`, `xai` |
```

With:

```
| `transport/` | Unified Transport trait, protocol adapters (ChatCompletions, Anthropic, Gemini, OpenAICompat), dispatcher |
```

Also add a line for the streaming module noting Gemini doesn't have a SSE parser:

```
| `streaming` | SSE parsers (OpenAI, Anthropic) via `sse_from_bytes_stream`, `StreamEvent`. Note: Gemini uses non-streaming path. |
```

- [ ] **Step 2: Fix transport/mod.rs deprecated docstring**

In `transport/mod.rs:211`, the doc says "Default: returns an empty vec (no streaming support)" but doesn't mention the method is deprecated. Add:

```rust
    #[deprecated(note = "Use SseParser via chat() instead. This method produces divergent output and will be removed in a future version.")]
    fn denormalize_stream_chunk(&self, _event_type: &str, _data: &Value) -> Vec<StreamEvent> {
        vec![]
    }
```

(The `#[deprecated]` attribute is already there; just update the note text.)

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md lattice-core/src/transport/mod.rs
git commit -m "docs(CORE-L12): fix module map, clarify streaming/Gemini, update deprecated docstring"
```

---

## Final Verification

```bash
cargo build
cargo test -p lattice-core
cargo clippy -- -D warnings
cargo fmt --check --all
```

**Total: 9 tasks covering all 13 P3 issues**