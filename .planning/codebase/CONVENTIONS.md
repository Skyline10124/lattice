# Coding Conventions

**Analysis Date:** 2026-05-14

## Naming Patterns

**Files**

- Rust crates use `lattice-*` kebab-case directory names under `crates/`, for example `crates/lattice-core/Cargo.toml` and `crates/lattice-runtime/Cargo.toml`.
- Rust modules and source files use snake_case, for example `crates/lattice-core/src/chat_completions.rs`, `crates/lattice-cli/src/frontend/tui/agent_runner.rs`, and `crates/lattice-prompt/src/project_instructions_provider.rs`.
- Public crate entry points are `src/lib.rs`. The CLI binary entry point is `crates/lattice-cli/src/main.rs`. Crate-level integration tests live under `crates/*/tests/*.rs`; many unit tests are inline under `#[cfg(test)] mod tests`.

**Functions**

- Functions and methods use snake_case, for example `Runtime::builder()`, `Runtime::resolve()`, `Config::load()`, and `lattice_core::init_logging()`.
- Helper constructors commonly use `make_*`, `spawn_*`, `build_*`, or `render_*`, for example `make_resolved_model()`, `spawn_mock_openai_server()`, and `build_runtime_security()`.
- Test names usually describe behavior rather than implementation details, often in `component_condition_expected_result` style.

**Variables**

- Local variables use snake_case, for example `base_url`, `provider_override`, and `session_id`.
- Environment variables and constants use SCREAMING_SNAKE_CASE, for example `LATTICE_LOG` and `ALL_CREDENTIAL_ENV_VARS`.
- Shared-state fields are named for ownership and purpose, for example `approved_rules`, `security_by_key`, and `latest_turn_completion`.

**Types**

- Structs, enums, and traits use PascalCase, for example `Runtime`, `RuntimeBuilder`, `ResolvedModel`, `StreamEvent`, `Plugin`, and `ToolExecutor`.
- Error types end with `*Error`, for example `LatticeError`, `RuntimeError`, `SessionError`, and `ValidationError`.
- Serializable enums intended for external JSON usually use `#[serde(rename_all = "snake_case")]` or tagged enums, for example `TodoStatus`, `RiskClass`, and `DenialKind`.

Evidence: `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/config.rs`, `crates/lattice-core/src/logging.rs`, `crates/lattice-core/tests/e2e/chat_mock.rs`, `crates/lattice-agent/src/state.rs`, `crates/lattice-policy/src/risk.rs`, `crates/lattice-policy/src/denial.rs`.

## Code Style

**Formatting**

- `cargo fmt --all --check` is the formatting gate documented in `AGENTS.md`, `CLAUDE.md`, and `README.md`.
- No `rustfmt.toml` was detected, so Rustfmt defaults and Rust 2021 style apply.
- Avoid unrelated whole-workspace formatting churn. Format only files touched by the task unless the task explicitly requests a workspace-wide formatting pass.

**Linting**

- Clippy runs in warning-as-error mode. Local docs use `cargo clippy -- -D warnings`; CI uses `cargo clippy --workspace -- -D warnings`.
- Lint allowances should be local and justified. Current examples include `#![allow(clippy::new_without_default)]` in `crates/lattice-plugin/src/lib.rs` and test/library allowances in `crates/lattice-cli/src/lib.rs`.
- New code should eliminate warnings rather than rely on broad crate-level allows.

**Rust Edition And MSRV**

- The workspace uses Rust 2021 and `rust-version = "1.85"`.
- The workspace uses `resolver = "2"`.
- New feature flags and optional dependencies should be propagated explicitly through crate features.

Evidence: `Cargo.toml`, `AGENTS.md`, `CLAUDE.md`, `.github/workflows/ci.yml`, crate `Cargo.toml` files.

## Import Organization

The observed import order is usually:

1. Standard library imports such as `std::collections::HashMap` and `std::sync::{Arc, Mutex}`.
2. Third-party imports such as `serde::{Deserialize, Serialize}`, `tokio::sync::mpsc`, and `tracing::warn`.
3. Internal crate imports such as `lattice_core::ResolvedModel` and `lattice_tools::ToolExecutor`.
4. Current-crate module imports such as `crate::config::Config`.

Examples:

- `crates/lattice-agent/src/state.rs` imports `std`, then `lattice_core` and `serde`, then current-crate modules.
- `crates/lattice-runtime/src/lib.rs` imports `std` and `futures`, then `lattice_*` crates, and later re-exports public runtime types.
- `crates/lattice-cli/src/main.rs` declares modules first, then groups command imports.

There are no TypeScript-style aliases. Internal Rust dependencies are declared through workspace dependency names such as `lattice-core = { workspace = true }`. Within a crate, use `crate::...`; across crates, use public APIs through the workspace crate name.

## Error Handling

**Patterns**

- Core libraries and cross-crate APIs use typed error enums with `thiserror`, for example `LatticeError`, `SessionError`, and `RuntimeError`.
- CLI layers use `anyhow::Result` to aggregate user-facing command failures.
- Recoverable policy denials use structured reports rather than plain strings, for example `DenialReport`, `PolicyDecision`, and `ValidationError`.
- Provider and transport error paths should avoid leaking credentials. `crates/lattice-core/src/errors.rs` contains credential scrubbing and error-body truncation logic.

**Do**

- Return `Result<T, SpecificError>` from library code.
- Use `anyhow::bail!` or `anyhow::anyhow!` in CLI glue when adding user-facing context.
- Preserve typed metadata for security, sandbox, and approval errors so the agent layer can generate approval proposals.

**Avoid**

- Do not use `panic!` for recoverable production-path errors. Existing `panic!` calls are mostly test assertions and mock-server setup failures.
- Do not drive security decisions by parsing arbitrary error strings. Prefer typed denials and typed policy decisions.

Evidence: `crates/lattice-core/src/errors.rs`, `crates/lattice-session/src/lib.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/main.rs`, `crates/lattice-policy/src/denial.rs`, `crates/lattice-agent/src/tool_result.rs`.

## Async And Concurrency

- Tokio is the primary runtime. The CLI entry point uses `#[tokio::main]`.
- Async agent execution, runtime turns, transport, and tool executor paths all run on Tokio.
- Async traits use `async_trait` or `async_trait(?Send)` in hook, tool, and plugin-adapter surfaces.
- Long-lived shared state uses `Arc<Mutex<...>>`, `RwLock`, or Tokio channels depending on the use case.
- `SharedTaskState = Arc<Mutex<TaskState>>` lives in `crates/lattice-agent/src/state.rs`.
- Runtime turn events use `tokio::sync::mpsc` in `crates/lattice-runtime/src/turn.rs`.
- Registry-style shared state uses `RwLock`, for example in `crates/lattice-plugin/src/registry.rs`.
- New async code should avoid holding a `std::sync::MutexGuard` across `.await`. Snapshot needed data before awaiting, or use an appropriate Tokio primitive.
- Frontend code should consume `RuntimeTurnEvent` and `TurnCompletion`; it should not directly own a live `Agent`. This is guarded by `crates/lattice-cli/tests/frontend_runtime_guards.rs`.

## Configuration

**Workspace**

- Shared dependency versions live in root `[workspace.dependencies]`.
- Crates reference shared versions with `{ workspace = true }`.
- Shared package metadata lives in `[workspace.package]`: `version`, `license`, `edition`, `rust-version`, and `authors`.

**CLI Config**

- CLI config lives in `crates/lattice-cli/src/config.rs`.
- Config uses `serde` + TOML and relies on `#[serde(default)]` or `default_*` functions for missing fields.
- Default config path is `dirs::config_dir()/lattice/config.toml`.
- Missing config returns defaults rather than failing.
- Config editing uses `toml_edit` to preserve comments.

**Environment**

- CLI startup calls `dotenvy::dotenv().ok()`.
- `.env.example` documents provider keys; `.env` must not be read into docs or committed.
- Project docs describe copying `.env.example` to `.env`.

Evidence: `Cargo.toml`, `crates/lattice-cli/src/config.rs`, `crates/lattice-cli/src/commands/config_cmd.rs`, `crates/lattice-cli/src/main.rs`, `.gitignore`.

## Module Design

**Exports**

- Crate roots declare modules and re-export stable public APIs.
- `crates/lattice-core/src/lib.rs` re-exports key core types such as `ResolvedModel`, `LatticeError`, and `StreamEvent`.
- `crates/lattice-runtime/src/lib.rs` re-exports runtime surface types such as `AgentHandleId`, `RuntimeTurnEvent`, and `TurnCompletion`.
- `lattice-policy` owns policy types, while `lattice-sandbox` re-exports them for compatibility.
- Keep implementation modules private by default. Promote APIs through `pub mod` or `pub use` only when they are intended to be part of the crate surface.

**Layering**

- Follow the documented dependency flow: `hooks -> core -> session/prompt/sandbox/tools -> agent -> plugin -> orchestrator -> runtime -> cli`.
- Do not make lower crates depend on upper crates.
- `crates/lattice-cli/tests/frontend_runtime_guards.rs` guards against the frontend re-owning `lattice_agent::Agent` or bypassing runtime turn completion.

**New Code Placement**

- New model/transport primitives: `crates/lattice-core/src/`.
- New agent turn or state behavior: `crates/lattice-agent/src/`, with inline tests or `crates/lattice-agent/tests/`.
- New runtime wiring or public runtime API: `crates/lattice-runtime/src/`.
- New CLI command: `crates/lattice-cli/src/commands/`, plus registration in `crates/lattice-cli/src/main.rs`.
- New TUI view/state behavior: `crates/lattice-cli/src/frontend/tui/`, following app/state/ui/widgets/interaction boundaries.
- New policy/sandbox rule: canonical type in `crates/lattice-policy/src/`, compatibility re-export in `crates/lattice-sandbox/src/lib.rs` if needed.

## Feature Flags

- Most core crates use `default = []`.
- Optional capabilities include `blob-store`, `sqlite-memory`, and `axum`.
- `lattice-plugins` is the exception: default features enable all official plugins.
- Conditional compilation uses `#[cfg(feature = "...")]` around modules, re-exports, and call sites.
- New optional capabilities should update crate `Cargo.toml` features, downstream feature propagation, call-site `#[cfg]`, tests, and planning docs.
- Feature propagation should be explicit. For example, `lattice-runtime/blob-store` forwards to `lattice-agent/blob-store` and `lattice-orchestrator/blob-store`.

Evidence: `crates/lattice-core/Cargo.toml`, `crates/lattice-agent/Cargo.toml`, `crates/lattice-runtime/Cargo.toml`, `crates/lattice-orchestrator/Cargo.toml`, `crates/lattice-plugins/Cargo.toml`, `crates/lattice-plugins/src/lib.rs`.

## Serialization

- Persistent data, cross-process contracts, tool calls, and API outputs use `serde::{Serialize, Deserialize}`.
- JSON construction usually uses `serde_json::json!`, especially in test fixtures, CLI JSON output, and plugin context.
- TOML config parsing uses `toml`; comment-preserving edits use `toml_edit`.
- External JSON contracts should have round-trip or field-default tests.
- Examples include `TaskState` serde tests in `crates/lattice-agent/src/state.rs` and `AgentHandleSnapshot` serde tests in `crates/lattice-runtime/src/handle.rs`.

Evidence: `crates/lattice-session/src/lib.rs`, `crates/lattice-agent/src/state.rs`, `crates/lattice-runtime/src/handle.rs`, `crates/lattice-cli/src/config.rs`, `crates/lattice-cli/src/commands/config_cmd.rs`.

## Logging

- Logging uses `tracing`.
- Initialization is centralized in `crates/lattice-core/src/logging.rs`.
- Non-verbose default logging is compact and error-level; verbose/debug paths enable more detailed output.
- `LATTICE_LOG` overrides the default log level.
- Debug command logging uses `init_debug_logging()` and writes trace-level files. On Unix, debug log paths must be absolute and files use `0600` permissions.
- Runtime/background code uses `tracing::{warn, info, error, debug, trace}`.
- Do not log API keys, tokens, or complete sensitive request bodies.
- Trace logs may contain sensitive data, so path and permission requirements matter.

Evidence: `crates/lattice-core/src/logging.rs`, `crates/lattice-plugin/src/watcher.rs`, `crates/lattice-orchestrator/src/micro_agent.rs`, `crates/lattice-agent/src/audit.rs`.

## CLI / TUI Patterns

**CLI**

- Command definitions live in the `Commands` enum in `crates/lattice-cli/src/main.rs`.
- Commands use `clap::{Parser, Subcommand}` derive.
- Subcommand implementations live in `crates/lattice-cli/src/commands/`, for example `run.rs`, `debug.rs`, `config_cmd.rs`, and `sessions.rs`.
- The CLI entry point loads `.env`, initializes logging, loads config and credentials, then delegates to command modules or TUI.

**TUI**

- TUI uses `ratatui` + `crossterm`.
- TUI entry is `crates/lattice-cli/src/frontend/tui/mod.rs`.
- TUI must restore raw mode, alternate screen, mouse capture, and bracketed paste through `TerminalGuard` or explicit cleanup.
- TUI behavior is split across `event`, `action`, `update`, `app`, `ui`, `widgets`, and `agent_runner`.
- New runtime behavior should preserve runtime turn completion architecture rather than directly owning `Agent`.

## Comments

- Comment cross-layer protocols, runtime constraints, security boundaries, async models, and persistence formats.
- Useful examples include Tokio runtime constraints in `Agent::run()` and policy vocabulary documentation in `crates/lattice-policy/src/lib.rs`.
- Comment complex test fixtures and mock servers, for example `crates/lattice-core/tests/e2e.rs` and `crates/lattice-core/tests/e2e/chat_mock.rs`.
- Avoid comments that restate obvious getters, setters, or field assignments.
- Rust uses `///` doc comments and `//!` crate/module docs. JSDoc/TSDoc does not apply.
- Public types and modules should prefer Rust doc comments.

## Function Design

- Small pure helpers can stay near their tests.
- Larger orchestration code should be split into focused helpers or modules, as seen in runtime turn, CLI command, and TUI update/interaction code.
- Public builder/config APIs should use explicit config structures for multi-parameter construction, for example `RuntimeSecurityConfig` and `TuiOptions`.
- If a UI/CLI glue function genuinely needs many parameters, a local `clippy::too_many_arguments` allowance can be acceptable with a clear boundary.
- Library APIs return typed `Result`; CLI APIs return `anyhow::Result<()>`.
- Streaming APIs return `futures::Stream` or event collections. Semantic event types include `StreamEvent`, `LoopEvent`, and `RuntimeTurnEvent`.

Evidence: `crates/lattice-runtime/src/turn.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-cli/src/frontend/tui/update.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/frontend/tui/mod.rs`, `crates/lattice-core/src/streaming/mod.rs`.

## Notes

- The user-provided root instruction include list references `@RTK.md`, but `RTK.md` was not found at the repository root.
- Do not create `RTK.md` as part of codebase mapping.
- Available project guidance comes from `AGENTS.md`, `CLAUDE.md`, and `README.md`.
- `.env.example` exists, but this map did not inspect secret-bearing env files. Do not read or commit `.env`, `.env.*`, credential, or key files.
- `crates/lattice-core/build.rs` still contains the `python3-config --ldflags --embed` build helper. Changes to core build/test plumbing should account for local and CI Python embed linker behavior.

---

*Convention analysis: 2026-05-14*
