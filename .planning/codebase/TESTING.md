<!-- refreshed: 2026-05-19 -->
# Testing Patterns

**Analysis Date:** 2026-05-19

## Test Framework

**Runner**

- Cargo's built-in test runner (`cargo test`). No standalone test runner crate.
- Async tests use `#[tokio::test]` with the workspace `tokio` dependency (`Cargo.toml:43`, `features = ["full"]`).
- Synchronous tests use plain `#[test]`.

**Assertion Library**

- Standard `assert!`, `assert_eq!`, `matches!`, plus pattern matching via `if let` / `match`.
- Diagnostic context is passed via the optional message argument (`assert!(cond, "msg: {}", value)`), used heavily in CLI smoke tests.

**Run Commands**

```bash
cargo test                          # workspace default test run
cargo test --workspace              # CI workspace test entry
cargo test <test_name>              # filter by test name substring
cargo test -p lattice-llm           # focused LLM type and stub tests
cargo test -p lattice-profile       # focused profile parsing and discovery
cargo test -p lattice-session       # focused session graph and SQLite tests
cargo test -p lattice-agent --test agent_loop      # AgentLoop integration tests
cargo test -p lattice-tools --test executor        # tool executor tests
cargo test -p lattice-api --test usage_turn        # API turn end-to-end tests
cargo test -p lattice-cli --test cli_cutover       # CLI surface boundary tests
cargo test -p lattice-cli --test release_smoke     # isolated CLI release smoke
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
```

CI workflow: `.github/workflows/ci.yml` runs `cargo fmt --all --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` on `ubuntu-latest` and `macos-latest` with `dtolnay/rust-toolchain@stable`.

## Test File Organization

**Locations**

- **Inline unit tests:** `#[cfg(test)] mod tests { ... }` (or named modules such as `mod session_graph`, `mod tests`) at the bottom of implementation files. Coverage spans 30+ source files (`crates/lattice-llm/src/types.rs`, `crates/lattice-llm/src/router.rs`, `crates/lattice-llm/src/sse.rs`, `crates/lattice-session/src/graph.rs`, `crates/lattice-session/src/sqlite_graph.rs`, `crates/lattice-profile/src/schema.rs`, `crates/lattice-profile/src/load.rs`, `crates/lattice-profile/src/discovery.rs`, `crates/lattice-tools/src/edit/parser.rs`, etc.).
- **Crate-external integration tests:** `crates/*/tests/*.rs`, one binary per file:
  - `crates/lattice-agent/tests/agent_loop.rs` (12 tests) -- AgentLoop multi-turn execution
  - `crates/lattice-tools/tests/executor.rs` (19 tests) -- tool executor + path containment
  - `crates/lattice-api/tests/usage_turn.rs` (3 tests) -- end-to-end `run_turn` flow
  - `crates/lattice-cli/tests/cli_cutover.rs` (9 tests) -- CLI surface boundary
  - `crates/lattice-cli/tests/release_smoke.rs` (11 tests) -- isolated CLI release smoke
- Internal-API test helpers live alongside the code: `crates/lattice-api/src/turn/tests.rs` is included via `#[cfg(test)] mod tests;` rather than the inline `mod tests` pattern.

**Naming**

- Test function names use `snake_case` and describe behavior, following `subject_condition_expected_result`. Examples:
  - `agent_loop_unknown_enabled_tools_fails_before_llm_call`
  - `read_file_supports_relative_paths_line_ranges_and_line_numbers`
  - `smoke_resolve_missing_credential_names_env_var`
  - `cli_cutover_help_omits_removed_commands`
- Integration test files name their intent: `cli_cutover` (boundary), `release_smoke` (isolated smoke), `usage_turn` (end-to-end usage), `agent_loop` (loop semantics), `executor` (tool executor).

**Structure**

```text
crates/<crate>/src/*.rs                # implementation + #[cfg(test)] mod tests at bottom
crates/<crate>/tests/*.rs              # crate-external integration tests
crates/lattice-api/src/turn/tests.rs   # cfg(test) module file included from turn/mod.rs
.github/workflows/ci.yml               # fmt + clippy + workspace test gate
scripts/source-loc-budget.sh           # strict LOC budget gate
scripts/release-boundary-check.sh      # boundary and docs drift gate
```

## Test Structure

**Inline Unit Test Pattern**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serde_roundtrip() {
        let cases = vec![Role::System, Role::User, Role::Assistant, Role::Tool];
        for role in cases {
            let json = serde_json::to_string(&role).unwrap();
            let deserialized: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(role, deserialized);
        }
    }
}
```

Source: `crates/lattice-llm/src/types.rs:239-262`.

**Async Unit Test Pattern**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chat_collects_stream() {
        let stub = StubLlm::new();
        let req = make_request("hello world");
        let resp = stub.chat(req).await.unwrap();
        assert_eq!(resp.content, "stub response: hello world");
        assert_eq!(resp.finish_reason, "stop");
    }
}
```

Source: `crates/lattice-llm/src/stub.rs:296-306`.

**Subprocess Integration Test Pattern**

```rust
fn lattice() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lattice"))
}

#[test]
fn cli_cutover_help_contains_supported_commands() {
    let output = lattice().arg("--help").output().unwrap();
    assert!(output.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in &["run", "models", "resolve", "session", "doctor"] {
        assert!(stdout.contains(name), "help should contain '{name}'");
    }
}
```

Source: `crates/lattice-cli/tests/cli_cutover.rs:18-28`. Cargo populates `CARGO_BIN_EXE_<name>` for binaries declared in the same package.

## Mocking

**Framework:** Hand-written fakes implementing the same async traits used in production. No standalone mocking crate (mockall, mockito, etc.) is in the workspace.

**AgentLoop Injectable Traits**

- `LlmClient` -- async trait for LLM calls; test doubles return queued `ChatResponse` values from a `Mutex<VecDeque<...>>`. See `FakeLlm` at `crates/lattice-agent/tests/agent_loop.rs:11-39` and `crates/lattice-api/tests/usage_turn.rs:17-49`.
- `ToolCallExecutor` -- async trait for tool execution; test doubles return queued strings. See `FakeToolExecutor` at `crates/lattice-agent/tests/agent_loop.rs:41-62`.
- `TurnDeps` -- crate-private trait at `crates/lattice-api/src/turn/deps.rs`; `FakeTurnDeps` at `crates/lattice-api/src/turn/tests.rs:14-86` provides full inspection of create/open/transcript/commit calls.

**Capturing Mock Pattern**

For tests that need to assert on LLM request shape, the test defines an inline `CapturingLlm` struct that records the request before delegating:

```rust
struct CapturingLlm {
    captured: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>>,
}

#[async_trait::async_trait]
impl LlmClient for CapturingLlm {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ...> {
        *self.captured.lock().unwrap() = Some(request.clone());
        Ok(ChatResponse { ... })
    }
}
```

Source: `crates/lattice-agent/tests/agent_loop.rs:140-159`.

**Order-Tracking Executor Pattern**

```rust
struct OrderTrackingExecutor {
    order: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ToolCallExecutor for OrderTrackingExecutor {
    async fn execute(&self, call: &ToolCall) -> Result<String, String> {
        self.order.lock().unwrap().push(call.id.clone());
        Ok(format!("content of {}", call.function.name))
    }
}
```

Source: `crates/lattice-agent/tests/agent_loop.rs:368-378`.

**What to Mock**

- LLM provider calls (`LlmClient`) -- always mocked outside `lattice-llm` itself.
- Multi-turn loop dependencies (`TurnDeps`) -- mocked when isolating loop logic from real session storage.
- Tool execution at the agent layer (`ToolCallExecutor`) -- mocked when verifying agent-loop wiring; real `DefaultToolExecutor` is used in `lattice-api` end-to-end tests.

**What NOT to Mock**

- `SqliteSessionGraphStore` -- used directly with `tempfile::TempDir`-rooted SQLite databases. See `crates/lattice-cli/tests/release_smoke.rs:43-85` and `crates/lattice-api/tests/usage_turn.rs:170-231`.
- `DefaultToolExecutor` -- used directly in `crates/lattice-tools/tests/executor.rs` and in API end-to-end tests with real temp workspaces.
- The CLI binary -- exercised via `Command::new(env!("CARGO_BIN_EXE_lattice"))` against real subprocess output.

## Fixtures and Factories

**Locations**

- `crates/lattice-llm/src/stub.rs` and `crates/lattice-llm/src/types.rs` -- inline `make_request()` helpers.
- `crates/lattice-llm/src/router.rs` -- `make_resolved()` helpers for ResolvedModel fixtures.
- `crates/lattice-cli/tests/release_smoke.rs:38-93` -- `seed_session()` and `seed_default_profile()` helpers seed the on-disk session store and a default agent profile in temp directories.
- `crates/lattice-api/tests/usage_turn.rs:96-156` -- `Workspace`, `setup_workspace()`, and `build_agent_loop()` build a complete temp workspace plus loaded profile and `AgentLoop`.
- `crates/lattice-session/src/lib.rs` -- `pub mod test_helpers` exposes `user_msg()` / `assistant_msg()` for downstream crate tests (consumed by `crates/lattice-api/src/session.rs:159`).

**Test Data Factories**

```rust
fn tool_call_response(id: &str, name: &str, args: &str) -> ChatResponse {
    ChatResponse {
        content: String::new(),
        tool_calls: Some(vec![ToolCall { id: id.into(), function: FunctionCall { name: name.into(), arguments: args.into() } }]),
        finish_reason: "tool_calls".to_string(),
        model: "fake-model".to_string(),
        usage: Some(TokenUsage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 }),
        ..Default::default()
    }
}

fn final_response(text: &str) -> ChatResponse { ... }
```

Source: `crates/lattice-api/tests/usage_turn.rs:59-93`.

## Environment Isolation

- **CLI smoke isolation:** Every `release_smoke.rs` test sets `HOME`, `XDG_CONFIG_HOME`, `LATTICE_AGENTS_DIR=""`, and `current_dir` to a temp directory. See `lattice()` and `temp_home()` at `crates/lattice-cli/tests/release_smoke.rs:16-35`.
- **Session seeding:** Smoke tests seed temp stores via `lattice_session::SqliteSessionGraphStore::new(...)` and direct `commit_turn` calls rather than running binary subcommands (`crates/lattice-cli/tests/release_smoke.rs:39-85`).
- **Workspace isolation:** API tests build per-test temp workspaces with `tempfile::tempdir()` and write `src/lib.rs`, `.lattice/agents/default/agent.toml`, then resolve the profile from a separate `global_agents_dir` temp dir to prevent leakage from the user's `~/.lattice` (`crates/lattice-api/tests/usage_turn.rs:103-140`).
- **Credential safety:** Smoke and cutover tests set fake `sk-...` values into `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` and assert that the secret value never appears in stdout/stderr (`crates/lattice-cli/tests/release_smoke.rs:120-123, 285-294, 311-321`).
- **Per-test temp dir naming:** `temp_home("models-auth")`, `temp_home("doctor")` -- each test names its sub-tree to make failure forensics easier and to avoid cross-test conflicts when running with `--test-threads=1`.

## Test Design

**Behavioral tests** assert specific expected outcomes with narrow assertions on status, message order, usage totals, terminal reason text, and event sequences. Most `agent_loop.rs` and `executor.rs` tests follow this style.

**Boundary tests** assert that removed features are absent. `crates/lattice-cli/tests/cli_cutover.rs` enumerates `--help` output and asserts the supported subcommand set is exactly `run`, `models`, `resolve`, `session`, `doctor` (plus clap's `help`). `cli_cutover.rs:211-297` walks the workspace source tree and fails the build if any `use lattice_core::*`, `use lattice_runtime::*`, etc. appears -- enforcing crate-level architecture invariants from the test suite.

**Smoke tests** verify subprocess behavior end-to-end with real binary invocation, focusing on exit codes, env-var-driven failure messages, and secret redaction (`crates/lattice-cli/tests/release_smoke.rs`).

**Round-trip serde tests** verify Serialize/Deserialize symmetry. See `crates/lattice-llm/src/types.rs:243-398` -- 12 tests covering every wire-protocol type.

## Coverage

There is no coverage threshold or coverage CI step. CI runs only `fmt`, `clippy`, and `cargo test --workspace`.

Current coverage shape (from `Grep "#[test]"` and `#[tokio::test]` counts):

- 364 `#[test]` total occurrences across 29 files.
- 68 `#[tokio::test]` occurrences across 8 files.
- Strongest coverage: LLM types and routing, profile parsing and discovery, session graph SQLite operations, agent loop multi-turn semantics, tool execution and path containment, API turn flow, CLI surface boundary.
- Weaker coverage: real provider HTTP transport, Criterion benchmark regression thresholds, hooks crate (single file, currently no extensive tests).

## Test Types

**Unit Tests**

- Co-located with implementation in `#[cfg(test)] mod tests` blocks.
- Cover pure logic, serde round-trip, parsers (TOML, SSE, patch grammar), formatters, state transitions, `validate_branch_name`, hash routines.

**Integration Tests**

- Crate-external tests under `crates/*/tests/` validate public APIs and cross-module wiring.
- Representative paths:
  - `crates/lattice-agent/tests/agent_loop.rs` -- multi-turn loop with tool calls, timeout, cancellation, max-rounds cap.
  - `crates/lattice-tools/tests/executor.rs` -- read/write/patch/grep/list/find/bash/web tool execution and path containment.
  - `crates/lattice-api/tests/usage_turn.rs` -- end-to-end `run_turn` with real tool executor + SQLite store + fake LLM.
  - `crates/lattice-cli/tests/cli_cutover.rs` -- CLI surface boundary against compiled binary.

**Release Smoke Tests**

- Isolated binary smoke for `run`, `models --auth`, `resolve sonnet`, `session status`, `session log`, and `doctor`.
- Location: `crates/lattice-cli/tests/release_smoke.rs`. All tests set temp `HOME`/`XDG_CONFIG_HOME` and a fake credential to assert secret redaction.

**Benchmarks**

- Workspace declares `[profile.bench] lto = true` (`Cargo.toml:66-67`) but no `benches/` directories or Criterion targets exist in the current crates. CI does not run `cargo bench`.

## Common Patterns

**Isolated CLI Smoke Pattern**

```rust
fn lattice(temp_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lattice"));
    cmd.env("HOME", temp_home)
        .env("XDG_CONFIG_HOME", temp_home.join(".config"))
        .env("LATTICE_AGENTS_DIR", "")
        .current_dir(temp_home);
    cmd
}
```

Source: `crates/lattice-cli/tests/release_smoke.rs:16-23`.

**Session Seeding via Graph API Pattern**

```rust
let session_dir = temp_home.join(".config").join("lattice").join("sessions");
let store = lattice_session::SqliteSessionGraphStore::new(&session_dir);
store.create_session(session_id, Some("Smoke test"), None, Some("sonnet"), Some("anthropic"))?;
store.commit_turn(TurnCommitInput { ... })?;
```

Source: `crates/lattice-cli/tests/release_smoke.rs:39-85`. Tests bypass the CLI for write side-effects to keep them deterministic.

**Async Timeout Test Pattern**

```rust
let mut request = make_request("hello", vec!["read_file".to_string()]);
request.timeout = Some(Duration::from_millis(50));

let started = std::time::Instant::now();
let result = loop_inst.run_turn(request).await;

assert_eq!(result.status, AgentTurnStatus::Error);
assert!(result.terminal_reason.as_ref().unwrap().contains("timeout"));
assert!(started.elapsed() < Duration::from_secs(3), "timeout should return promptly");
```

Source: `crates/lattice-agent/tests/agent_loop.rs`. Asserts functional outcome, bounded wall-clock latency, and preservation of pre-timeout process events.

**Cancellation Test Pattern**

A shared `Arc<AtomicBool>` cancel flag is passed via `AgentTurnRequest::cancel_flag`; the fake LLM flips it after the first response and the test asserts `AgentTurnStatus::Cancelled` (`crates/lattice-agent/tests/agent_loop.rs:567-628`).

**Error Path Test Pattern**

```rust
let llm = Arc::new(FakeLlm::new(vec![Err("provider unavailable".to_string())]));
let result = loop_inst.run_turn(request).await;

assert_eq!(result.status, AgentTurnStatus::Error);
assert!(result.terminal_reason.as_ref().unwrap().contains("LLM error"));
assert!(result.terminal_reason.as_ref().unwrap().contains("provider unavailable"));
```

Source: `crates/lattice-agent/tests/agent_loop.rs:704-732`.

**Secret Safety Test Pattern**

```rust
.env("ANTHROPIC_API_KEY", "sk-smoke-test-secret")
// ...
assert!(!stdout.contains("sk-smoke-test-secret"), "must not leak secret value");
assert!(stdout.contains("ANTHROPIC_API_KEY"), "should name the env var");
```

Source: `crates/lattice-cli/tests/release_smoke.rs:103-123`. The contract is: env-var name appears, value never does.

## Recommended Verification Matrix

**Low-risk changes**

- Pure logic, parser, or formatter change: `cargo test -p <crate> <test_name>` plus `cargo fmt --all --check`.

**Medium-risk changes**

- Provider/transport/streaming: `cargo test -p lattice-llm` plus targeted SSE tests.
- Agent loop or tool result handling: `cargo test -p lattice-agent` plus `cargo test -p lattice-tools`.
- API facade: `cargo test -p lattice-api`.
- CLI command output: `cargo test -p lattice-cli --test cli_cutover`.

**High-risk changes**

Cross-crate APIs, feature flags, or dependency changes:

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
./scripts/source-loc-budget.sh
./scripts/release-boundary-check.sh
```

The CI workflow at `.github/workflows/ci.yml` runs the first three on every push and PR.

## CI Risks and Notes

- CI uses `dtolnay/rust-toolchain@stable`; the workspace declares `rust-version = "1.85"` (`Cargo.toml:17`) which is not pinned in CI. A toolchain newer than the MSRV could mask a regression.
- `release_smoke.rs` and `usage_turn.rs` tests rely on a writable `std::env::temp_dir()`. They assume Unix-style permissions; the `#[cfg(unix)]` shell tests in `crates/lattice-tools/tests/executor.rs:339-400` are explicit about platform gating.
- `.planning/` is gitignored; codebase map files need `git add -f` to commit.
- Subprocess tests depend on the `lattice` binary being built first by the cargo test harness. Running individual tests without the binary built yields opaque failures.

---

*Testing analysis: 2026-05-19*
