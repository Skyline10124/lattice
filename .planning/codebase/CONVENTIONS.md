<!-- refreshed: 2026-05-19 -->
# Coding Conventions

**Analysis Date:** 2026-05-19

## Naming Patterns

**Files**

- Rust source files use `snake_case`, for example `agent_loop/mod.rs`, `enabled_tools.rs`, `sqlite_graph.rs`, `tool_error.rs`.
- Crate names use `lattice-*` in kebab-case, for example `lattice-llm`, `lattice-hooks`, `lattice-api`. Directory names under `crates/` match: `crates/lattice-llm/`, `crates/lattice-agent/`.
- Public crate entry points are `src/lib.rs`. The CLI binary entry point is `crates/lattice-cli/src/main.rs`.
- Crate-level integration tests live under `crates/*/tests/*.rs`; unit tests are inline under `#[cfg(test)] mod tests` (or named modules such as `mod session_graph`).
- No `mod.rs` is used at the crate root. Multi-file modules use a directory plus an internal `mod.rs`, for example `crates/lattice-agent/src/agent_loop/mod.rs`, `crates/lattice-tools/src/edit/mod.rs`, `crates/lattice-tools/src/executor/mod.rs`, `crates/lattice-llm/src/providers/mod.rs`.

**Functions**

- Functions and methods use `snake_case`, for example `AgentLoop::run_turn()`, `SessionApi::create_session()`, `lattice_llm::init_logging()`, `lattice_api::run_turn()`, `DefaultToolExecutor::for_project_root()`.
- Builder/factory helpers use `make_*`, `setup_*`, `build_*`, or `for_*`, for example `make_request()`, `setup_workspace()`, `build_agent_loop()`, `DefaultToolExecutor::for_project_root()`.
- Test names describe behavior with the `subject_condition_expected_result` pattern, for example `agent_loop_unknown_enabled_tools_fails_before_llm_call`, `read_file_supports_relative_paths_line_ranges_and_line_numbers`.

**Variables**

- Local variables use `snake_case`, for example `session_id`, `project_root`, `agent_loop`, `loaded_profile`.
- Module-level constants use `SCREAMING_SNAKE_CASE`, for example `MAX_BRANCH_NAME_LEN` in `crates/lattice-session/src/lib.rs`, `MAX_TURN_ROUNDS` referenced by `crates/lattice-agent/tests/agent_loop.rs:794`.
- Environment variables use `SCREAMING_SNAKE_CASE`: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `LATTICE_AGENTS_DIR`, `XDG_CONFIG_HOME`.

**Types**

- Structs, enums, and traits use `CamelCase`, for example `AgentLoop`, `ResolvedModel`, `ChatRequest`, `SessionGraphStore`, `RunTurnRequest`, `LoadedAgentProfile`.
- Error types end with `Error`, for example `LlmError` (`crates/lattice-llm/src/stub.rs`), `SessionError` (`crates/lattice-session/src/lib.rs`), `ProfileError` (`crates/lattice-profile/src/error.rs`), `ApiError` (`crates/lattice-api/src/session.rs`), `ToolError` (`crates/lattice-tools/src/tool_error.rs`).
- Status enums end with `Status`, for example `AgentTurnStatus`, `RunTurnStatus`, `CredentialStatus`, `TurnCommitStatus`.
- Event enums end with `Event`, for example `LlmEvent`, `AgentTurnEvent`, `TurnEvent`.

## Code Style

**Formatting**

- `cargo fmt --all --check` is the formatting gate. There is no `rustfmt.toml`; rustfmt defaults under Rust 2021 apply.
- The CI workflow at `.github/workflows/ci.yml` runs `cargo fmt --all --check` before clippy and tests.
- Avoid unrelated whole-workspace formatting churn.

**Linting**

- Clippy runs in deny-warnings mode: `cargo clippy --workspace -- -D warnings` (see `.github/workflows/ci.yml:23`).
- There is no `clippy.toml`; default clippy lints apply.
- Inline `#[allow(...)]` overrides are rare and scoped to a specific item, for example `#[non_exhaustive]` and `#[cfg(test)]`-gated helpers.
- New code should eliminate warnings rather than rely on broad crate-level allows.

**Rust Edition and MSRV**

- The workspace uses Rust 2021 edition and `rust-version = "1.85"` with `resolver = "2"` (see `Cargo.toml:1-17`).
- CI uses `dtolnay/rust-toolchain@stable`. The `1.85` MSRV is not pinned in CI.

## Import Organization

The observed import order, consistent across crates:

1. **Standard library imports** -- `std::path::{Path, PathBuf}`, `std::sync::{Arc, Mutex}`, `std::sync::atomic::{AtomicBool, AtomicUsize, Ordering}`.
2. **Third-party crate imports** -- `serde::{Deserialize, Serialize}`, `tokio::time::sleep`, `tracing::warn`, `thiserror::Error`, `clap::{Parser, Subcommand}`, `colored::Colorize`, `anyhow::Result`.
3. **Internal workspace crate imports** -- `lattice_llm::ResolvedModel`, `lattice_session::{GraphMessage, HeadTranscript}`, `lattice_tools::executor::DefaultToolExecutor`.
4. **Current-crate module imports** -- `crate::session::ApiError`, `crate::turn::deps::TurnDeps`, `super::*` (in test modules).

Cross-crate dependencies use workspace names: `lattice-llm = { workspace = true }` in crate `Cargo.toml` files. All third-party version pins live centrally in `Cargo.toml:[workspace.dependencies]`.

## Error Handling

**Patterns**

- Library crates use typed error enums with `#[derive(Debug, thiserror::Error)]`. Examples:
  - `LlmError` in `crates/lattice-llm/src/stub.rs:14-20`
  - `SessionError` in `crates/lattice-session/src/lib.rs:26-49`
  - `ProfileError` in `crates/lattice-profile/src/error.rs:3-105`
  - `ApiError` in `crates/lattice-api/src/session.rs:15-22`
  - `ToolError` in `crates/lattice-tools/src/tool_error.rs:3-30`
- Error variants carry typed structured fields rather than free-form strings (see `ProfileError::InvalidField` and `SessionError::Io`).
- `#[source]` attaches the underlying cause, used in `SessionError::Io { source: std::io::Error }`, `ProfileError::Toml { source: toml::de::Error }`, `ToolError::IoError`.
- `#[non_exhaustive]` is used for crate-private error enums that may grow (`ToolError`).
- Module-level `pub type Result<T> = std::result::Result<T, SpecificError>;` aliases exist in `lattice-session` and `lattice-api`.
- CLI command entry points use `anyhow::Result` to aggregate errors from multiple sources (`crates/lattice-cli/src/commands/run.rs:1`, `crates/lattice-cli/src/main.rs:1`).
- Error mapping at boundaries is explicit: `lattice-api` maps `SessionError::NotFound` to `ApiError::NotFound` and everything else to `ApiError::Internal` (`crates/lattice-api/src/session.rs:131-136`).

**Do**

- Return `Result<T, SpecificError>` from library code.
- Use `anyhow::bail!()` or `anyhow::anyhow!()` only in CLI glue.
- Preserve typed metadata via `thiserror` `#[error]` template strings.
- Convert errors at crate boundaries with explicit `match` or `map_err` rather than blanket `From` impls when the mapping needs to flatten variants.

**Avoid**

- Do not use `panic!` for recoverable production-path errors. `unwrap()` is acceptable in tests and in `Default::default()` paths.
- Do not drive control flow by parsing error strings.
- Do not let `anyhow::Error` leak across library boundaries -- it stays in CLI/binary code.

## Async and Concurrency

- **Tokio** is the primary runtime. The CLI binary uses `#[tokio::main]` (`crates/lattice-cli/src/main.rs:64`).
- Workspace dependency: `tokio = { version = "1", features = ["full"] }` (`Cargo.toml:43`).
- Async traits use `#[async_trait::async_trait]`. Key trait surfaces:
  - `LlmClient` in `crates/lattice-agent/src/agent_loop/traits.rs`, exposed as `lattice_agent::LlmClient`
  - `ToolCallExecutor` in the same module
  - `ToolExecutor` in `crates/lattice-tools/src/executor/mod.rs`
  - `TurnDeps` (crate-private) in `crates/lattice-api/src/turn/deps.rs`
- `Arc<T>` wraps shared owners of async traits passed by `AgentLoopConfig` (`Arc<dyn LlmClient>`, `Arc<dyn ToolCallExecutor>`).
- Long-lived shared state in tests uses `Arc<std::sync::Mutex<...>>`. Production paths avoid holding a `std::sync::MutexGuard` across `.await`.
- Cooperative cancellation flows through `Arc<AtomicBool>` cancel flags on `AgentTurnRequest::cancel_flag`; `AgentTurnRequest::timeout` is enforced as a cooperative deadline around LLM stream creation, LLM stream consumption, and tool execution.

## Module Design

**Module Declarations**

- Crate roots declare modules and re-export the stable public API.
- Private modules use `mod foo;` (no `pub`), for example `mod client;`, `mod endpoints;`, `mod stub;` in `crates/lattice-llm/src/lib.rs`.
- Public modules use `pub mod foo;`, for example `pub mod security;`, `pub mod sse;` in `lattice-llm`.
- Multi-file modules live in directories with an internal `mod.rs` (the only place `mod.rs` is used). The crate root never uses `mod.rs`.

**Re-export Pattern**

- Each crate's `lib.rs` is the canonical export surface. No wildcard re-exports.
- `pub use` lifts items explicitly: see `crates/lattice-llm/src/lib.rs:13-19` and `crates/lattice-api/src/lib.rs:16-20`.
- Cross-crate visibility uses `pub(crate)` for items shared inside a crate (e.g., `MAX_BRANCH_NAME_LEN` in `lattice-session`).

**Layering (Dependency Flow)**

The documented dependency flow (from `crates/lattice/CLAUDE.md`):

```
hooks -> llm -> session/profile/tools -> agent -> api -> cli
```

`lattice-hooks` is declared in `[workspace.dependencies]` in `Cargo.toml:22` but is not currently a member of `[workspace] members` (`Cargo.toml:3-11`). Lower crates must not depend on upper crates.

**New Code Placement**

- New model/transport primitives: `crates/lattice-llm/src/`. Provider modules live under `crates/lattice-llm/src/providers/` (`anthropic.rs`, `openai.rs`, `gemini.rs`).
- New agent loop behavior: `crates/lattice-agent/src/agent_loop/` (`messages.rs`, `traits.rs`, `types.rs`).
- New API methods: `crates/lattice-api/src/` (`session.rs`, `turn/`).
- New CLI subcommand: add a module under `crates/lattice-cli/src/commands/` (mirroring `doctor.rs`, `models.rs`, `resolve.rs`, `run.rs`, `session.rs`), plus a variant in `Commands` enum and dispatch in `crates/lattice-cli/src/main.rs:28-62`.
- New profile fields: `crates/lattice-profile/src/schema.rs` plus tests, with errors in `crates/lattice-profile/src/error.rs`.
- New session features: `crates/lattice-session/src/graph.rs` (trait surface) and `crates/lattice-session/src/sqlite_graph.rs` (storage).
- New tool: register in `crates/lattice-tools/src/enabled_tools.rs`, schema in `crates/lattice-tools/src/tool_definitions.rs`, executor in `crates/lattice-tools/src/executor/`.
- New hook variant: `crates/lattice-hooks/src/lib.rs` (single-file crate today).

## Visibility Conventions

- Implementation modules default to private (`mod foo;`).
- Public APIs are explicitly lifted via `pub use` in `lib.rs`.
- Cross-crate visibility uses `pub(crate)` for types shared within a crate but not part of the public API (`SessionApi::store()` is gated by `#[cfg(test)] pub(crate)` at `crates/lattice-api/src/session.rs:53-56`).
- Test-only items use `#[cfg(test)]` plus `pub(crate)` or are placed inside `#[cfg(test)] mod tests`.
- Helpers shared between unit tests and downstream crate test suites live in `pub mod test_helpers` (see `lattice_session::test_helpers::{assistant_msg, user_msg}` referenced from `crates/lattice-api/src/session.rs:159`).

## Builder and Injectable Trait Patterns

`AgentLoop` uses injectable async traits for testability. Tests instantiate `AgentLoop::new(AgentLoopConfig { llm, tool_executor })` with `Arc<FakeLlm>` and `Arc<FakeToolExecutor>`:

```rust
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>;
}

#[async_trait::async_trait]
pub trait ToolCallExecutor: Send + Sync {
    async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, String>;
}
```

Production code provides real implementations (`RealLlmClient` in `crates/lattice-cli/src/commands/run.rs:20-33`, `DefaultToolExecutor` in `lattice-tools`); tests provide doubles (`FakeLlm`, `FakeToolExecutor`).

The `TurnDeps` trait (`crates/lattice-api/src/turn/deps.rs`) is the same pattern at the API layer: `RealTurnDeps` in production, `FakeTurnDeps` in `crates/lattice-api/src/turn/tests.rs`.

## Feature Flags

- Most core crates use `default = []` (see `crates/lattice-tools/Cargo.toml:32`, `crates/lattice-session/Cargo.toml:19`, `crates/lattice-cli/Cargo.toml:43-44`).
- Conditional compilation uses `#[cfg(unix)]` for platform-specific code (Unix-only signal handling in `crates/lattice-tools/src/executor/shell.rs` and the `#[cfg(unix)]` tests in `crates/lattice-tools/tests/executor.rs:339-400`).
- `#[cfg(test)]` gates inline tests, test-only constructors, and shared test helpers.

## Serialization

- Persistent data, cross-process contracts, tool calls, and API outputs derive `serde::{Serialize, Deserialize}` directly. Examples: `Message`, `ChatRequest`, `ChatResponse`, `LlmEvent` in `crates/lattice-llm/src/types.rs`.
- JSON construction uses `serde_json::json!` macro in test fixtures (`crates/lattice-tools/tests/executor.rs`, `crates/lattice-api/tests/usage_turn.rs`).
- TOML config parsing uses `toml`; comment-preserving edits use `toml_edit`. Profile schema lives in `crates/lattice-profile/src/schema.rs`.
- `#[serde(skip_serializing_if = "Option::is_none")]` is used on optional message fields to keep wire payloads small (`crates/lattice-llm/src/types.rs:52-59, 151-159`).
- `#[serde(default)]` is used on new fields that need backward-compatible deserialization (in profile schema).

## Logging

- Logging uses `tracing` with the standard levels: `trace`, `debug`, `info`, `warn`, `error`.
- Initialization is centralized in `crates/lattice-llm/src/logging.rs` with `init_logging(verbose: bool)`. The CLI entry calls it once based on the `--verbose` flag (`crates/lattice-cli/src/main.rs:69-74`).
- Subscriber configuration uses `tracing-subscriber` with `env-filter`, `fmt`, and `time` features (`Cargo.toml:46`).
- Do not log API keys, tokens, or complete sensitive request bodies. CLI smoke tests assert that secrets never appear in output (`crates/lattice-cli/tests/release_smoke.rs:120-123`).

## Comments

- Use `//!` for module/crate-level documentation, for example `//! LATTICE LLM vocabulary and functional call facade.` at `crates/lattice-llm/src/lib.rs:1`.
- Use `///` for public item documentation. Most public types in `crates/lattice-llm/src/types.rs` and `crates/lattice-llm/src/router.rs` are documented this way.
- Use `//` for in-function explanations of non-obvious logic. Avoid comments that restate code.
- Section dividers in test files use Unicode box-drawing comments: `// ─── models --auth ─────...` (`crates/lattice-cli/tests/release_smoke.rs:95`).

## Function Design

- Small pure helpers stay near their tests or in the same module.
- Constructor patterns prefer explicit struct literals or `new`/`for_*` factory functions. Builders are reserved for multi-parameter, multi-step construction (`AgentLoopConfig` is a plain struct passed to `AgentLoop::new`, not a builder).
- Public APIs accept `impl Into<String>` for ergonomic string parameters (`Message::user(content: impl Into<String>)`, `FunctionCall::new`, `ToolDefinition::new`).
- Library APIs return `Result<T, SpecificError>`; CLI APIs return `anyhow::Result<()>` or `std::process::ExitCode`.

## CLI Patterns

- Commands are defined in the `Commands` enum at `crates/lattice-cli/src/main.rs:28-62`.
- Commands use `clap::{Parser, Subcommand}` derive with `#[command]` and `#[arg]` attributes.
- Supported subcommands: `run`, `models`, `resolve`, `session`, `doctor` (verified by `crates/lattice-cli/tests/cli_cutover.rs:18-65`).
- Subcommand implementations live one per file under `crates/lattice-cli/src/commands/`.
- Global flags `--json` and `--verbose` are declared on `Cli` with `global = true`.
- Exit codes are explicit:
  - `0` -- success.
  - `1` -- runtime failure (turn `Error` or `Cancelled`).
  - `2` -- input/configuration error (missing profile, missing credentials, missing session).
  See header doc at `crates/lattice-cli/src/commands/run.rs:1-7`.
- Error printing uses `colored::Colorize` for the `error` prefix in red (`crates/lattice-cli/src/main.rs:97`).
- `dotenvy::dotenv().ok()` loads `.env` once at the top of `main` (`crates/lattice-cli/src/main.rs:66`).

---

*Convention analysis: 2026-05-19*
