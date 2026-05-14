# Testing Patterns

**Analysis Date:** 2026-05-14

## Test Framework

**Runner**

- Rust uses Cargo's built-in test runner.
- Async tests use `#[tokio::test]`.
- Benchmarking uses Criterion. Benchmark files exist at `benches/benchmarks.rs` and `crates/lattice-core/benches/benchmarks.rs`.
- Python script tests exist in `tests/test_rewrite_links.py`. The script uses a standard-library custom entry point and can be run directly with `python3 tests/test_rewrite_links.py`.
- No `pytest.ini`, `pyproject.toml`, or `requirements*.txt` was detected for Python tests.

**Assertion Library**

- Rust tests use standard `assert!`, `assert_eq!`, `matches!`, `panic!`, and pattern matching.
- Python script tests use standard `assert`.

**Run Commands**

```bash
cargo test                         # project-documented default test entry
cargo test --workspace             # CI workspace test entry
cargo test <test_name>             # run tests matching a name
cargo test -p lattice-cli --test code_agent_eval
cargo test -p lattice-core --test e2e
cargo clippy --workspace -- -D warnings
cargo fmt --all --check
cargo bench
python3 tests/test_rewrite_links.py
python3 scripts/generate-summary.py --verify
python3 scripts/rewrite-links.py --check
```

**CI Commands**

`.github/workflows/ci.yml` runs on `ubuntu-latest` and `macos-latest`:

- `cargo fmt --all --check`
- `cargo clippy --workspace -- -D warnings`
- `cargo test --workspace`

## Test File Organization

**Locations**

- Many unit tests live next to implementation code under `#[cfg(test)] mod tests`, for example in `crates/lattice-agent/src/state.rs`, `crates/lattice-core/src/router.rs`, `crates/lattice-runtime/src/lib.rs`, and `crates/lattice-cli/src/frontend/tui/markdown.rs`.
- Crate-level integration tests live under `crates/*/tests/`.
- Important integration paths include:
  - `crates/lattice-core/tests/e2e.rs`
  - `crates/lattice-core/tests/e2e/*.rs`
  - `crates/lattice-core/tests/transport_dispatcher_characterization.rs`
  - `crates/lattice-core/tests/transport_integration.rs`
  - `crates/lattice-agent/tests/agent_mock.rs`
  - `crates/lattice-agent/tests/context_e2e.rs`
  - `crates/lattice-cli/tests/code_agent_eval.rs`
  - `crates/lattice-cli/tests/frontend_runtime_guards.rs`
  - `crates/lattice-tools/tests/exposure.rs`
- Root `tests/` currently contains Python docs/wiki script tests.
- Root `examples/*.rs` and `crates/lattice-core/examples/*.rs` are manual provider/transport examples and are not the primary CI path.

**Naming**

- Rust test functions use snake_case and describe behavior, for example `test_chat_basic_streaming`, `frontend_no_live_agent_ownership`, and `task_state_serde_round_trip`.
- Characterization and regression files name their intent explicitly, for example `error_classification_characterization.rs` and `regression_wave4_5.rs`.
- Python tests use `test_*` names and the script collects them by name at the end of `tests/test_rewrite_links.py`.

**Structure**

```text
crates/<crate>/src/*.rs             # implementation + #[cfg(test)] unit tests
crates/<crate>/tests/*.rs           # crate-external integration tests
crates/lattice-core/tests/e2e.rs    # e2e test hub + shared env helpers
crates/lattice-core/tests/e2e/*.rs  # e2e scenarios
crates/lattice-cli/src/test_support.rs
benches/benchmarks.rs
crates/lattice-core/benches/benchmarks.rs
tests/test_rewrite_links.py
```

**Observed Counts**

- Rust source files: 243.
- Rust files under `tests`, `benches`, or `examples`: 30.
- `#[test]` occurrences in `crates/`, `tests/`, and `benches/`: 1205.
- `#[tokio::test]` occurrences in `crates/`, `tests/`, and `benches/`: 160.

## Test Structure

**Typical Unit Test Shape**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_condition_expected_result() {
        let result = subject_under_test();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn async_component_condition_expected_result() {
        let result = subject_under_test().await;
        assert!(result.is_ok());
    }
}
```

**Actual Patterns**

- Single-file behavior tests: `crates/lattice-agent/src/state.rs` tests `TaskState` todo behavior, verification behavior, and serde round-trips.
- Integration test hub: `crates/lattice-core/tests/e2e.rs` defines environment isolation helpers and mounts e2e scenario files with `#[path = "..."] mod ...;`.
- Mock HTTP streaming tests: `crates/lattice-core/tests/e2e/chat_mock.rs` and `crates/lattice-agent/tests/agent_mock.rs` start `tokio::net::TcpListener`, return OpenAI-style SSE chunks, and validate `StreamEvent` / `LoopEvent`.
- Architecture guard tests: `crates/lattice-cli/tests/frontend_runtime_guards.rs` reads active source files and asserts that the frontend does not re-own a live `Agent` or render `LoopEvent` directly.
- CLI/runtime fixtures: `crates/lattice-cli/src/test_support.rs` builds `Runtime`, registers scripted models, and submits `TurnRequest` values for tests such as `code_agent_eval`.

## Mocking

**Framework:** hand-written mocks and fixtures. No standalone mocking crate was detected.

**Loopback Provider Pattern**

```rust
let listener = TcpListener::bind("127.0.0.1:0").await?;
let port = listener.local_addr().unwrap().port();
tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    let _ = stream.read(&mut buf).await;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await.unwrap();
});
```

**What To Mock**

- Provider HTTP endpoints: use loopback `TcpListener` with OpenAI-compatible SSE responses. See `crates/lattice-core/tests/e2e/chat_mock.rs`, `crates/lattice-agent/tests/agent_mock.rs`, and `crates/lattice-cli/src/test_support.rs`.
- Model catalog entries: construct `ResolvedModel` manually or use `Runtime::register_model()` for scripted models.
- CLI runtime turns: use `CodingAgentRuntimeTurnFixture`, `RunCommandRuntimeTurnFixture`, and `DebugRuntimeTurnFixture` from `crates/lattice-cli/src/test_support.rs`.
- Filesystem state: use `tempfile` or `tempfile::tempdir()` for sessions, config, tool executor, and patch apply tests.

**What Not To Mock**

- Do not mock parsers/serializers themselves. Use real parser input and round-trip tests for `serde_json`, TOML, SSE parsers, and policy denials.
- Do not bypass runtime architecture in frontend tests. Frontend tests should continue using `RuntimeTurnEvent`, `TurnCompletion`, and `submit_turn()`.
- Do not rely on real provider API keys or real internet. Existing e2e tests use local mock servers, and credential resolution tests isolate environment variables.

## Fixtures And Factories

**Representative Test Data**

```rust
fn make_resolved_model(base_url: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test-model".into(),
        provider: "test".into(),
        api_key: Some("sk-test".into()),
        base_url: base_url.to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "test-model".into(),
        context_length: 8192,
        provider_specific: HashMap::new(),
        credential_status: CredentialStatus::Present,
    }
}
```

**Locations**

- `crates/lattice-core/tests/e2e/chat_mock.rs`: `make_resolved_model()`, `make_user_message()`, and OpenAI SSE body fixtures.
- `crates/lattice-agent/tests/agent_mock.rs`: `make_resolved()`, content chunks, tool-call chunks, and SSE response helpers.
- `crates/lattice-cli/src/test_support.rs`: runtime turn fixture structs, scripted OpenAI server, and scripted response chunk helpers.
- `benches/benchmarks.rs`: benchmark-only factories such as `make_model_catalog_entry()`, `make_resolved_model()`, and `make_conversation_messages()`.
- `tests/test_rewrite_links.py`: Python `tempfile.TemporaryDirectory()` plus synthetic wiki files.

## Environment Isolation

**Environment Variables**

- `crates/lattice-core/tests/e2e.rs` defines `env_lock::lock()` and `ALL_CREDENTIAL_ENV_VARS`.
- Any test that modifies provider credential environment variables should acquire the global mutex first.
- Helpers include `isolate_env()` and `restore_env_batch()` for clearing and restoring every environment variable that can affect model credential resolution.
- New credential/router/provider resolution tests should reuse these helpers rather than clearing only a single key.

**Loopback Networking**

- Mock servers bind `127.0.0.1:0`.
- If a sandbox denies loopback binding, tests print a skip message and return.
- New loopback tests should preserve this `PermissionDenied` skip pattern to avoid false failures in restricted environments.

## Coverage

No coverage threshold config, `tarpaulin.toml`, or coverage CI step was detected.

Potential coverage command if the project later chooses a tool:

```bash
cargo test --workspace
# Coverage would require a separate cargo-tarpaulin or llvm-cov convention.
```

Current coverage shape:

- Stronger coverage: core types, routing, error classification, streaming parsers, policy/sandbox validation, runtime turns, session persistence, and TUI rendering.
- Weaker coverage: real provider APIs, long-running TUI behavior, benchmark regression thresholds, Python docs scripts in CI, and wiki checks.

## Test Types

**Unit Tests**

- Co-located with implementation.
- Cover pure logic, serde, parsers, formatters, state transitions, and policy matching.
- Representative paths: `crates/lattice-core/src/errors.rs`, `crates/lattice-core/src/router.rs`, `crates/lattice-agent/src/state.rs`, `crates/lattice-policy/src/authority.rs`, `crates/lattice-cli/src/frontend/tui/markdown.rs`.

**Integration Tests**

- Crate-external tests validate public APIs and cross-module wiring.
- Representative paths: `crates/lattice-core/tests/e2e.rs`, `crates/lattice-agent/tests/agent_mock.rs`, `crates/lattice-cli/tests/code_agent_eval.rs`, `crates/lattice-tools/tests/exposure.rs`.

**E2E Tests**

- Current e2e tests use local mock providers, not real external services.
- `crates/lattice-core/tests/e2e/chat_mock.rs` validates the resolve -> chat -> stream parsing path.
- `crates/lattice-agent/tests/agent_mock.rs` validates `Agent::send_message()`, `Agent::run()`, `Agent::run_streaming()`, and tool-call handling.
- `crates/lattice-cli/src/test_support.rs` supports CLI `run`, `code`, and `debug` paths through runtime turn completion.

**Benchmarks**

- Criterion benchmark paths are `benches/benchmarks.rs` and `crates/lattice-core/benches/benchmarks.rs`.
- Benchmarks cover JSON throughput, SSE parsing, concurrent model resolve, and single model resolve latency.
- CI does not run `cargo bench`; performance changes need manual benchmark runs.

**Script Tests**

- `tests/test_rewrite_links.py` covers `scripts/rewrite-links.py` link rewrite, dry-run, and broken-link behavior.
- `scripts/generate-summary.py --verify` includes md reference count checks, but CI does not call it.
- `.github/workflows/ci.yml` currently runs only Rust fmt, clippy, and test commands.

## Common Patterns

**Async Testing**

```rust
#[tokio::test]
async fn test_chat_basic_streaming() {
    let Some(port) = spawn_mock_openai_server(sse_body).await else {
        eprintln!("skipping mock server test: loopback bind denied");
        return;
    };

    let mut stream = lattice_core::chat(&resolved, &messages, &[])
        .await
        .expect("chat() should succeed");
}
```

**Sync Wrapper Around Async**

```rust
#[test]
fn test_agent_run_basic() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some(port) = rt.block_on(spawn_mock_server(sse_body)) else {
        eprintln!("skipping mock server test: loopback bind denied");
        return;
    };
    let events = rt.block_on(agent.run("Test run", 3));
}
```

**Error Testing**

```rust
match result {
    Err(lattice_core::LatticeError::Authentication { provider, .. }) => {
        assert_eq!(provider, "test");
    }
    Err(other) => panic!("Expected Authentication error, got: {other:?}"),
    Ok(_) => panic!("Expected error, got Ok"),
}
```

**Serde Round-Trip Testing**

```rust
let json = serde_json::to_string(&state).unwrap();
let round: TaskState = serde_json::from_str(&json).unwrap();
assert_eq!(round, state);
```

**Source Guard Testing**

```rust
assert_absent_in_active_source(&[
    ".run_streaming(",
    ".run(",
    "LoopEvent::Manifest",
    "render(&LoopEvent",
]);
```

## Recommended Verification Matrix

**Low-risk changes**

- Rust pure logic, parser, or formatter: `cargo test -p <crate> <test_name>` plus `cargo fmt --all --check`.
- Single CLI/TUI helper: `cargo test -p lattice-cli <test_name>` plus `cargo clippy --workspace -- -D warnings`.
- Python docs script: `python3 tests/test_rewrite_links.py` or the corresponding script `--check` / `--verify`.

**Medium-risk changes**

- Provider/transport/streaming: `cargo test -p lattice-core --test e2e` plus targeted `cargo test -p lattice-core transport`.
- Agent state/tool/result handling: `cargo test -p lattice-agent` plus `cargo test -p lattice-tools`.
- Runtime turn/session persistence: `cargo test -p lattice-runtime` plus `cargo test -p lattice-session`.
- CLI command output: `cargo test -p lattice-cli --test code_agent_eval`.

**High-risk changes**

For cross-crate APIs, feature flags, runtime wiring, or policy/sandbox changes, run the CI-equivalent matrix:

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

For feature-gating changes, add relevant no-default or target-feature combinations:

```bash
cargo test --workspace --no-default-features
cargo test -p lattice-runtime --features blob-store
cargo test -p lattice-agent --features sqlite-memory
```

For performance or hot SSE parser changes, also run:

```bash
cargo bench
```

## CI Risks And Notes

- CI uses `dtolnay/rust-toolchain@stable`, while the workspace declares `rust-version = "1.85"`. Local Rust versions older than 1.85 do not meet the workspace MSRV.
- CI does not run Python script tests, wiki link checks, examples, or benchmarks. Changes in `scripts/`, `wiki/`, `examples/`, or `benches/` need manual checks.
- CI runs on Ubuntu and macOS. Linux-only Landlock code is behind `#[cfg(target_os = "linux")]`, while macOS covers non-Linux compile paths.
- `crates/lattice-core/build.rs` calls `python3-config --ldflags --embed` and silently returns if unavailable. Build/test plumbing changes should account for local and CI Python embed linker availability.
- Loopback mock server tests skip on `PermissionDenied`. If a task requires proving the network path was exercised, run the relevant tests in an unrestricted environment and inspect output.

## Notes

- The user-provided root instruction include list references `@RTK.md`, but no `RTK.md` file exists at the repository root.
- This testing map does not depend on `RTK.md` and does not create it.
- `.env.example` exists. Tests and docs should not read `.env` or print real provider secrets.
- `tests/__pycache__/test_rewrite_links.cpython-314-pytest-9.0.3.pyc` is generated output, not test source.

---

*Testing analysis: 2026-05-14*
