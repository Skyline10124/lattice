---
phase: 11-deletion-budget-and-release-verification
reviewed: 2026-05-18T00:00:00Z
depth: standard
files_reviewed: 38
files_reviewed_list:
  - AGENTS.md
  - Cargo.toml
  - CLAUDE.md
  - crates/lattice-agent/Cargo.toml
  - crates/lattice-agent/src/agent_loop.rs
  - crates/lattice-agent/src/lib.rs
  - crates/lattice-api/src/lib.rs
  - crates/lattice-api/src/session.rs
  - crates/lattice-api/src/turn.rs
  - crates/lattice-cli/Cargo.toml
  - crates/lattice-cli/src/commands/doctor.rs
  - crates/lattice-cli/src/commands/mod.rs
  - crates/lattice-cli/src/commands/resolve.rs
  - crates/lattice-cli/src/commands/run.rs
  - crates/lattice-cli/src/commands/session.rs
  - crates/lattice-cli/src/config.rs
  - crates/lattice-cli/src/credentials.rs
  - crates/lattice-cli/src/lib.rs
  - crates/lattice-cli/src/main.rs
  - crates/lattice-cli/tests/cli_cutover.rs
  - crates/lattice-cli/tests/release_smoke.rs
  - crates/lattice-hooks/Cargo.toml
  - crates/lattice-hooks/src/lib.rs
  - crates/lattice-llm/Cargo.toml
  - crates/lattice-llm/src/lib.rs
  - crates/lattice-llm/src/logging.rs
  - crates/lattice-llm/src/router.rs
  - crates/lattice-profile/src/error.rs
  - crates/lattice-profile/src/lib.rs
  - crates/lattice-profile/src/load.rs
  - crates/lattice-session/src/graph.rs
  - crates/lattice-session/src/lib.rs
  - crates/lattice-session/src/sqlite_graph.rs
  - crates/lattice-tools/Cargo.toml
  - crates/lattice-tools/src/edit/apply.rs
  - crates/lattice-tools/src/edit/diagnostics.rs
  - crates/lattice-tools/src/edit/types.rs
  - crates/lattice-tools/src/executor.rs
  - crates/lattice-tools/src/lib.rs
  - crates/lattice-tools/src/tool_definitions.rs
  - crates/lattice-tools/src/tool_error.rs
  - README.md
findings:
  critical: 5
  warning: 7
  info: 3
  total: 15
fixed:
  critical_fixed: 5
  warning_fixed: 3
  info_fixed: 0
  total_fixed: 8
  unfixed: 7
status: partially_fixed
---

# Phase 11: Code Review Report

**Reviewed:** 2026-05-18T00:00:00Z
**Depth:** standard
**Files Reviewed:** 38
**Status:** issues_found

## Summary

Reviewed 38 source files across the 8-crate LATTICE Rust workspace at standard depth. The codebase is broadly well-structured with strong security validation (path traversal blocking, symlink escape detection, command injection metacharacter filtering, forbidden profile sections). However, five critical and seven warning-level issues were found that must be addressed before release.

Key concerns: silent role corruption in the agent loop, hardcoded provider metadata erasing audit trails, incomplete shell metacharacter blocking that permits environment variable expansion (enabling credential leakage), literal API key material stored in memory without isolation, and misleading empty alias lists when credentials are missing.

## Critical Issues

### CR-01: Unknown message roles silently mapped to User in convert_to_llm_message

**File:** `crates/lattice-agent/src/agent_loop.rs:475-481`
**Issue:** When a `GraphMessage` contains a role string that is not one of `"system"`, `"user"`, `"assistant"`, or `"tool"`, the `_` wildcard branch maps it to `Role::User`. This silently corrupts the conversation: a malformed or corrupted database record with an unknown role will be sent to the LLM as a user message. This can cause the model to interpret tool output or internal state as a user instruction, leading to unintended behavior (e.g., the model acting on tool error messages as if the user requested them). No warning or error is emitted.
**Fix:**
```rust
fn convert_to_llm_message(msg: &GraphMessage) -> Result<lattice_llm::Message, String> {
    let role = match msg.role.as_str() {
        "system" => lattice_llm::Role::System,
        "user" => lattice_llm::Role::User,
        "assistant" => lattice_llm::Role::Assistant,
        "tool" => lattice_llm::Role::Tool,
        other => return Err(format!("unknown message role '{}'", other)),
    };
    Ok(lattice_llm::Message { role, content: msg.content.clone(), ... })
}
```

### CR-02: Provider hardcoded as "unknown" in session commit input, erasing audit metadata

**File:** `crates/lattice-api/src/turn.rs:501`
**Issue:** The `TurnCommitInput` struct that persists turn data to the session graph always sets `provider: "unknown".to_string()`. The actual provider is resolved by the LLM router and is available in the `ResolvedModel` returned from the chat call, but it is never passed through to the commit. This means every session commit record in the database permanently records provider="unknown", destroying the audit trail. When debugging which provider handled a turn (e.g., did this go to Anthropic or OpenAI?), the data is gone.
**Fix:** Pass the resolved provider name from the `ChatResponse` or `ResolvedModel` through the agent loop result into the commit input. The `AgentTurnResult` should carry a `provider: String` field populated from the LLM response metadata, and `turn.rs:501` should use it instead of the hardcoded `"unknown"`.

### CR-03: `${VAR}` environment variable expansion not blocked in bash check_command

**File:** `crates/lattice-tools/src/executor.rs:877`
**Issue:** The `check_command` function blocks shell metacharacters like `&`, `;`, `|`, `$(`, backtick, newlines, and redirects. However, it does not block `${VAR}` dollar-sign variable expansion. The metacharacter list includes `$(` (subshell) but not plain `$` when followed by a `{` or an identifier. A prompt injection that crafts a bash command like `echo ${ANTHROPIC_API_KEY}` would pass the metacharacter check and, if executed by a shell that expands environment variables, would leak the API key into tool output that the LLM can read and repeat to the user. The PATH= and LD_PRELOAD= checks at line 871 are insufficient since `${ANTHROPIC_API_KEY}` does not contain `PATH=` or `LD_PRELOAD=`.
**Fix:** Add `"${"` and `"$"` followed by an alphabetic character to the blocked metacharacter list, or alternatively, validate that the command string contains no `$` character at all (since `$(` is already blocked, the remaining `$` patterns are all variable expansion). A simpler approach:
```rust
for meta in ["&", ";", "|", "&&", "||", "$(", "$", "`", "\n", "\r", ">", "<"] {
    if command.contains(meta) {
        return Err(format!(
            "command contains dangerous shell metacharacter '{}'",
            meta
        ));
    }
}
```
Note: blocking bare `$` would also block `$(` which is already handled, making the check redundant but complete.

### CR-04: Literal API keys stored as both HashMap key and value in CredentialStore

**File:** `crates/lattice-cli/src/credentials.rs:53`
**Issue:** When a literal API key string is found in the config file (not an env var reference), `CredentialStore::from_config` stores it as `values.insert(key_spec.clone(), key_spec.clone())` -- the same literal key string is both the HashMap key and the HashMap value. While the `CredentialStore` struct derives `Zeroize` and is documented as zeroize-on-drop, the HashMap itself does not implement `Zeroize`. When the `CredentialStore` is dropped, the HashMap entries (both key and value allocations) remain in memory until the allocator reclaims the pages. Any process that can read freed memory (e.g., through a core dump or memory inspection tool) can recover the literal key. Additionally, the key is duplicated in memory (stored twice for each literal entry), doubling the exposure window.
**Fix:** Replace the literal-key-as-value pattern with an env-var indirection even for config-file literals. Store the literal key in a `Zeroize`-compatible container (e.g., `zeroize::Zeroizing<String>` or a `Vec<u8>` that gets explicitly zeroed on drop), and ensure the HashMap is wrapped in a type that zeroizes all entries before dropping. At minimum, avoid storing the same secret as both key and value:
```rust
values.insert(format!("config:{}", provider_name), key_spec.clone());
```

### CR-05: aliases() returns empty Vec when no provider credentials are set, misleading downstream

**File:** `crates/lattice-llm/src/router.rs:103-119`
**Issue:** The `ModelRouter::aliases()` method filters out any model whose `resolve()` call fails. `resolve()` returns an error when the provider's credential env var is not set (`CredentialStatus::Missing`). If no API keys are configured at all, `aliases()` returns an empty Vec. The `doctor` and `models` commands use this list to display available models. An empty Vec makes it appear that the system has no model support at all, rather than that credentials are missing. This is functionally misleading: the user sees "no models available" instead of "6 models available, but credentials are missing for all of them." The doctor command separately checks credentials, but the models command does not, creating inconsistent UX.
**Fix:** Return resolved models even when credentials are missing, but include the `CredentialStatus::Missing` information so the caller can display it. Change `aliases()` to return a list that includes credentialless models and models with missing credentials (with the status flag), rather than silently omitting them:
```rust
pub fn aliases(&self) -> Vec<ResolvedModel> {
    // Return all known aliases regardless of credential status
    names.iter().filter_map(|name| {
        let target = ModelTarget::new(*name);
        // Resolve even with missing credentials; the ResolvedModel carries the status
        self.resolve_full(&target) // returns ResolvedModel with credential_status field
    }).collect()
}
```

## Warnings

### WR-01: Blanket #![allow(dead_code, unused_imports, private_interfaces)] suppresses all lint warnings

**File:** `crates/lattice-cli/src/lib.rs:1`
**Issue:** The `lattice-cli` lib root has `#![allow(dead_code, unused_imports, private_interfaces)]` which globally suppresses three important Rust lint categories for the entire crate. This means any dead code, unused imports, or leaked private interfaces will never generate a compiler warning. This masks real quality issues -- for example, the `lattice-hooks` dependency is imported in Cargo.toml but never used in any source file, and the blanket allow ensures no warning fires for it. Dead code and unused imports should be discovered via warnings and removed, not silenced.
**Fix:** Remove the blanket `#![allow(...)]` directive. Address each suppressed warning individually: either remove the dead code/unused imports, or add targeted `#[allow(...)]` annotations on specific items where suppression is intentional (e.g., serde-generated code). Add `#![deny(unused_imports)]` or at least `#![warn(unused_imports)]` to prevent accumulation.

### WR-02: Inconsistent env var test synchronization across test files

**File:** `crates/lattice-llm/src/router.rs` (tests), `crates/lattice-cli/src/commands/doctor.rs` (tests)
**Issue:** The `doctor.rs` tests use an `ENV_MUTEX` to synchronize `std::env::set_var` calls, recognizing that concurrent env mutation in parallel `#[test]` functions is unsafe. However, the `router.rs` tests use raw `std::env::set_var` and `std::env::remove_var` without any synchronization. When `cargo test` runs with multiple threads (the default), these tests can race, causing intermittent failures where one test sets an env var that another test reads or removes unexpectedly. This is a test reliability issue that produces flaky CI.
**Fix:** Apply the same `ENV_MUTEX` pattern used in `doctor.rs` to the `router.rs` tests, or refactor both to use a shared test utility. Alternatively, restructure the router tests to use dependency injection for credential resolution rather than mutating global env vars.

### WR-03: Each SQLite method opens a new connection with no pooling

**File:** `crates/lattice-session/src/sqlite_graph.rs`
**Issue:** The `SqliteSessionGraphStore` stores a `PathBuf` to the database file, and every method call (e.g., `commit_turn`, `head_transcript`, `checkout`, `branch`, `diff`) opens a new `rusqlite::Connection` via `self.conn()`, performs its operation, and drops the connection. This means each session operation pays the full cost of SQLite connection setup (file open, schema init, WAL mode setup). While this is not a performance-critical bug per the v1 scope exclusion, it does create a correctness concern: between two operations on the same session, another process could modify the database, and the second operation would see a different state because it opens a fresh connection. This breaks the expectation that a sequence of operations on one `SqliteSessionGraphStore` instance operates on a consistent snapshot.
**Fix:** Replace the per-call `self.conn()` pattern with a persistent connection held in the struct. Use `rusqlite::Connection::open(&path)` once at construction time and store the `Connection` (or a `Mutex<Connection>` for thread safety). This ensures all operations on one store instance share the same transaction context and WAL state.

### WR-04: tool_output_is_ok relies on fragile string prefix matching

**File:** `crates/lattice-agent/src/agent_loop.rs:504-506`
**Issue:** The `tool_output_is_ok` function determines whether a tool execution succeeded by checking if the output string does NOT start with `"Error:"` or `"Tool denied:"`. This is a fragile heuristic: any tool executor that formats errors differently (e.g., `"error:"` lowercase, `"ERROR:"`, or a JSON error object) will pass the check and be treated as a successful tool result. The agent loop will then feed this output back to the LLM as if it were valid tool output, potentially causing the model to misinterpret error text as data. The `DefaultToolExecutor` does use `"Error:"` prefix consistently, but third-party or future tool executors may not.
**Fix:** Replace the string-prefix check with a structured result type. The `ToolCallExecutor::execute` trait method should return a `Result<String, ToolError>` or an `ExecutionResult { output: String, success: bool }` enum. The agent loop can then branch on the structured success/failure flag rather than guessing from the text content.

### WR-05: ProviderConfig stores raw api_key as Option<String>, risk of inadvertent serialization

**File:** `crates/lattice-cli/src/config.rs:26`
**Issue:** `ProviderConfig` has `pub api_key: Option<String>` with `#[derive(Serialize)]`. The `Config` struct is `Serialize`-complete. If `Config` is ever serialized to JSON, TOML, or any format (e.g., for a config export command, a debug dump, or a network API), the literal API key string will be included in the output. The `CredentialStore` correctly warns about literal keys in config files, but the `Config` struct itself can silently leak them if serialized. There is no `#[serde(skip)]` annotation on the `api_key` field.
**Fix:** Add `#[serde(skip)]` to the `api_key` field to prevent serialization:
```rust
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProviderConfig {
    #[serde(skip)]
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}
```
Note: `Deserialize` will still read the field from config files, but `Serialize` will omit it.

### WR-06: Extra transcript re-read after agent loop to obtain parent_commit_id

**File:** `crates/lattice-api/src/turn.rs:476-481`
**Issue:** After the agent loop completes, `turn.rs` calls `deps.head_transcript(&session_id)` again solely to obtain `parent_commit_id` for the commit. The agent loop itself already read the transcript at the start of the turn to build the conversation context. Between the initial read and this post-loop read, another concurrent turn on the same session could have committed, changing HEAD. The post-loop read would then pick up the wrong parent, potentially creating a commit that conflicts with the concurrent turn's commit. This is a TOCTOU (time-of-check-time-of-use) race condition for concurrent session access.
**Fix:** Capture the `parent_commit_id` from the initial transcript read (before the agent loop) and carry it through the turn execution, rather than re-reading after the loop. The parent should be fixed at the time the turn starts, not re-resolved after LLM latency.

### WR-07: write_file uses string concatenation for absolute path construction

**File:** `crates/lattice-tools/src/executor.rs:491-495`
**Issue:** The `write_file` tool constructs the absolute file path via `format!("{}/{}", self.project_root, path.trim_start_matches('/'))`. While `reject_parent_traversal` is called first, string concatenation for path construction is fragile: it does not handle double slashes, trailing slashes in `project_root`, or platform-specific path separators. On Windows, `project_root` might be `C:\Users\...` while the format uses `/`. The resulting path could be malformed or, in edge cases, resolve differently than intended by the containment check.
**Fix:** Use `PathBuf::join` for path construction:
```rust
let abs = std::path::PathBuf::from(&self.project_root)
    .join(path.trim_start_matches('/'));
```
`PathBuf::join` handles trailing slashes, double components, and platform separators correctly.

## Info

### IN-01: Duplicated test helper functions across multiple test modules

**File:** `crates/lattice-agent/src/agent_loop.rs`, `crates/lattice-api/src/turn.rs`, `crates/lattice-api/src/session.rs`
**Issue:** The `user_msg`, `assistant_msg`, and `tool_msg` helper functions are duplicated in the `#[cfg(test)]` blocks of at least three different crate test modules. Each copy constructs the same `GraphMessage` or `Message` structs. This is maintainability overhead: when the `GraphMessage` struct changes, all three copies need updating.
**Fix:** Extract these helpers into a shared `lattice-test-utils` crate or a `tests/common` module that each test module imports.

### IN-02: lattice-hooks crate fully implemented but unused by any workspace crate

**File:** `crates/lattice-hooks/src/lib.rs`
**Issue:** The `lattice-hooks` crate (405 lines) implements a complete hook system with `HookPoint`, `HookContext`, `HookDecision`, `HookChain`, and `InMemoryHookRegistry`. However, no other crate in the workspace imports or uses any of these types. The `lattice-cli/Cargo.toml` lists `lattice-hooks` as a dependency, but no `lattice-cli` source file contains `use lattice_hooks::`. This is dead code at the crate level that compiles but never executes.
**Fix:** Either remove the `lattice-hooks` crate and its Cargo.toml dependency, or integrate it into the agent loop/runtime where hook dispatch would actually occur. If it is intentionally deferred for future use, document that decision in the crate's lib.rs and add a `#[cfg(feature = "hooks")]` gate to prevent it from being compiled in default builds.

### IN-03: lattice-cli depends on lattice-hooks but never imports from it

**File:** `crates/lattice-cli/Cargo.toml`
**Issue:** The CLI crate's Cargo.toml includes `lattice-hooks` in its dependencies, but no source file in `lattice-cli` uses any type from that crate. Combined with the blanket `#![allow(unused_imports)]` in `lib.rs`, this unused dependency compiles silently. It increases build time and binary size unnecessarily, and combined with WR-01, ensures no compiler warning will ever flag it.
**Fix:** Remove `lattice-hooks` from `crates/lattice-cli/Cargo.toml` dependencies until it is actually needed.

---

_Reviewed: 2026-05-18T00:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_