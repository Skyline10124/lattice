# Kernel Separation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split monolithic `artemis-core` into five crates: core (resolve + chat), agent (AgentLoop + conversation), memory (persistent memory trait), token-pool (shared budget trait), python (PyO3 bindings).

**Architecture:** Cargo workspace at repo root with five member crates. artemis-core keeps 10 modules and exposes `resolve()` + `chat()` + `chat_complete()`. artemis-agent depends on core + memory + token-pool. artemis-python depends on all lower crates. All crates share `artemis_core::ArtemisError`.

**Tech Stack:** Rust 2021 edition, Cargo workspace, PyO3 (python crate only)

**Spec:** `docs/superpowers/specs/2026-04-29-kernel-separation-design.md`

**Constraints:** Keep existing tests passing throughout. 714 regression tests must be split across crates correctly.

---

## Task 0: Cargo Workspace Setup

**Files:**
- Create: `Cargo.toml` (workspace root)
- Modify: `artemis-core/Cargo.toml` (drop pyo3 deps, keep as rlib only)

- [ ] **Step 1: Create workspace Cargo.toml**

```toml
# /home/astrin/artemis/Cargo.toml
[workspace]
resolver = "2"
members = [
    "artemis-core",
]
```

- [ ] **Step 2: Verify workspace builds**

Run: `cargo build` from repo root
Expected: builds artemis-core as before (but without python features)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add Cargo workspace at repo root"
```

---

## Task 1: Create artemis-memory Crate

**Files:**
- Create: `artemis-memory/Cargo.toml`
- Create: `artemis-memory/src/lib.rs`

- [ ] **Step 1: Create crate scaffold**

```bash
mkdir -p artemis-memory/src
```

- [ ] **Step 2: Write Cargo.toml**

```toml
# /home/astrin/artemis/artemis-memory/Cargo.toml
[package]
name = "artemis-memory"
version = "0.1.0"
edition = "2021"

[dependencies]
artemis-core = { path = "../artemis-core" }
```

- [ ] **Step 3: Write lib.rs**

```rust
// /home/astrin/artemis/artemis-memory/src/lib.rs
use artemis_core::types::Message;
use std::collections::HashMap;

/// Trait for cross-session conversation memory.
pub trait Memory: Send + Sync {
    /// Store a message in the given session.
    fn save(&mut self, session: &str, msg: &Message);

    /// Return all messages for a session in chronological order.
    fn history(&self, session: &str) -> Vec<Message>;

    /// Search past sessions for messages relevant to a query.
    /// Returns up to `limit` messages sorted by relevance.
    fn search(&self, _query: &str, _limit: usize) -> Vec<Message> {
        vec![] // default: no search
    }
}

/// Default implementation: in-memory HashMap. Not persisted across restarts.
pub struct InMemoryMemory {
    sessions: HashMap<String, Vec<Message>>,
}

impl InMemoryMemory {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }
}

impl Memory for InMemoryMemory {
    fn save(&mut self, session: &str, msg: &Message) {
        self.sessions
            .entry(session.to_string())
            .or_default()
            .push(msg.clone());
    }

    fn history(&self, session: &str) -> Vec<Message> {
        self.sessions.get(session).cloned().unwrap_or_default()
    }

    fn search(&self, _query: &str, _limit: usize) -> Vec<Message> {
        vec![] // in-memory impl doesn't search; embedder needed
    }
}

impl Default for InMemoryMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artemis_core::types::{Message, Role};

    #[test]
    fn test_save_and_history() {
        let mut mem = InMemoryMemory::new();
        let msg = Message {
            role: Role::User,
            content: "hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        mem.save("session-1", &msg);
        let history = mem.history("session-1");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "hello");
    }

    #[test]
    fn test_history_empty_session() {
        let mem = InMemoryMemory::new();
        assert!(mem.history("nonexistent").is_empty());
    }

    #[test]
    fn test_search_returns_empty_by_default() {
        let mem = InMemoryMemory::new();
        assert!(mem.search("query", 10).is_empty());
    }
}
```

- [ ] **Step 4: Add to workspace and verify build**

Edit `/home/astrin/artemis/Cargo.toml`, add `"artemis-memory"` to members.
Run: `cargo build -p artemis-memory`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add artemis-memory/ Cargo.toml
git commit -m "feat: add artemis-memory crate (Memory trait + InMemoryMemory)"
```

---

## Task 2: Create artemis-token-pool Crate

**Files:**
- Create: `artemis-token-pool/Cargo.toml`
- Create: `artemis-token-pool/src/lib.rs`

- [ ] **Step 1: Create crate scaffold**

```bash
mkdir -p artemis-token-pool/src
```

- [ ] **Step 2: Write Cargo.toml**

```toml
# /home/astrin/artemis/artemis-token-pool/Cargo.toml
[package]
name = "artemis-token-pool"
version = "0.1.0"
edition = "2021"

[dependencies]
# No dependencies on other artemis crates. Standalone.
```

- [ ] **Step 3: Write lib.rs**

```rust
// /home/astrin/artemis/artemis-token-pool/src/lib.rs

/// Trait for sharing a token budget across multiple agents.
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

impl TokenPool for UnlimitedPool {
    fn acquire(&mut self, _agent: &str, _amount: u32) -> bool {
        true
    }

    fn release(&mut self, _agent: &str, _amount: u32) {}

    fn remaining(&self) -> u32 {
        u32::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unlimited_pool_always_acquires() {
        let mut pool = UnlimitedPool;
        assert!(pool.acquire("agent-1", 1_000_000));
    }

    #[test]
    fn test_unlimited_pool_remaining() {
        let pool = UnlimitedPool;
        assert_eq!(pool.remaining(), u32::MAX);
    }
}
```

- [ ] **Step 4: Add to workspace and verify build**

Edit `/home/astrin/artemis/Cargo.toml`, add `"artemis-token-pool"` to members.
Run: `cargo build -p artemis-token-pool`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add artemis-token-pool/ Cargo.toml
git commit -m "feat: add artemis-token-pool crate (TokenPool trait + UnlimitedPool)"
```

---

## Task 3: Strip artemis-core Down to Core Modules

**Files:**
- Modify: `artemis-core/Cargo.toml` (remove pyo3 deps, cdylib crate-type)
- Modify: `artemis-core/src/lib.rs` (remove non-core modules, remove PyO3 pymodule fn)

- [ ] **Step 1: Rewrite Cargo.toml to remove Python dependencies**

```toml
# /home/astrin/artemis/artemis-core/Cargo.toml
[package]
name = "artemis-core"
version = "0.1.0"
edition = "2021"

[lib]
name = "artemis_core"
crate-type = ["rlib"]

[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["stream", "rustls-tls", "json"] }
async-trait = "0.1"
thiserror = "2"
reqwest-eventsource = "0.6"
futures = "0.3"
tiktoken-rs = "0.11"
rand = "0.8"
regex = "1"

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "benchmarks"
harness = false
```

Removed: `pyo3`, `pyo3-async-runtimes`, `uuid`, `chrono`, `rig-core`

- [ ] **Step 2: Rewrite lib.rs — remove non-core modules and PyO3 code**

```rust
// /home/astrin/artemis/artemis-core/src/lib.rs
pub mod catalog;
pub mod errors;
pub mod provider;
pub mod providers;
pub mod retry;
pub mod router;
pub mod streaming;
pub mod tokens;
pub mod transport;
pub mod types;

mod mock; // internal, for tests

use errors::ArtemisError;
use futures::Stream;

pub fn resolve(model: &str) -> Result<catalog::ResolvedModel, ArtemisError> {
    router::ModelRouter::new().resolve(model, None)
}

pub async fn chat(
    resolved: &catalog::ResolvedModel,
    messages: &[types::Message],
) -> Result<impl Stream<Item = streaming::StreamEvent>, ArtemisError> {
    // Stub for now — delegates to provider in Task 6
    todo!("wire up chat() through dispatcher")
}

pub fn chat_complete(
    resolved: &catalog::ResolvedModel,
    messages: &[types::Message],
) -> Result<provider::ChatResponse, ArtemisError> {
    // Stub for now
    todo!("wire up chat_complete() through dispatcher")
}
```

Note: `mock.rs` stays as `mod mock` (internal, not `pub`) since it's only used by tests. If agent tests need it later, we expose it via `#[cfg(test)]` re-export or move it.

- [ ] **Step 3: Delete non-core source files AND strip PyO3 from errors.rs**

```bash
rm artemis-core/src/agent_loop.rs
rm artemis-core/src/tool_boundary.rs
rm artemis-core/src/streaming_bridge.rs
rm artemis-core/src/engine.rs
```

Edit `artemis-core/src/errors.rs`: remove the `pub mod py_exc` block and the `From<ArtemisError> for PyErr` impl. These use `pyo3::create_exception!` and `Python::try_attach` — PyO3 types that won't exist in core after the split. Keep only the Rust `ArtemisError` enum, `Display` impl, `Error` impl, and `ErrorClassifier`. The PyO3 conversion moves to `artemis-python/src/errors.rs` in Task 7.

- [ ] **Step 4: Remove non-core test files (they'll be recreated in agent crate)**

Move affected test files aside for later use in the agent crate. Test files that depend on agent_loop, engine, or streaming_bridge won't compile under artemis-core:

```bash
# Move affected e2e tests to a temp location for later use
mkdir -p /tmp/artemis-moved-tests
mv artemis-core/tests/e2e/agent_loop_characterization.rs /tmp/artemis-moved-tests/ 2>/dev/null
mv artemis-core/tests/e2e/state_machine_characterization.rs /tmp/artemis-moved-tests/ 2>/dev/null
# Keep tests that only test core functionality
```

- [ ] **Step 5: Verify core compiles and existing core tests pass**

Run: `cargo build -p artemis-core`
Expected: fails on `todo!()` in chat/chat_complete (expected — will be fixed in Task 6)

Run: `cargo test -p artemis-core --no-default-features 2>&1 | tail -20`
Expected: catalog, router, provider, streaming, retry, errors, tokens tests pass (some tests may break due to moved files)

- [ ] **Step 6: Fix any broken tests, commit**

Run: `cargo test -p artemis-core 2>&1 | tail -5`
Fix any compilation errors from moved modules. Commit when green.

```bash
git add artemis-core/ Cargo.toml
git commit -m "refactor: strip artemis-core to core modules (resolve + chat stubs)"
```

---

## Task 4: Implement resolve() in Core

**Files:**
- Modify: `artemis-core/src/lib.rs`

- [ ] **Step 1: Replace resolve() stub with real implementation**

```rust
// /home/astrin/artemis/artemis-core/src/lib.rs — resolve() function
pub fn resolve(model: &str) -> Result<catalog::ResolvedModel, ArtemisError> {
    router::ModelRouter::new().resolve(model, None)
}
```

The `ModelRouter::resolve()` already exists and works. Just wrap it.

- [ ] **Step 2: Add resolve test**

```rust
#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn test_resolve_sonnet_alias() {
        let result = resolve("sonnet");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.canonical_id, "claude-sonnet-4-6");
    }

    #[test]
    fn test_resolve_gpt4o() {
        let result = resolve("gpt-4o");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.api_protocol, catalog::ApiProtocol::OpenAiChat);
    }

    #[test]
    fn test_resolve_nonexistent() {
        let result = resolve("nonexistent-model-xyz-12345");
        assert!(result.is_err());
    }
}
```

- [ ] **Step 3: Run tests and commit**

Run: `cargo test -p artemis-core resolve_tests`
Expected: PASS

```bash
git add artemis-core/src/lib.rs
git commit -m "feat: implement resolve() in artemis-core public API"
```

---

## Task 5: Implement chat() and chat_complete() in Core

**Files:**
- Modify: `artemis-core/src/lib.rs` (replace stubs)
- Modify: `artemis-core/src/provider.rs` (expose shared client)

- [ ] **Step 1: Add chat() and chat_complete() implementation**

The core needs to dispatch to the right transport based on ResolvedModel.api_protocol. The existing TransportDispatcher handles this. Wire it up:

```rust
// /home/astrin/artemis/artemis-core/src/lib.rs — add: use statements
use provider::{ChatRequest, ChatResponse, Provider};
use std::pin::Pin;
use futures::stream::StreamExt;

pub async fn chat(
    resolved: &catalog::ResolvedModel,
    messages: &[types::Message],
) -> Result<Pin<Box<dyn Stream<Item = streaming::StreamEvent> + Send>>, ArtemisError> {
    let request = ChatRequest::new(messages.to_vec(), vec![], resolved.clone());
    let dispatcher = transport::TransportDispatcher::new();
    let transport = dispatcher.dispatch_for_resolved(resolved)
        .ok_or_else(|| ArtemisError::Config {
            message: format!("no transport for protocol {:?}", resolved.api_protocol),
        })?;
    transport.chat_stream(request).await
}
```

Note: This requires `TransportDispatcher::dispatch_for_resolved` to be public. Check current visibility.

- [ ] **Step 2: Check TransportDispatcher visibility and fix**

Read `transport/dispatcher.rs`, verify `TransportDispatcher::new()`, `dispatch_for_resolved()`, and the `Transport` trait's `chat_stream` method are public.

- [ ] **Step 3: Run tests and fix compilation errors**

Run: `cargo test -p artemis-core 2>&1 | tail -30`
Fix any issues. Known likely issues:
- TransportDispatcher may need `Box<dyn Transport>` return type adjustments
- `chat_stream` may not exist on Transport yet — may need to add or adapt

- [ ] **Step 4: Commit**

```bash
git add artemis-core/
git commit -m "feat: implement chat() and chat_complete() in artemis-core"
```

---

## Task 6: Create artemis-agent Crate

**Files:**
- Create: `artemis-agent/Cargo.toml`
- Create: `artemis-agent/src/lib.rs`
- Create: `artemis-agent/src/loop_.rs` (from old agent_loop.rs)
- Create: `artemis-agent/src/state.rs` (conversation state, from old engine.rs)
- Create: `artemis-agent/src/tool_boundary.rs` (from old tool_boundary.rs)

- [ ] **Step 1: Create crate scaffold**

```bash
mkdir -p artemis-agent/src
```

- [ ] **Step 2: Write Cargo.toml**

```toml
# /home/astrin/artemis/artemis-agent/Cargo.toml
[package]
name = "artemis-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
artemis-core = { path = "../artemis-core" }
artemis-memory = { path = "../artemis-memory" }
artemis-token-pool = { path = "../artemis-token-pool" }
tokio = { version = "1", features = ["full"] }
```

- [ ] **Step 3: Move agent_loop.rs content to loop_.rs**

Recover the old agent_loop.rs from git history and adapt:

```bash
git show HEAD~3:artemis-core/src/agent_loop.rs > artemis-agent/src/loop_.rs
```

```rust
// artemis-agent/src/loop_.rs
use artemis_core::catalog::ResolvedModel;
use artemis_core::errors::{ArtemisError, ErrorClassifier};
use artemis_core::provider::{ChatRequest, Provider};
use artemis_core::retry::RetryPolicy;
use artemis_core::tokens::TokenEstimator;
use artemis_core::types::{Message, Role, ToolCall, ToolDefinition};

// ... rest of AgentLoop code
```

- [ ] **Step 4: Create state.rs for conversation management**

```rust
// artemis-agent/src/state.rs
use artemis_core::types::Message;

/// Holds the conversation history and resolved model for an agent session.
pub struct AgentState {
    pub messages: Vec<Message>,
    pub resolved: artemis_core::catalog::ResolvedModel,
}

impl AgentState {
    pub fn new(resolved: artemis_core::catalog::ResolvedModel) -> Self {
        Self {
            messages: vec![],
            resolved,
        }
    }
}
```

- [ ] **Step 5: Create tool_boundary.rs**

Move the old `tool_boundary.rs` content with updated imports:

```rust
// artemis-agent/src/tool_boundary.rs
use artemis_core::types::ToolCall;

#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolCallResult {
    pub call_id: String,
    pub result: String,
}

pub type ToolResult = (String, String);
```

- [ ] **Step 6: Write agent lib.rs with Agent struct**

```rust
// artemis-agent/src/lib.rs
pub mod loop_;
pub mod state;
pub mod tool_boundary;

use artemis_core::catalog::ResolvedModel;
use artemis_core::retry::RetryPolicy;

pub struct Agent {
    pub resolved: ResolvedModel,
    state: state::AgentState,
    retry: RetryPolicy,
    memory: Option<Box<dyn artemis_memory::Memory>>,
    token_pool: Option<Box<dyn artemis_token_pool::TokenPool>>,
    runtime: tokio::runtime::Runtime,
}

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self {
        Self {
            resolved: resolved.clone(),
            state: state::AgentState::new(resolved),
            retry: RetryPolicy::default(),
            memory: None,
            token_pool: None,
            runtime: tokio::runtime::Runtime::new()
                .expect("Failed to create tokio runtime"),
        }
    }

    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    pub fn with_memory(mut self, memory: Box<dyn artemis_memory::Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_token_pool(mut self, pool: Box<dyn artemis_token_pool::TokenPool>) -> Self {
        self.token_pool = Some(pool);
        self
    }
}
```

- [ ] **Step 7: Add to workspace and verify build**

Edit `Cargo.toml` workspace members, add `"artemis-agent"`.
Run: `cargo build -p artemis-agent`
Expected: compiles (may have import issues from moved agent_loop code — fix iteratively)

- [ ] **Step 8: Commit**

```bash
git add artemis-agent/ Cargo.toml
git commit -m "feat: add artemis-agent crate (AgentLoop + conversation state + tool boundary)"
```

---

## Task 7: Create artemis-python Crate

**Files:**
- Create: `artemis-python/Cargo.toml`
- Create: `artemis-python/src/lib.rs`
- Create: `artemis-python/src/engine.rs` (PyO3 classes from old engine.rs)
- Create: `artemis-python/src/streaming_bridge.rs` (from old streaming_bridge.rs)

- [ ] **Step 1: Create crate scaffold**

```bash
mkdir -p artemis-python/src
```

- [ ] **Step 2: Write Cargo.toml**

```toml
# /home/astrin/artemis/artemis-python/Cargo.toml
[package]
name = "artemis-core"  # keep original pip package name
version = "0.1.0"
edition = "2021"

[lib]
name = "artemis_core"
crate-type = ["cdylib", "rlib"]

[dependencies]
artemis-core = { path = "../artemis-core", package = "artemis-core" }
artemis-agent = { path = "../artemis-agent" }
artemis-memory = { path = "../artemis-memory" }
artemis-token-pool = { path = "../artemis-token-pool" }
pyo3 = { version = "0.28", features = ["extension-module"] }

[dev-dependencies]
pyo3 = { version = "0.28", features = ["auto-initialize"] }
```

Note: the Python crate keeps the name `artemis-core` for pip backward compatibility. The workspace member name is `artemis-python` but the crate name exposed to pip is `artemis-core`.

- [ ] **Step 3: Write lib.rs with PyO3 module**

```rust
// artemis-python/src/lib.rs
#![allow(deprecated)]
mod engine;
mod streaming_bridge;

use pyo3::prelude::*;

#[pymodule]
fn artemis_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.1.0")?;

    // ── Register exception hierarchy ──
    m.add("ArtemisError", m.py().get_type::<artemis_core::errors::py_exc::ArtemisError>())?;
    // ... (copy from old lib.rs, adapting imports)

    // ── Register types ──
    m.add_class::<engine::ArtemisEngine>()?;
    m.add_class::<engine::Event>()?;
    m.add_class::<engine::ToolCallInfo>()?;
    m.add_class::<engine::PyResolvedModel>()?;
    m.add_class::<streaming_bridge::StreamIterator>()?;

    Ok(())
}
```

Wait — this won't work because the error types were defined with `create_exception!` macro in the old `errors.rs` which needs PyO3. After the split, `artemis-core` (no PyO3) won't have exception classes.

**Fix**: The exception classes must be defined in `artemis-python/src/errors.rs` instead. The Rust `ArtemisError` enum stays in core. The PyO3 exception wrappers move to the python crate.

- [ ] **Step 4: Create errors.rs in artemis-python**

```rust
// artemis-python/src/errors.rs
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use artemis_core::errors::ArtemisError as CoreError;

create_exception!(artemis_core, ArtemisError, PyException);
create_exception!(artemis_core, RateLimitError, ArtemisError);
create_exception!(artemis_core, AuthenticationError, ArtemisError);
create_exception!(artemis_core, ModelNotFoundError, ArtemisError);
create_exception!(artemis_core, ProviderUnavailableError, ArtemisError);
create_exception!(artemis_core, ContextWindowExceededError, ArtemisError);
create_exception!(artemis_core, ToolExecutionError, ArtemisError);
create_exception!(artemis_core, StreamingError, ArtemisError);
create_exception!(artemis_core, ConfigError, ArtemisError);
create_exception!(artemis_core, NetworkError, ArtemisError);

impl From<CoreError> for PyErr {
    fn from(err: CoreError) -> PyErr {
        match err {
            CoreError::RateLimit { retry_after, provider } => {
                // ... (copy conversion logic from old errors.rs, adapting type names)
            }
            // ... remaining variants
        }
    }
}
```

The existing `From<ArtemisError> for PyErr` implementation in `artemis-core/src/errors.rs` must be removed (it depends on PyO3). Move the entire conversion block to `artemis-python/src/errors.rs`.

- [ ] **Step 5: Move streaming_bridge.rs**

Copy old `streaming_bridge.rs` content. Update imports to use artemis_core types.

- [ ] **Step 6: Move engine.rs PyO3 classes**

Stub the Python classes:

```rust
// artemis-python/src/engine.rs
use pyo3::prelude::*;
use artemis_core::catalog::ResolvedModel;

#[pyclass]
pub struct ArtemisEngine {
    router: artemis_core::router::ModelRouter,
    // ... minimal state
}

#[pymethods]
impl ArtemisEngine {
    #[new]
    pub fn new() -> Self {
        Self {
            router: artemis_core::router::ModelRouter::new(),
        }
    }

    pub fn resolve_model(&self, model: &str) -> PyResult<PyResolvedModel> {
        let resolved = self.router.resolve(model, None)
            .map_err(|e| PyErr::from(e))?;
        Ok(PyResolvedModel { inner: resolved })
    }
}
// ... PyResolvedModel, Event, ToolCallInfo stubs
```

- [ ] **Step 7: Add to workspace and verify build**

Edit `Cargo.toml` workspace members, replace `"artemis-core"` with `"artemis-python"` (not removing core — it stays as a dependency). Members should be:
```toml
members = [
    "artemis-core",
    "artemis-memory",
    "artemis-token-pool",
    "artemis-agent",
    "artemis-python",
]
```

Run: `cargo build -p artemis-python`
Expected: compiles (may have import issues — fix iteratively)

- [ ] **Step 8: Commit**

```bash
git add artemis-python/ Cargo.toml artemis-core/ # core for removing py_exc module
git commit -m "feat: add artemis-python crate (PyO3 bindings)"
```

---

## Task 8: Final Integration and Test Split

**Files:**
- Move: affected test files to correct crates
- Modify: `artemis-core/Cargo.toml` (clean up remaining pyo3 references in test cfg)

- [ ] **Step 1: Split test files across crates**

Tests that test core functionality (catalog, router, transport, streaming, retry, errors, tokens) stay in `artemis-core/tests/`.
Tests that test agent_loop, conversation state, tool boundary go to `artemis-agent/tests/`.
Tests that test Python bindings go to `artemis-python/tests/`.

Recover moved test files from `/tmp/artemis-moved-tests/` and place them in the correct crate.

- [ ] **Step 2: Verify entire workspace builds and tests pass**

Run: `cargo build` from repo root
Expected: all crates compile

Run: `cargo test --no-default-features` from repo root
Expected: core tests pass (no Python runtime needed)

Run: `cargo test` from repo root
Expected: all tests pass (Python runtime needed for python crate tests)

- [ ] **Step 3: Final commit**

```bash
git add -A
git commit -m "refactor: complete kernel separation — five crates with clean dependencies"
```

---

## Task 9: Cleanup — Remove Dead Code and Unused Dependencies

- [ ] **Step 1: Verify no leftover dead code**

```bash
grep -rn "uuid\|chrono\|rig-core\|pyo3-async-runtimes" */Cargo.toml
```

None should remain.

- [ ] **Step 2: Verify artemis-core Cargo.toml has no PyO3**

```bash
grep pyo3 artemis-core/Cargo.toml
```

Should return nothing.

- [ ] **Step 3: Run full CI-equivalent checks**

```bash
cargo test --no-default-features
cargo clippy --no-default-features -- -D warnings
cargo fmt --check
```

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: remove unused dependencies and dead code post-separation"
```

---

## Verification Checklist

After all tasks complete, verify:

- [ ] `cargo build` from repo root succeeds
- [ ] `cargo test --no-default-features` passes (core only, no Python)
- [ ] `cargo test` passes (all crates, including Python)
- [ ] `cargo clippy --no-default-features -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] Artifact sizes: `ls -lh target/debug/libartemis_core.rlib` (should be smaller, no PyO3)
- [ ] Python smoke test: `maturin develop -m artemis-python/Cargo.toml && python -c "import artemis_core; e = artemis_core.ArtemisEngine(); print(e.resolve_model('sonnet'))"`
