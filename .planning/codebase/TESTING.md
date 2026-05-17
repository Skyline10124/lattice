<!-- refreshed: 2026-05-17 -->
# Testing Patterns

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## Test Framework

**Runner**

- Rust uses Cargo's built-in test runner (`cargo test`).
- Async tests use `#[tokio::test]`.

**Assertion Library**

- Rust tests use standard `assert!`, `assert_eq!`, `matches!`, and pattern matching.

**Run Commands**

```bash
cargo test                          # default test entry
cargo test --workspace              # CI workspace test entry
cargo test <test_name>              # run tests matching a name
cargo test -p lattice-llm           # focused LLM routing tests
cargo test -p lattice-profile       # focused profile parsing tests
cargo test -p lattice-session       # focused session graph tests
cargo test -p lattice-agent agent_loop  # focused agent loop tests
cargo test -p lattice-tools         # focused tools/executor tests
cargo test -p lattice-api           # focused API facade tests
cargo test -p lattice-cli --test cli_cutover    # CLI surface boundary tests
cargo test -p lattice-cli --test release_smoke  # isolated CLI smoke tests
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
cargo bench
```

## Test File Organization

**Locations**

- **Inline unit tests**: `#[cfg(test)] mod tests` blocks at the bottom of implementation files.
- **Crate-external integration tests** live under `crates/*/tests/`:
  - `crates/lattice-cli/tests/cli_cutover.rs` -- CLI surface boundary tests
  - `crates/lattice-cli/tests/release_smoke.rs` -- isolated CLI smoke tests covering all D-16 commands

**Naming**

- Test function names use `snake_case` and describe behavior.
- Integration test files name their intent: `cli_cutover`, `release_smoke`.

**Structure**

```text
crates/<crate>/src/*.rs                # implementation + #[cfg(test)] unit tests at bottom
crates/<crate>/tests/*.rs              # crate-external integration tests
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
    fn component_condition_expected_result() {
        let result = subject_under_test();
        assert_eq!(result, expected);
    }
}
```

**Async Unit Test Pattern**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn async_component_condition_expected_result() {
        let result = subject_under_test().await;
        assert!(result.is_ok());
    }
}
```

## Mocking

**Framework:** Hand-written mocks, injectable async traits, and loopback TCP servers. No standalone mocking crate.

**AgentLoop Injectable Traits:**

- `LlmClient` -- async trait for LLM calls; test doubles provide deterministic responses.
- `ToolCallExecutor` -- async trait for tool execution; test doubles return fixed outcomes.
- `TurnDeps` -- crate-private trait providing dependency injection for `run_turn`/`stream_turn`.

**Loopback Provider Pattern:**

Mock servers bind `127.0.0.1:0` with OpenAI-compatible SSE responses. Returns `Option<u16>` where `None` signals a `PermissionDenied` skip.

## Fixtures and Factories

**Locations:**

- `crates/lattice-llm/src/` inline tests: `make_resolved()` helpers, SSE body fixture builders.
- `crates/lattice-cli/tests/release_smoke.rs`: isolated CLI smoke with temp HOME/XDG/cwd isolation, session seeding via `SqliteSessionGraphStore`.
- `crates/lattice-cli/tests/cli_cutover.rs`: boundary tests asserting removed commands do not appear in help.

## Environment Isolation

- **CLI smoke isolation:** Every `release_smoke.rs` test sets `HOME`, `XDG_CONFIG_HOME`, and `cwd` to temp directories.
- **Session seeding:** Smoke tests seed temp stores via `SqliteSessionGraphStore` directly rather than calling binary subcommands.
- **Credential safety:** Smoke tests set fake `sk-` secrets and assert output does not contain the secret value.

## Test Design: Characterization vs Behavioral

**Characterization tests** capture current behavior as-is with `_characterization` suffix.

**Behavioral tests** assert specific expected outcomes with narrow assertions.

**Boundary tests** assert removed features are absent (e.g., `cli_cutover.rs` asserting no `tui`, `debug`, `config` commands in help).

## Coverage

No coverage threshold config or coverage CI step exists.

Current coverage shape:
- Strongest coverage: LLM routing, profile parsing, session graph operations, agent loop, tool execution, API facade, CLI surface.
- Weaker coverage: real provider APIs, benchmark regression thresholds.

## Test Types

**Unit Tests**

- Co-located with implementation in `#[cfg(test)] mod tests` blocks.
- Cover pure logic, serde, parsers, formatters, state transitions.

**Integration Tests**

- Crate-external tests under `crates/*/tests/` validate public APIs and cross-module wiring.
- Representative paths: `crates/lattice-cli/tests/cli_cutover.rs`, `crates/lattice-cli/tests/release_smoke.rs`.

**Release Smoke Tests**

- Isolated binary smoke with temp HOME/XDG/cwd for `run`, `models --auth`, `resolve`, `session status`, `session log`, and `doctor`.
- Location: `crates/lattice-cli/tests/release_smoke.rs`.

**Benchmarks**

- Criterion benchmark paths: `benches/`.
- CI does not run `cargo bench`; performance changes need manual benchmark runs.

## Common Patterns

**Isolated CLI Smoke Pattern:**

```rust
let tmp = tempfile::tempdir().unwrap();
let home = tmp.path().join("home");
let xdg = tmp.path().join("xdg");
let project = tmp.path().join("project");
// Create isolated environment
std::fs::create_dir_all(&home)?;
std::fs::create_dir_all(&xdg)?;
// Run command with isolation
let mut cmd = Command::cargo_bin("lattice")?;
cmd.env("HOME", &home).env("XDG_CONFIG_HOME", &xdg);
```

**Session Seeding via Graph API Pattern:**

```rust
let store = SqliteSessionGraphStore::open_in_memory()?;
let api = SessionApi::new(Arc::new(store));
let session_id = api.create_session("default", None)?;
// Now run `lattice session status <session_id>` against temp store
```

**CLI Boundary Test Pattern:**

The boundary test asserts that unsupported command names do not appear in the CLI help output. See `crates/lattice-cli/tests/cli_cutover.rs` for the exact list.

## Recommended Verification Matrix

**Low-risk changes:**

- Rust pure logic, parser, or formatter: `cargo test -p <crate> <test_name>` plus `cargo fmt --all --check`.

**Medium-risk changes:**

- Provider/transport/streaming: `cargo test -p lattice-llm` plus targeted transport tests.
- Agent loop or tool result handling: `cargo test -p lattice-agent` plus `cargo test -p lattice-tools`.
- API facade: `cargo test -p lattice-api`.
- CLI command output: `cargo test -p lattice-cli --test cli_cutover`.

**High-risk changes:**

Cross-crate APIs, feature flags, or dependency changes:

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
./scripts/source-loc-budget.sh --max 20000
./scripts/release-boundary-check.sh
./scripts/release-boundary-check.sh --docs
```

## CI Risks and Notes

- CI uses `dtolnay/rust-toolchain@stable`, while the workspace declares `rust-version = "1.85"`.
- Loopback mock server tests skip on `PermissionDenied` in restricted environments.
- `.planning/` is gitignored; codebase map files need `git add -f` when committing.

---

*Testing analysis: 2026-05-17 (post-Phase 11 deletion)*
