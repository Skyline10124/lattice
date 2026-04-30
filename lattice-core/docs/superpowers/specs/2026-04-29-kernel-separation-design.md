# LATTICE Kernel Separation Design

**Date**: 2026-04-29
**Status**: approved

## Goal

Split the monolithic `lattice-core` crate into five focused crates following
the model-centric philosophy: core does model routing + inference, and nothing
else. Agent logic, persistent memory, token budgeting, and Python bindings are
independent crates.

## Crate Architecture

```
lattice-python        PyO3 bindings, exposes all four lower crates to Python
    ↓
lattice-agent         AgentLoop, conversation state, tool boundary
    ↓
lattice-memory        Cross-session persistent memory (trait)
lattice-token-pool    Multi-agent shared token budget (trait)
    ↓
lattice-core          Model resolution + streaming inference
```

Dependencies are one-way: nothing in core depends on agent/memory/token.

## lattice-core

### Responsibility

Given a model name, resolve to provider + credentials + endpoint. Given
messages, send them and stream back the response.

### Public API

```rust
/// Resolve a model name (or alias) to connection details.
pub fn resolve(model: &str) -> Result<ResolvedModel, ArtemisError>;

/// Send messages to the resolved model, returning a stream of events.
pub fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
) -> Result<impl Stream<Item = StreamEvent>, ArtemisError>;

/// Non-streaming convenience: collect all events into a ChatResponse.
pub fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
) -> Result<ChatResponse, ArtemisError>;
```

### Internal modules (not re-exported unless needed by other crates)

| Module | Purpose |
|--------|---------|
| `catalog` | Model catalog, aliases, provider defaults, `ResolvedModel`, `ApiProtocol` |
| `router` | `ModelRouter`: normalize model IDs, resolve aliases, select provider |
| `provider` | `Provider` trait, `ChatRequest`/`ChatResponse`, shared HTTP client |
| `providers` | Concrete providers: OpenAI, Anthropic, Gemini, DeepSeek, Groq, Mistral, Ollama, xAI |
| `transport` | Unified `Transport` trait, `TransportDispatcher`, protocol adapters |
| `streaming` | SSE parsers (`OpenAiSseParser`, `AnthropicSseParser`), `StreamEvent` |
| `retry` | `RetryPolicy` with jittered exponential backoff |
| `tokens` | `TokenEstimator`: tiktoken for OpenAI, char/4 estimate for others |
| `errors` | `ArtemisError` enum, `ErrorClassifier` |
| `types` | `Role`, `Message`, `ToolDefinition`, `ToolCall`, `FunctionCall` |

### Runtime

`chat()` and `chat_complete()` are async functions. Core does NOT create its own
tokio runtime. Callers (lattice-agent, lattice-python) bring their runtime
handle. The shared `reqwest::Client` is a `LazyLock` static.

### No dependency on

- PyO3
- tokio runtime management (callers manage their own)
- Agent logic
- Python binding types

## lattice-agent

### Responsibility

Multi-turn conversation: maintain message history, invoke core for each turn,
handle tool calls, enforce token budget, and fallback across providers.

### Public API (draft)

```rust
pub struct Agent {
    /// Create an agent bound to a resolved model.
    pub fn new(resolved: ResolvedModel) -> Self;

    /// Override the default retry policy.
    pub fn with_retry(self, policy: RetryPolicy) -> Self;

    /// Inject a memory backend (default: in-memory HashMap).
    pub fn with_memory(self, memory: Box<dyn Memory>) -> Self;

    /// Inject a token pool (default: unlimited).
    pub fn with_token_pool(self, pool: Box<dyn TokenPool>) -> Self;

    /// Send a user message, get streaming events back.
    pub fn send(&mut self, message: &str) -> impl Iterator<Item = LoopEvent>;

    /// Submit tool call results, continue the conversation.
    pub fn submit_tools(&mut self, results: Vec<ToolResult>) -> impl Iterator<Item = LoopEvent>;
}
```

`LoopEvent` variants: `Token { text }`, `ToolCallRequired { calls }`,
`Done { usage }`, `Error { message }`.

### Internal modules

| Module | Purpose |
|--------|---------|
| `loop_` | `AgentLoop`: the run loop, `trim_conversation`, budget enforcement |
| `state` | Conversation history (`Vec<Message>`), session management |
| `tool_boundary` | `ToolCallRequest`/`ToolResult`: yield tool calls, accept results |

### Dependencies

- `lattice-core` (for `resolve`, `chat`, types)
- `lattice-memory` (optional trait)
- `lattice-token-pool` (optional trait)

### No dependency on

- PyO3

## lattice-memory

### Responsibility

Store and retrieve conversation messages across sessions. Plug in different
backends (in-memory, file, vector DB) without changing agent code.

### Public API

```rust
pub trait Memory: Send + Sync {
    /// Store a message in the given session.
    fn save(&mut self, session: &str, msg: &Message);

    /// Return all messages for a session in chronological order.
    fn history(&self, session: &str) -> Vec<Message>;

    /// Search past sessions for messages relevant to a query.
    /// Returns up to `limit` messages sorted by relevance.
    fn search(&self, query: &str, limit: usize) -> Vec<Message>;
}

/// Default implementation: in-memory HashMap. Not persisted.
pub struct InMemoryMemory { ... }

impl Memory for InMemoryMemory { ... }
```

### Dependencies

- `lattice-core` (for `Message` type)

## lattice-token-pool

### Responsibility

Share a token budget across multiple agents. Agents acquire tokens before
making API calls and release unused tokens back.

### Public API

```rust
pub trait TokenPool: Send + Sync {
    /// Try to acquire `amount` tokens. Returns false if not enough remain.
    fn acquire(&mut self, agent: &str, amount: u32) -> bool;

    /// Return unused tokens to the pool.
    fn release(&mut self, agent: &str, amount: u32);

    /// Tokens currently available.
    fn remaining(&self) -> u32;
}

/// Default implementation: no limit. acquire() always returns true.
pub struct UnlimitedPool;

impl TokenPool for UnlimitedPool { ... }
```

### Dependencies

None (standalone trait, no lattice-core types needed).

## lattice-python

### Responsibility

Thin PyO3 wrapper crate. Registers Python classes that wrap the Rust types from
the four lower crates.

### Public (Python) API

```python
import lattice_core  # crate name stays lattice_core for pip compatibility

# Model resolution
resolved = lattice_core.resolve("sonnet")

# Single-turn chat
for event in lattice_core.chat(resolved, messages):
    print(event)

# Multi-turn agent
agent = lattice_core.Agent(resolved)
for event in agent.send("Hello"):
    if event.kind == "tool_call_required":
        results = execute_tools(event.tool_calls)
        agent.submit_tools(results)
```

### Internal

Re-exports types from lower crates as PyO3 classes. Handles Python GIL,
exception conversion, and `StreamIterator` for streaming.

## What Moves

| From current `lattice-core/src/` | To |
|------|----|
| `agent_loop.rs` | `lattice-agent/src/loop_.rs` |
| `tool_boundary.rs` | `lattice-agent/src/tool_boundary.rs` |
| `streaming_bridge.rs` | `lattice-python/src/streaming_bridge.rs` |
| `engine.rs` | Split three ways: 1) `ArtemisEngine` PyClass + `Event`/`ToolCallInfo`/`PyResolvedModel` PyO3 types → `lattice-python/src/engine.rs`; 2) `run_once()`/`run_conversation()`/`submit_tool_result[s]()` → `lattice-agent/src/run.rs` (merged into `Agent::send`/`submit_tools`); 3) `resolve_model()`/`list_models()`/`register_model()` already handled by `router.rs` in core — nothing to move |
| `mock.rs` | `lattice-agent/` (only used by agent tests; not needed in core) |
| Everything else | Stays in `lattice-core` |

## What Gets Removed

- `engine.rs` `run_conversation()`, `submit_tool_result()`, `submit_tool_results()` — these are agent methods, replaced by `Agent::send()` + `Agent::submit_tools()`
- `engine.rs` `MockProvider` hardcoded fallback — agent constructors take a `ResolvedModel`, caller provides it
- `#![allow(deprecated)]` in lib.rs (no deprecated items remain)
- Unused dependencies: `uuid`, `chrono`, `pyo3-async-runtimes`

## Errors & Testing

- All crates use `lattice_core::ArtemisError` as the error type. No new error enums.
- Core tests (`cargo test --no-default-features`) stay in core. Agent tests stay in agent.
- Regression tests (714) are split across crates based on what they test.
- Characterisation tests for TransportDispatcher, AgentLoop, credential resolution, and error classification are moved with their respective modules.

## Out of Scope (for this spec)

- Memory backend implementations beyond the default `InMemoryMemory`
- TokenPool implementations beyond the default `UnlimitedPool`
- Plugin system (`Input → to_prompt → from_output → Output`)
- Agent handoff protocol
- Nix-style lockfile and content-addressed cache
