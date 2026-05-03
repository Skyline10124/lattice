# Cross-Entry Consistency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close 5 cross-entry consistency gaps: resolve semantics, credential injection, AgentFactory, command allowlist, TUI multi-turn.

**Architecture:** 3 core change points (router.rs, pipeline.rs/assembly.rs, app.rs). Other files only adapt call sites. TDD per task, commit after each task.

**Tech Stack:** Rust workspace (8 crates), tokio async, ratatui TUI, reqwest HTTP

---

## File Structure

| File | Responsibility | Status |
|------|---------------|--------|
| `lattice-core/src/router.rs` | resolve semantics, inspect_model(), empty base_url rejection | Modify |
| `lattice-core/src/lib.rs` | inspect_model() global function re-export | Modify |
| `lattice-harness/src/assembly.rs` | assemble_agent() public AgentFactory function | **Create** |
| `lattice-harness/src/lib.rs` | assembly module declaration + re-export | Modify |
| `lattice-harness/src/runner.rs` | add `pub fn agent()` accessor to AgentRunner | Modify |
| `lattice-harness/src/pipeline.rs` | remove build_runner(), add credentials field, use assembly | Modify |
| `lattice-harness/src/micro_agent.rs` | use assemble_agent() instead of Agent::new() | Modify |
| `lattice-agent/src/sandbox.rs` | command_matches_allowlist() token-prefix matching | Modify |
| `lattice-tui/src/app.rs` | Option<Agent> long-lived agent, previous_model field | Modify |
| `lattice-cli/src/main.rs` | pass credentials to Pipeline, doctor uses inspect_model | Modify |

---

### Task 1: Resolve Semantics — inspect_model() + empty base_url rejection

**Files:**
- Modify: `lattice-core/src/router.rs:141,154,489-526,627-629`
- Modify: `lattice-core/src/lib.rs:1-29`
- Test: `lattice-core/src/router.rs` (existing test module)

- [ ] **Step 1: Write failing test for inspect_model()**

In `lattice-core/src/router.rs`, add to the test module:

```rust
#[test]
fn test_inspect_model_returns_missing_credential() {
    let router = ModelRouter::new();
    // "openai/gpt-5-turbo" — not in catalog, but provider defaults exist
    // Even with Missing credentials, inspect_model should return Ok
    let result = router.inspect_model("openai/gpt-5-turbo");
    assert!(result.is_ok(), "inspect_model should return Ok even with Missing credentials");
    let model = result.unwrap();
    assert_eq!(model.credential_status, CredentialStatus::Missing);
}

#[test]
fn test_resolve_rejects_unknown_model() {
    let router = ModelRouter::new();
    // "unknown-provider/some-model" — not in catalog, no provider defaults
    let result = router.resolve("unknown-provider/some-model", None);
    assert!(result.is_err(), "resolve should not fall through to permissive for unknown models");
}

#[test]
fn test_resolve_rejects_empty_base_url_for_credentialed_provider() {
    // This test requires a provider entry with empty base_url and required credentials.
    // We test via validate_base_url rejection in the resolve path.
    // Empty base_url + credential required → Err
    let url = "";
    // validate_base_url still allows empty, but resolve() rejects it
    // We verify by checking that resolve() for a model whose resolved base_url is empty
    // returns an error (unless credentialless).
    // This will be verified after implementation.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo test -p lattice-core test_inspect_model_returns_missing_credential -- --nocapture 2>&1 | tail -5`
Expected: FAIL — `inspect_model` method doesn't exist yet

- [ ] **Step 3: Rename resolve_permissive to inspect_model**

In `lattice-core/src/router.rs`, rename the function:

```rust
// Line 485-526: Change function name and doc comment
/// Inspect a model without requiring it to be callable.
///
/// Allows returning models with Missing credentials and empty base_url.
/// Useful for `doctor`, `debug`, and diagnostic commands that need to
/// show what the catalog knows even if the model can't be called.
pub fn inspect_model(&self, model_name: &str) -> Result<ResolvedModel, LatticeError> {
    // Body unchanged — still returns Ok even with Missing credential_status
    if let Some((provider_part, model_part)) = model_name.split_once('/') {
        let provider_lower = provider_part.to_lowercase();
        let model_lower = model_part.to_lowercase();
        let defaults = self.catalog.get_provider_defaults(&provider_lower);
        if let Some(defaults) = defaults {
            let temp_entry = CatalogProviderEntry {
                provider_id: provider_lower.clone(),
                api_model_id: model_lower.clone(),
                priority: 1,
                credential_keys: defaults.credential_keys.clone(),
                base_url: Some(defaults.base_url.clone()),
                api_protocol: defaults.api_protocol.clone(),
                provider_specific: HashMap::new(),
            };
            let api_key = self.resolve_credentials(&temp_entry);
            let credential_status = Self::credential_status_from_key(&api_key, &temp_entry);

            let model = ResolvedModel {
                canonical_id: model_lower.clone(),
                provider: provider_lower,
                api_key,
                base_url: defaults.base_url.clone(),
                api_protocol: defaults.api_protocol.clone(),
                api_model_id: model_lower,
                context_length: 0,
                provider_specific: HashMap::new(),
                credential_status,
            };
            validate_base_url(&model.base_url)?;
            return Ok(model);
        }
    }

    Err(LatticeError::ModelNotFound {
        model: model_name.to_string(),
    })
}
```

- [ ] **Step 4: Remove resolve() fallthrough to permissive**

In `lattice-core/src/router.rs`, change the two fallthrough points:

Line 141 — change:
```rust
return self.resolve_permissive(model_name);
```
to:
```rust
return Err(LatticeError::ModelNotFound {
    model: model_name.to_string(),
});
```

Line 154 — same change:
```rust
None => return self.resolve_permissive(model_name);
```
to:
```rust
None => return Err(LatticeError::ModelNotFound {
    model: model_name.to_string(),
});
```

- [ ] **Step 5: Add empty base_url rejection in resolve()**

In `lattice-core/src/router.rs`, after each `validate_base_url(&model.base_url)?;` call in `resolve()` (the priority iteration path, line ~259 and the override path), add:

```rust
// Reject empty base_url for providers that require credentials
if model.base_url.is_empty() && credential_status != CredentialStatus::NotRequired {
    return Err(LatticeError::Config {
        message: format!(
            "Model '{}' resolved with empty base_url for provider '{}' (credential required). \
            Set a valid API endpoint URL in catalog defaults or config.",
            model_name, model.provider
        ),
    });
}
```

Note: The exact insertion points need to be after `validate_base_url()` calls in `resolve()`. Search for `validate_base_url(&model.base_url)` within the `resolve()` method body.

In `inspect_model()`, DO NOT add this check — inspect_model allows empty base_url.

- [ ] **Step 6: Add inspect_model() global function in lib.rs**

In `lattice-core/src/lib.rs`, add after the existing `resolve()` function (line 29):

```rust
/// Inspect a model without requiring it to be callable.
///
/// Unlike `resolve()`, this allows returning models with Missing credentials
/// and empty base_url. Useful for diagnostic commands (`doctor`, `debug`).
pub fn inspect_model(model: &str) -> Result<ResolvedModel, LatticeError> {
    ModelRouter::new().inspect_model(model)
}
```

- [ ] **Step 7: Update any internal references to resolve_permissive**

Search: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && grep -rn 'resolve_permissive' lattice-core/src/ lattice-harness/src/ lattice-cli/src/ lattice-tui/src/`
Replace any remaining `resolve_permissive` references with `inspect_model`.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo test -p lattice-core -- --nocapture 2>&1 | tail -10`
Expected: All core tests pass, including new inspect_model tests

- [ ] **Step 9: Run clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy -p lattice-core -- -D warnings 2>&1 | tail -5`
Expected: No issues found

- [ ] **Step 10: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-core/src/router.rs lattice-core/src/lib.rs
git commit -m "feat(core): resolve semantics closure — inspect_model() + empty base_url rejection

resolve() no longer falls through to permissive for unknown models.
Missing credential → Err. Empty base_url → Err (unless credentialless).
New inspect_model() allows returning non-callable models for debug/doctor."
```

---

### Task 2: Command Allowlist Token-Prefix Matching

**Files:**
- Modify: `lattice-agent/src/sandbox.rs:196-208`
- Test: existing test module in sandbox.rs

- [ ] **Step 1: Write failing test for token-prefix matching**

In `lattice-agent/src/sandbox.rs` test module, add:

```rust
#[test]
fn test_command_matches_allowlist_token_prefix() {
    let config = SandboxConfig::default(); // has "cargo test", "cargo build", "grep", etc.

    // Multi-word entries: "cargo test" allows "cargo test" + args, not "cargo run"
    assert!(config.check_command("cargo test").is_ok());
    assert!(config.check_command("cargo test -p lattice-core").is_ok());
    assert!(config.check_command("cargo build").is_ok());
    assert!(config.check_command("cargo build --release").is_ok());
    assert!(config.check_command("cargo clippy").is_ok());
    assert!(config.check_command("cargo fmt").is_ok());

    // These should be REJECTED — not matching any multi-word allowlist entry
    assert!(config.check_command("cargo run").is_err());
    assert!(config.check_command("cargo install").is_err());
    assert!(config.check_command("cargo publish").is_err());

    // Single-word entries: "grep" allows any grep command
    assert!(config.check_command("grep").is_ok());
    assert!(config.check_command("grep . file").is_ok());
    assert!(config.check_command("ls -la").is_ok());
    assert!(config.check_command("find . -name '*.rs'").is_ok());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo test -p lattice-agent test_command_matches_allowlist_token_prefix -- --nocapture 2>&1 | tail -10`
Expected: FAIL — `cargo run` currently passes (because "cargo" matches as first token)

- [ ] **Step 3: Add command_matches_allowlist() helper function**

In `lattice-agent/src/sandbox.rs`, add before `check_command()` (before line 157):

```rust
/// Check if a command matches an allowlist entry using token-prefix matching.
///
/// Single-word entries (e.g. "grep") match by program name only.
/// Multi-word entries (e.g. "cargo test") match by program + subcommand prefix,
/// so "cargo test -p core" matches "cargo test" but "cargo run" does not.
fn command_matches_allowlist(cmd: &str, entry: &str) -> bool {
    let cmd_tokens: Vec<&str> = cmd.split_whitespace().take(4).collect();
    let entry_tokens: Vec<&str> = entry.split_whitespace().collect();

    if cmd_tokens.is_empty() {
        return false;
    }

    // Single-word entry: match program name only
    if entry_tokens.len() == 1 {
        return cmd_tokens[0] == entry_tokens[0];
    }

    // Multi-word entry: match program + subcommand prefix
    if cmd_tokens.len() < entry_tokens.len() {
        return false;
    }

    for i in 0..entry_tokens.len() {
        if cmd_tokens[i] != entry_tokens[i] {
            return false;
        }
    }
    true
}
```

- [ ] **Step 4: Update check_command() to use command_matches_allowlist()**

In `lattice-agent/src/sandbox.rs`, replace lines 196-208 (the current allowlist matching logic):

```rust
// OLD (lines 196-208):
// Extract program name (first whitespace-delimited token)
let program = cmd.split_whitespace().next().unwrap_or("");
if program.is_empty() {
    return Err("Sandbox: empty command".into());
}

// Check program against allowlist — compare by first token of each entry.
// This prevents prefix-match bypasses like "cargo test; rm -rf /"
// from passing because "cargo test; rm -rf /" is not a real program name.
let allowed = self
    .command_allowlist
    .iter()
    .any(|entry| entry.split_whitespace().next() == Some(program));

if !allowed {
    let allowed_programs: Vec<&str> = self
        .command_allowlist
        .iter()
        .map(|e| e.split_whitespace().next().unwrap())
        .collect();
    return Err(format!(
        "Sandbox: program '{}' is not in allowlist. Allowed programs: {:?}",
        program, allowed_programs
    ));
}
```

Replace with:

```rust
// NEW:
if cmd.split_whitespace().next().unwrap_or("").is_empty() {
    return Err("Sandbox: empty command".into());
}

let allowed = self
    .command_allowlist
    .iter()
    .any(|entry| command_matches_allowlist(cmd, entry));

if !allowed {
    let allowed_prefixes: Vec<&str> = self
        .command_allowlist
        .iter()
        .map(|e| e.as_ref())
        .collect();
    return Err(format!(
        "Sandbox: command '{}' does not match any allowlist entry. Allowed: {:?}",
        cmd.trim(), allowed_prefixes
    ));
}
```

- [ ] **Step 5: Update existing tests that relied on old matching**

The test `test_prefix_match_injection_rejected` (around line 438-445) tested that `"cargo-inject"` doesn't match `"cargo"`. With token-prefix matching, this is still true because `"cargo-inject"` (first token) ≠ `"cargo"` (first token of single-word or `"cargo"` first token of `"cargo test"`). Verify this test still passes.

If the test reference the old `allowed_programs` format, update it to match the new `allowed_prefixes` format.

- [ ] **Step 6: Run tests**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo test -p lattice-agent -- --nocapture 2>&1 | tail -10`
Expected: All tests pass, including new token-prefix test and existing tests

- [ ] **Step 7: Run clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy -p lattice-agent -- -D warnings 2>&1 | tail -5`
Expected: No issues

- [ ] **Step 8: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-agent/src/sandbox.rs
git commit -m "feat(agent): command allowlist token-prefix matching

Single-word entries match program name. Multi-word entries match
program+subcommand prefix. 'cargo run' now rejected, 'cargo test' allowed."
```

---

### Task 3: AgentFactory — assembly.rs + runner.rs accessor

**Files:**
- Create: `lattice-harness/src/assembly.rs`
- Modify: `lattice-harness/src/lib.rs:1-31`
- Modify: `lattice-harness/src/runner.rs:17-30`
- Test: inline in assembly.rs

- [ ] **Step 1: Add agent() accessor to AgentRunner**

In `lattice-harness/src/runner.rs`, add after `from_profile()` (after line 31):

```rust
/// Access the inner Agent for bus registration or direct manipulation.
/// The Agent is still owned by the runner — callers must not move it out.
pub fn agent(&self) -> &Agent {
    &self.agent
}

pub fn agent_mut(&mut self) -> &mut Agent {
    &mut self.agent
}
```

- [ ] **Step 2: Create assembly.rs**

Create `lattice-harness/src/assembly.rs`:

```rust
//! Agent assembly from profile — the canonical construction path.
//!
//! Every entry point (Pipeline, fork, MicroAgent, CLI) should use
//! `assemble_agent()` to ensure consistent wiring of system prompt,
//! tools, executor, and memory.

use std::sync::Arc;

use lattice_agent::{Agent, DefaultToolExecutor, default_tool_definitions, memory::Memory};
use crate::runner::AgentRunner;
use crate::profile::AgentProfile;

/// Assemble an AgentRunner from a profile and resolved model.
///
/// This is the single canonical path for constructing a fully-wired agent.
/// Applies system prompt, tool definitions, tool executor, and memory
/// from the profile configuration.
pub fn assemble_agent(
    profile: &AgentProfile,
    resolved: lattice_core::ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
) -> AgentRunner {
    let mut agent = Agent::new(resolved);

    // Apply system prompt from profile
    let system_prompt = profile.system_prompt();
    agent.set_system_prompt(&system_prompt);

    // Apply tools from profile — filter default tool definitions by enabled names
    if !profile.tools.enabled.is_empty() {
        let all_tools = default_tool_definitions();
        let enabled_tools: Vec<_> = all_tools
            .into_iter()
            .filter(|td| profile.tools.enabled.contains(&td.name))
            .collect();
        if enabled_tools.is_empty() {
            tracing::warn!(
                "Agent '{}' has tools enabled but no matching tool definitions found",
                profile.agent.name
            );
        } else {
            agent = agent.with_tools(enabled_tools);
            let executor = DefaultToolExecutor::new(".");
            agent = agent.with_tool_executor(Box::new(executor));
        }
    }

    if let Some(ref mem) = memory {
        agent = agent.with_memory(mem.clone_box());
    }

    let mut runner = AgentRunner::from_profile(profile.clone(), agent);
    if let Some(ref mem) = memory {
        runner = runner.with_memory(Arc::clone(mem));
    }
    runner
}
```

- [ ] **Step 3: Register assembly module in lib.rs**

In `lattice-harness/src/lib.rs`, add module declaration after line 12:

```rust
pub mod assembly;
```

And add re-export after the existing re-exports (after line 27):

```rust
pub use assembly::assemble_agent;
```

- [ ] **Step 4: Run build to verify compilation**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo build -p lattice-harness 2>&1 | tail -5`
Expected: Build succeeds

- [ ] **Step 5: Run clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy -p lattice-harness -- -D warnings 2>&1 | tail -5`
Expected: No issues

- [ ] **Step 6: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-harness/src/assembly.rs lattice-harness/src/lib.rs lattice-harness/src/runner.rs
git commit -m "feat(harness): AgentFactory — assembly.rs + runner agent() accessor

Extract build_runner() logic to public assemble_agent() function.
Add agent()/agent_mut() accessors to AgentRunner for bus integration."
```

---

### Task 4: Pipeline — remove build_runner, add credentials, use assembly

**Files:**
- Modify: `lattice-harness/src/pipeline.rs:21-60,104-164,223,440`

- [ ] **Step 1: Remove build_runner() and replace with assembly call**

In `lattice-harness/src/pipeline.rs`, delete the private `build_runner()` function (lines 21-60).

Add the import at the top of the file:

```rust
use crate::assembly::assemble_agent;
```

Replace all calls to `build_runner()` with `assemble_agent()`:

- In `run_async()`: `build_runner(&profile, resolved, memory_box.clone())` → `assemble_agent(&profile, resolved, memory_box.clone())`
- In `run_fork_async()`: same replacement

- [ ] **Step 2: Add credentials field to Pipeline struct**

In `lattice-harness/src/pipeline.rs`, modify the Pipeline struct (lines 104-119):

```rust
pub struct Pipeline {
    pub name: String,
    pub registry: Arc<AgentRegistry>,
    pub shared_memory: Option<Arc<dyn Memory>>,
    pub event_bus: Option<Arc<EventBus>>,
    pub plugin_registry: Option<Arc<lattice_plugin::registry::PluginRegistry>>,
    pub tool_registry: Option<Arc<crate::tools::ToolRegistry>>,
    /// External credentials for programmatic injection.
    /// When set, ModelRouter::with_credentials() is used instead of env-only resolve.
    pub credentials: Option<std::collections::HashMap<String, String>>,
    /// Maximum number of memory entries (default: 1000).
    pub max_memory_entries: usize,
    /// Maximum total bytes for memory content (default: 10 MB).
    pub max_memory_bytes: usize,
    /// Current count of memory entries saved in this pipeline run.
    memory_entry_count: Cell<usize>,
    /// Current total bytes of memory entry content.
    memory_entry_bytes: Cell<usize>,
}
```

- [ ] **Step 3: Update Pipeline::new()**

In `lattice-harness/src/pipeline.rs`, update `Pipeline::new()` (lines 146-164):

```rust
pub fn new(
    name: &str,
    registry: Arc<AgentRegistry>,
    memory: Option<Arc<dyn Memory>>,
    event_bus: Option<Arc<EventBus>>,
) -> Self {
    Self {
        name: name.to_string(),
        registry,
        shared_memory: memory,
        event_bus,
        plugin_registry: None,
        tool_registry: None,
        credentials: None,
        max_memory_entries: DEFAULT_MAX_MEMORY_ENTRIES,
        max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
        memory_entry_count: Cell::new(0),
        memory_entry_bytes: Cell::new(0),
    }
}

/// Set external credentials for programmatic injection.
pub fn with_credentials(mut self, creds: std::collections::HashMap<String, String>) -> Self {
    self.credentials = Some(creds);
    self
}
```

- [ ] **Step 4: Replace env-only resolve with credential-aware resolve**

In `lattice-harness/src/pipeline.rs`, replace the `lattice_core::resolve()` calls in `run_async()` (line 223) and `run_fork_async()` (line 440).

Add import:

```rust
use lattice_core::router::ModelRouter;
```

Replace:
```rust
// OLD (line 223):
let resolved = match lattice_core::resolve(&profile.agent.model) {
```

With:
```rust
// NEW:
let router = ModelRouter::new();
let router = match &self.credentials {
    Some(creds) => router.with_credentials(creds.clone()),
    None => router,
};
let resolved = match router.resolve(&profile.agent.model, None) {
```

Same replacement in `run_fork_async()` (line 440).

- [ ] **Step 5: Run tests**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo test -p lattice-harness -- --nocapture 2>&1 | tail -10`
Expected: All tests pass

- [ ] **Step 6: Run clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy -p lattice-harness -- -D warnings 2>&1 | tail -5`
Expected: No issues

- [ ] **Step 7: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-harness/src/pipeline.rs
git commit -m "feat(harness): pipeline credential injection + use assembly

Remove private build_runner(). Pipeline now uses public assemble_agent().
Add credentials field and with_credentials() builder.
Resolve uses ModelRouter::with_credentials() when available."
```

---

### Task 5: MicroAgent — use assemble_agent()

**Files:**
- Modify: `lattice-harness/src/micro_agent.rs:99-166`

- [ ] **Step 1: Replace Agent::new() with assemble_agent() in spawn()**

In `lattice-harness/src/micro_agent.rs`, add import at top:

```rust
use crate::assembly::assemble_agent;
```

Replace lines 114-120 in `MicroAgent::spawn()`:

```rust
// OLD:
let resolved = lattice_core::resolve(&self.profile.agent.model)
    .map_err(|e| BusError::Resolve(e.to_string()))?;

let mut agent = Agent::new(resolved);
if let Some(ref mem) = self.memory {
    agent = agent.with_memory(mem.clone_box());
}
```

With:

```rust
// NEW:
let resolved = lattice_core::resolve(&self.profile.agent.model)
    .map_err(|e| BusError::Resolve(e.to_string()))?;

let runner = assemble_agent(&self.profile, resolved, self.memory.clone());
let mut agent = runner.agent().clone();
```

Wait — `Agent` doesn't implement `Clone`. We need a different approach.

Instead, we use the runner directly and access the agent via the accessor for bus loop:
```rust
// NEW approach — use assemble_agent to create runner, then extract for bus loop
let resolved = lattice_core::resolve(&self.profile.agent.model)
    .map_err(|e| BusError::Resolve(e.to_string()))?;

// assemble_agent creates a fully-wired runner with system prompt, tools, executor, memory
let runner = assemble_agent(&self.profile, resolved, self.memory.clone());
```

Then in the `tokio::spawn` at line 161, instead of passing `agent` directly to `micro_agent_loop`, pass the runner:

```rust
// OLD:
let join_handle = tokio::spawn(async move {
    micro_agent_loop(agent, ctx, max_turns, request_rx).await;
});

// NEW:
let join_handle = tokio::spawn(async move {
    micro_agent_loop(runner, ctx, max_turns, request_rx).await;
});
```

- [ ] **Step 2: Update micro_agent_loop to use AgentRunner**

The `micro_agent_loop` function (line 179) currently takes `mut agent: Agent`. Change it to take `mut runner: AgentRunner`:

```rust
// OLD signature:
async fn micro_agent_loop(
    mut agent: Agent,
    ctx: AgentLoopContext,
    max_turns: u32,
    mut request_rx: tokio::sync::mpsc::Receiver<BusRequest>,
)

// NEW signature:
async fn micro_agent_loop(
    mut runner: AgentRunner,
    ctx: AgentLoopContext,
    max_turns: u32,
    mut request_rx: tokio::sync::mpsc::Receiver<BusRequest>,
)
```

In the loop body, replace `agent.run()` with `runner.run_once()`:

```rust
// OLD (line 192):
let events = agent.run(&enriched, max_turns).await;

// NEW:
let run_result = runner.run_once(&enriched, max_turns);
let content = match run_result {
    Ok(json) => json.to_string(),
    Err(e) => {
        warn!("MicroAgent '{}': run_once error: {:?}", agent_name, e);
        format!("{{\"error\": \"{}\"}}", e.to_string())
    }
};
```

Note: `run_once()` returns `Result<serde_json::Value, Box<dyn std::error::Error>>`. The content extraction changes accordingly.

- [ ] **Step 3: Run build to verify compilation**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo build -p lattice-harness 2>&1 | tail -10`
Expected: Build succeeds (may need adjustments for AgentRunner visibility in async context)

- [ ] **Step 4: Run clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy -p lattice-harness -- -D warnings 2>&1 | tail -5`

- [ ] **Step 5: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-harness/src/micro_agent.rs
git commit -m "feat(harness): MicroAgent uses assemble_agent() for full wiring

MicroAgent::spawn() now creates a fully-wired AgentRunner via assemble_agent()
instead of bare Agent::new(). Gets system prompt, tools, executor, memory.
micro_agent_loop uses AgentRunner::run_once() for JSON validation."
```

---

### Task 6: CLI — pass credentials to Pipeline, doctor uses inspect_model

**Files:**
- Modify: `lattice-cli/src/main.rs`
- Modify: `lattice-cli/src/commands/doctor.rs` (if it exists) or relevant command file

- [ ] **Step 1: Find run_pipeline() and add credential injection**

Search: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && grep -rn 'run_pipeline\|Pipeline::new' lattice-cli/src/`

In the run command handler, find where `Pipeline::new()` is called and add:

```rust
// OLD:
let pipeline = Pipeline::new(name, registry, memory, event_bus);

// NEW:
let mut pipeline = Pipeline::new(name, registry, memory, event_bus);
if let Some(creds) = credential_store_opt {
    pipeline = pipeline.with_credentials(creds);
}
```

- [ ] **Step 2: Update doctor command to use inspect_model**

Search: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && grep -rn 'doctor\|inspect' lattice-cli/src/commands/`

In the doctor command handler, replace:

```rust
// OLD:
let resolved = lattice_core::resolve(&model)?;

// NEW (for diagnostic display, show even non-callable models):
let resolved = lattice_core::inspect_model(&model)?;
```

- [ ] **Step 3: Run build and tests**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo build -p lattice-cli 2>&1 | tail -5 && cargo test -p lattice-cli -- --nocapture 2>&1 | tail -10`

- [ ] **Step 4: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-cli/src/
git commit -m "feat(cli): pipeline credential injection + doctor inspect_model

CLI run command passes CredentialStore to Pipeline via with_credentials().
Doctor command uses inspect_model() to show non-callable models."
```

---

### Task 7: TUI — Long-Lived Agent

**Files:**
- Modify: `lattice-tui/src/app.rs:48-59,69-82,169-312`

- [ ] **Step 1: Add agent and previous_model fields to App struct**

In `lattice-tui/src/app.rs`, modify the App struct (lines 48-59):

```rust
pub struct App {
    pub should_quit: bool,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub input_cursor: usize,
    pub status: AppStatus,
    pub current_model: String,
    pub previous_model: String,
    pub current_provider: String,
    pub token_count: usize,
    pub scroll_offset: usize,
    pub event_tx: Option<UnboundedSender<Event>>,
    pub agent: Option<Agent>,
}
```

- [ ] **Step 2: Update App::new()**

In `lattice-tui/src/app.rs`, update `App::new()` (lines 69-82):

```rust
pub fn new() -> Self {
    Self {
        should_quit: false,
        messages: vec![],
        input: String::new(),
        input_cursor: 0,
        status: AppStatus::Ready,
        current_model: "deepseek-v4-flash".into(),
        previous_model: String::new(),
        current_provider: "".into(),
        token_count: 0,
        scroll_offset: 0,
        event_tx: None,
        agent: None,
    }
}
```

- [ ] **Step 3: Rewrite submit() — long-lived Agent with rebuild logic**

The core change in `submit()` is to check `self.agent` and rebuild only when needed. The current submit() creates a new Agent every time inside `tokio::spawn`. We need to restructure so the Agent is held in App and only rebuilt when model changes.

The tricky part: `submit()` spawns an async task that uses the Agent. Since `Agent` can't be cloned, we need to pass a reference or use the Agent inside the spawned task. But `self` can't be shared across async tasks.

The simplest approach: move the Agent creation/rebuild OUTSIDE the spawn, then pass a mutable reference via some mechanism.

However, `tokio::spawn` requires owned values. So we need to take the Agent out of `self.agent`, pass it to the spawned task, and put it back after.

Alternative approach: use `Agent::send_message()` which borrows self. Since the agent loop is already inside a spawned task, we can extract the Agent, send it into the task, and receive it back.

The cleanest Rust approach for this pattern:

```rust
async fn submit(&mut self) -> Result<()> {
    let text = self.input.trim().to_string();
    if text.is_empty() { return Ok(()); }

    // Add user message
    self.messages.push(ChatMessage {
        role: Role::User,
        content: text.clone(),
        reasoning: None,
    });
    self.input.clear();
    self.input_cursor = 0;
    self.scroll_offset = 0;
    self.status = AppStatus::Streaming;

    // Thinking indicator
    self.messages.push(ChatMessage {
        role: Role::Assistant,
        content: String::new(),
        reasoning: None,
    });

    let tx = match self.event_tx.clone() {
        Some(tx) => tx,
        None => { self.status = AppStatus::Error("Event channel not initialized".into()); return Ok(()); }
    };

    // Rebuild agent if needed
    let needs_rebuild = self.agent.is_none()
        || self.current_model != self.previous_model;

    if needs_rebuild {
        let model = self.current_model.clone();
        let resolved = match tokio::task::spawn_blocking(move || lattice_core::resolve(&model)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => { self.status = AppStatus::Error(format!("resolve failed: {e}")); return Ok(()); }
            Err(_) => { self.status = AppStatus::Error("resolve task panicked".into()); return Ok(()); }
        };

        let _ = tx.send(Event::ModelInfo {
            model: resolved.canonical_id.clone(),
            provider: resolved.provider.clone(),
        });

        let mut agent = Agent::new(resolved).with_tools(default_tool_definitions());
        agent.set_system_prompt("You are a helpful assistant.");
        agent = agent.with_tool_executor(Box::new(DefaultToolExecutor::new(".")));
        self.agent = Some(agent);
        self.previous_model = self.current_model.clone();
    }

    // Take the agent out of self to send into spawned task
    let mut agent = self.agent.take().unwrap();
    let text_for_spawn = text.clone();

    tokio::spawn(async move {
        let mut events = agent.send_message(&text_for_spawn).await;

        // Process events as before (lines 232-308)
        // ... (forward tokens, handle tool calls, etc.)

        // When done, return the agent via a channel or store it
        // This is the tricky part — we need to put agent back into App
    });

    // Problem: we took agent out of self.agent and it's now in the spawned task.
    // We can't put it back until the task completes.
    // Solution: use a oneshot channel to receive the agent back.
```

This pattern is complex. A simpler approach: keep the current `tokio::spawn` structure but pass the **entire messages history** to the Agent inside the spawned task, without holding a long-lived Agent in App.

Wait — the spec says "long-lived Agent, Agent 内部自带历史". But `Agent::send_message()` only takes the current text. The Agent's internal state maintains conversation history across calls. So we need the same Agent instance across submits.

The real problem: App is on the main thread, Agent needs to be in the spawned async task. We can't share a mutable Agent between them.

**Practical solution**: Use an `Arc<Mutex<Agent>>` pattern, or use a channel to send commands to a long-lived Agent task.

Even simpler: don't use `tokio::spawn` for the agent loop. Instead, run the agent loop in the main async context (since TUI already has a tokio runtime). But TUI uses ratatui which needs the main thread for rendering.

**Most practical solution for TUI**: Keep `tokio::spawn` but store the Agent in the spawned task and communicate via channels. The "long-lived Agent" lives in the spawned task, not in App struct.

```rust
// App holds a channel sender to send messages to a persistent Agent task
pub struct App {
    // ... existing fields ...
    agent_tx: Option<UnboundedSender<AgentCommand>>,
    previous_model: String,
}
```

Where `AgentCommand` is:
```rust
enum AgentCommand {
    SendMessage { text: String },
    Rebuild { model: String },
}
```

But this adds significant complexity to TUI. Given that TUI is "原型" stage (score 3/10), a simpler approach is better.

**Final pragmatic approach**: Keep the current architecture but pass the full message history each time. This means `App.messages` gets converted to `Vec<Message>` and passed as conversation context. No long-lived Agent needed — we just ensure multi-turn context reaches the model.

```rust
// In submit(), build the message history and pass to agent:
let history: Vec<lattice_core::Message> = self.messages.iter()
    .map(|m| lattice_core::Message {
        role: m.role,
        content: m.content.clone(),
    })
    .collect();

// Then in spawned task, send history + current text:
// But Agent::send_message() only takes a single text string...
// Agent doesn't have a send_with_history() method.
```

This also requires adding a method to Agent. Given the spec approved "长驻 Agent", we need to find a way.

**Realistic implementation**: Use `Arc<Mutex<Agent>>` stored in App. The spawned task locks the mutex to call `send_message()`, then releases it.

```rust
pub struct App {
    // ... existing fields ...
    agent: Option<Arc<tokio::sync::Mutex<Agent>>>,
    previous_model: String,
}
```

In submit():
```rust
if needs_rebuild {
    // ... resolve and create agent ...
    self.agent = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    self.previous_model = self.current_model.clone();
}

let agent_arc = self.agent.clone().unwrap(); // Arc clone is cheap
let text = text.clone();

tokio::spawn(async move {
    let mut agent = agent_arc.lock().await;
    let mut events = agent.send_message(&text).await;
    // ... process events ...
});
```

This works! Arc<Mutex<Agent>> allows the App to hold a reference while the spawned task also accesses it. Since submit() only fires when user presses enter, there's no concurrent access risk.

- [ ] **Step 4: Import tokio::sync::Mutex and add the struct changes**

Add imports in `lattice-tui/src/app.rs`:

```rust
use std::sync::Arc;
use tokio::sync::Mutex;
```

Update App struct as shown in Step 1 (with `agent: Option<Arc<Mutex<Agent>>>` and `previous_model: String`).

Update App::new() as shown in Step 2.

- [ ] **Step 5: Rewrite submit() body**

Replace the submit() method (lines 169-312) with the new logic. The key changes:

1. Check if agent needs rebuild (`self.agent.is_none() || self.current_model != self.previous_model`)
2. If rebuild needed: resolve model, create Agent, wrap in Arc<Mutex>, store in self.agent
3. Clone the Arc<Mutex<Agent>> and pass to spawned task
4. In spawned task: lock the mutex, call `agent.send_message(&text)`, process events

The spawned task body (event processing loop) stays the same as current lines 232-308. The only change is how the Agent is obtained.

- [ ] **Step 6: Run build**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo build -p lattice-tui 2>&1 | tail -10`
Expected: Build succeeds

- [ ] **Step 7: Run clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy -p lattice-tui -- -D warnings 2>&1 | tail -5`

- [ ] **Step 8: Commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add lattice-tui/src/app.rs
git commit -m "feat(tui): long-lived Agent for true multi-turn chat

App holds Arc<Mutex<Agent>> instead of creating new Agent per submit.
Agent rebuilds only when model changes. Conversation history persists
inside Agent across turns."
```

---

### Task 8: Integration Verification

- [ ] **Step 1: Run full workspace test suite**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo test --workspace --quiet 2>&1 | tail -5`
Expected: 623+ tests pass, 0 failures

- [ ] **Step 2: Run workspace clippy**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo clippy --workspace -- -D warnings 2>&1 | tail -5`
Expected: No issues found

- [ ] **Step 3: Run fmt check**

Run: `cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency && cargo fmt --check --all 2>&1 | tail -5`
Expected: No formatting issues

- [ ] **Step 4: Final verification commit**

```bash
cd /home/astrin/lattice/.worktrees/fix/cross-entry-consistency
git add -A
git commit -m "verify: all 623+ tests pass, clippy clean, fmt clean after cross-entry consistency fixes"
```