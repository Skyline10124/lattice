---
title: Cross-Entry Consistency Fixes
date: 2026-05-03
status: approved
priority: P0/P1 (5 items)
strategy: 压强原则 — 3 core change points (router.rs, pipeline.rs, app.rs)
---

# Cross-Entry Consistency Fixes — Design Spec

## Problem Summary

5 items from post-alpha-fix audit where "cross-entry consistency" and "config semantics closure"
remain incomplete. Each has partial fix but core gap persists.

| # | Issue | Crate | Core Gap |
|---|-------|-------|----------|
| 1 | resolve_permissive missing credential | core/router | Missing→Ok, empty base_url→Ok |
| 2 | pipeline credential injection | harness/pipeline | env-only resolve, no CredentialStore |
| 3 | AgentFactory unified assembly | harness/pipeline | build_runner() private, MicroAgent unwired |
| 4 | command allowlist token-prefix | agent/sandbox | "cargo test" degrades to all "cargo" |
| 5 | TUI false multi-turn | tui/app | new Agent each submit, no history |

## Strategy: 压强原则 (Pressure Principle)

3 core change points, each penetrating multiple items:

1. **router.rs** — resolve semantics closure + inspect_model() (items 1, 2, 5)
2. **pipeline.rs** — AgentFactory extraction + credential injection (items 2, 3)
3. **app.rs** — long-lived Agent (item 5)

Other files (micro_agent.rs, runner.rs, sandbox.rs, cli) only adapt call sites.

---

## Section 1: lattice-core/src/router.rs — Resolve Semantics Closure

### Current Problems

- `resolve()` priority iteration silently skips Missing-credential providers
- `resolve_permissive()` returns models with `credential_status == Missing`
- `resolve_base_url()` can return empty string; `validate_base_url("")` returns Ok
- Error messages when all providers Missing lack actionable detail

### Changes

#### resolve() main path

Behavior unchanged for priority iteration (skip Missing → try next provider).

**Improvement**: When ALL providers are Missing, error message now lists:
- Each attempted provider name
- The credential env var name for each
- Hint: "set one of these env vars or use with_credentials()"

#### resolve_permissive() → inspect_model()

- Rename `resolve_permissive()` to `inspect_model()`
- `inspect_model()` allows returning `Missing` credential and empty base_url
- `resolve()` no longer falls through to `resolve_permissive()` for unknown models
  - Unknown model → `ModelNotFound` error (not a permissive Ok)
  - If caller wants permissive behavior, use `inspect_model()` explicitly

#### validate_base_url() empty rejection

- `resolve()` rejects empty base_url → `Err(ConfigError("empty base_url for provider ..."))`
  - Exception: credentialless providers (local/self-hosted with no API endpoint) skip this check
- `inspect_model()` does NOT reject empty base_url (it's for debugging, shows what the catalog has)
- Implementation: `validate_base_url()` unchanged (still allows empty). The rejection is done
  explicitly in `resolve()` after calling `validate_base_url()`:
  ```rust
  validate_base_url(&base_url)?;
  if base_url.is_empty() && !is_credentialless(&entry) {
      return Err(ConfigError("empty base_url"));
  }
  ```
  `inspect_model()` skips the empty-base_url check.

#### Public API Changes

```rust
// Existing — unchanged behavior for catalog/alias/override paths
pub fn resolve(&self, model_name: &str, provider_override: Option<&str>) -> Result<ResolvedModel, LatticeError>

// New — allows Missing credentials, empty base_url, and permissive provider/model format
pub fn inspect_model(&self, model_name: &str) -> Result<ResolvedModel, LatticeError>

// Existing global function — unchanged
pub fn resolve(model: &str) -> Result<ResolvedModel, LatticeError>
```

#### Call Site Adaptations

- `lattice-cli doctor` — switch from `resolve()` to `inspect_model()` (doctor shows Missing models)
- `lattice-cli resolve/models` — keep `resolve()` (only shows callable models)
- `lattice-harness pipeline` — uses `resolve()` via `ModelRouter::with_credentials()`
- `lattice-tui` — uses `resolve()` (Missing → error displayed to user)

---

## Section 2: lattice-harness — AgentFactory + Credential Injection

### AgentFactory Extraction

**New module**: `lattice-harness/src/assembly.rs`

```rust
pub fn assemble_agent(
    profile: &AgentProfile,
    resolved: ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
) -> AgentRunner {
    let mut agent = Agent::new(resolved);

    // System prompt from profile
    if let Some(prompt) = profile.system_prompt() {
        agent.set_system_prompt(&prompt);
    }

    // Tool definitions filtered by profile.tools.enabled
    let enabled_tools = filter_tools_by_profile(&profile);
    if !enabled_tools.is_empty() {
        agent.with_tools(enabled_tools);
        let executor = DefaultToolExecutor::new(".");
        agent.with_tool_executor(Box::new(executor));
    }

    // Memory attachment
    if let Some(ref mem) = memory {
        agent.with_memory(mem.clone_box());
    }

    let runner = AgentRunner::from_profile(profile.clone(), agent);

    if let Some(ref mem) = memory {
        runner.with_memory(Arc::clone(mem));
    }

    runner
}
```

**pipeline.rs changes**:
- Remove private `build_runner()` function
- Replace calls with `assembly::assemble_agent()`
- Fork branches also use `assembly::assemble_agent()`

**micro_agent.rs changes**:
- `MicroAgent::spawn()` calls `assembly::assemble_agent()` to get a fully-wired `AgentRunner`
- Extracts the inner `Agent` via `runner.agent()` accessor (new pub method on AgentRunner)
  for bus registration and the micro_agent_loop
- `micro_agent_loop()` uses `AgentRunner::run_once()` instead of `agent.run()` directly
  (gets JSON schema validation, error detection, shared memory enrichment)

### Credential Injection

**Pipeline struct changes**:

```rust
pub struct Pipeline {
    // ... existing fields ...
    credentials: Option<HashMap<String, String>>,  // NEW
}
```

**Pipeline::new() changes**:
- Accept `Option<HashMap<String, String>>` parameter for credentials
- If provided, store in `self.credentials`

**resolve calls in pipeline**:
- `run_async()` and `run_fork_async()` replace `lattice_core::resolve()` with:

```rust
let router = ModelRouter::new();
let router = match &self.credentials {
    Some(creds) => router.with_credentials(creds.clone()),
    None => router,
};
let resolved = router.resolve(&profile.agent.model, None)?;
```

**CLI run command**:
- Pass `credential_store.to_hashmap()` to `Pipeline::new()`

**lib.rs global resolve function**:
- `lattice_core::resolve()` remains unchanged (env-only, for simple callers)
- New callers who need credentials use `ModelRouter::with_credentials()` directly

---

## Section 3: lattice-agent/src/sandbox.rs — Command Allowlist Token-Prefix

### Current Problem

`check_command()` compares only the first token (program name) of both command and allowlist entry.
Multi-word entries like `"cargo test"` effectively allow all `"cargo"` subcommands.

### Changes

**New matching function**:

```rust
fn command_matches_allowlist(cmd: &str, entry: &str) -> bool {
    let cmd_tokens: Vec<&str> = cmd.split_whitespace().take(4).collect();
    let entry_tokens: Vec<&str> = entry.split_whitespace().collect();

    if cmd_tokens.is_empty() { return false; }

    // Single-word entry: match program name only
    // "grep" matches "grep . /etc/passwd"
    if entry_tokens.len() == 1 {
        return cmd_tokens[0] == entry_tokens[0];
    }

    // Multi-word entry: match program + subcommand prefix
    // "cargo test" matches "cargo test -p core" but NOT "cargo run"
    if cmd_tokens.len() < entry_tokens.len() {
        return false;
    }

    for i in 0..entry_tokens.len() {
        if cmd_tokens[i] != entry_tokens[i] { return false; }
    }
    true
}
```

**check_command() adaptation**:
- Replace the current `entry.split_whitespace().next() == Some(program)` logic
- Use `command_matches_allowlist(cmd, entry)` for each allowlist entry

**Effect**:
| Command | Allowlist Entry | Match |
|---------|-----------------|-------|
| `cargo test -p core` | `cargo test` | YES |
| `cargo build --release` | `cargo build` | YES |
| `cargo run` | any | NO |
| `cargo install` | any | NO |
| `grep . /etc/passwd` | `grep` | YES |
| `ls -la` | `ls` | YES |

**Default allowlist unchanged** — entries remain: `cargo test`, `cargo clippy`, `cargo build`, `cargo fmt`, `grep`, `find`, `ls`, `ps`

---

## Section 4: lattice-tui/src/app.rs — Long-Lived Agent

### Current Problem

`submit()` creates `Agent::new(resolved)` every call, passes only current `text` to model.
`self.messages` is display-only; no conversation history reaches the model.

### Changes

**App struct**:

```rust
pub struct App {
    pub agent: Option<Agent>,  // NEW: long-lived agent
    // ... existing fields unchanged ...
}
```

**submit() changes**:

```rust
fn submit(&mut self) {
    let text = self.input.trim().to_string();
    if text.is_empty() { return; }

    self.messages.push(ChatMessage { role: Role::User, content: text.clone(), .. });
    self.status = AppStatus::Streaming;

    // Rebuild agent if none exists or model changed
    let needs_rebuild = self.agent.is_none()
        || self.current_model != self.previous_model;

    if needs_rebuild {
        let resolved = lattice_core::resolve(&self.current_model)?;
        let mut agent = Agent::new(resolved)
            .with_tools(default_tool_definitions());
        agent.set_system_prompt("You are a helpful assistant.");
        agent.with_tool_executor(Box::new(DefaultToolExecutor::new(".")));
        self.agent = Some(agent);
        self.previous_model = self.current_model.clone();
    }

    // Send message to existing agent — conversation history is inside Agent
    let mut events = self.agent.unwrap().send_message(&text).await;
    // ... process events as before ...
}
```

**New App field**: `previous_model: String` — tracks last-used model for rebuild decision.

**Model switch handling**:
- User changes `current_model` → Agent rebuilt on next submit
- Old Agent dropped → conversation history cleared (fresh start with new model)
- This is intentional: changing model mid-conversation would produce inconsistent context

**Lifecycle**:
- `App::new()` → `self.agent = None`
- First submit → creates Agent
- App drop / quit → Agent auto-dropped
- Panic → TerminalGuard still restores terminal; Agent dropped by Rust ownership

---

## File Change Summary

| File | Change Type | Items Affected |
|------|------------|----------------|
| lattice-core/src/router.rs | Major: resolve semantics, inspect_model(), validate_base_url | 1 |
| lattice-core/src/lib.rs | Minor: add inspect_model() global function | 1 |
| lattice-harness/src/assembly.rs | New: AgentFactory public function | 3 |
| lattice-harness/src/pipeline.rs | Medium: remove build_runner(), add credentials field, use assembly | 2, 3 |
| lattice-harness/src/micro_agent.rs | Medium: use assemble_agent(), run_once() | 3 |
| lattice-agent/src/sandbox.rs | Small: command_matches_allowlist() | 4 |
| lattice-tui/src/app.rs | Medium: Option<Agent>, submit() rebuild logic | 5 |
| lattice-cli/src/main.rs | Minor: pass credentials to Pipeline, doctor uses inspect_model | 2, 1 |

**Total**: 8 files, ~1 new file, 7 modifications

---

## Testing Plan

### router.rs tests
- `resolve()` with Missing credentials → Err with actionable hint
- `resolve()` with credentialless provider → Ok (no credential needed)
- `inspect_model()` with Missing credentials → Ok (credential_status = Missing)
- `inspect_model()` with empty base_url → Ok (base_url = "")
- `resolve()` with empty base_url → Err

### assembly.rs tests
- `assemble_agent()` creates AgentRunner with system prompt, tools, executor
- `assemble_agent()` with empty tools.enabled → no tools attached
- `assemble_agent()` with memory → both agent and runner have memory

### sandbox.rs tests
- `cargo test -p core` matches `cargo test` allowlist entry
- `cargo run` does NOT match any allowlist entry
- `grep . file` matches `grep` single-word entry

### app.rs tests
- First submit creates Agent
- Second submit uses same Agent (no rebuild)
- Model switch triggers rebuild
- Agent drop on App quit

### Integration
- `cargo test --workspace` must still pass 623+ tests
- `cargo clippy -- -D warnings` must pass