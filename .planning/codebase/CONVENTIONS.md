<!-- refreshed: 2026-05-17 -->
# Coding Conventions

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## Naming Patterns

**Files**

- Rust source files use `snake_case`, for example `agent_loop.rs`, `enabled_tools.rs`, `turn_tx.rs`.
- Crate names use `lattice-*` in kebab-case, for example `lattice-llm`, `lattice-hooks`, `lattice-api`. Directory names under `crates/` match: `crates/lattice-llm/`, `crates/lattice-agent/`.
- Public crate entry points are `src/lib.rs`. The CLI binary entry point is `crates/lattice-cli/src/main.rs`.
- Crate-level integration tests live under `crates/*/tests/*.rs`; unit tests are inline under `#[cfg(test)] mod tests`.

**Functions**

- Functions and methods use `snake_case`, for example `AgentLoop::run_turn()`, `Config::load()`, `lattice_llm::init_logging()`.
- Factory/builder helper constructors commonly use `make_*`, `build_*`, or `render_*`.
- Test names describe behavior rather than implementation details, using the `subject_condition_expected_result` pattern.

**Variables**

- Local variables use `snake_case`, for example `base_url`, `provider_override`, `session_id`.
- Environment variables and constants use `SCREAMING_SNAKE_CASE`.

**Types**

- Structs, enums, and traits use `CamelCase`, for example `AgentLoop`, `ResolvedModel`, `StreamEvent`.
- Error types uniformly end with `Error`, for example `LatticeError`, `SessionError`, `ProfileError`.
- Serializable enums use `#[serde(rename_all = "snake_case")]`.

## Code Style

**Formatting**

- `cargo fmt --all --check` is the formatting gate. No `rustfmt.toml` -- Rustfmt defaults and Rust 2021 style apply.
- Avoid unrelated whole-workspace formatting churn.

**Linting**

- Clippy runs in warning-as-error mode: `cargo clippy --workspace -- -D warnings`.
- Inline lint overrides are rare and scoped to the smallest possible item.
- New code should eliminate warnings rather than rely on broad crate-level allows.

**Rust Edition and MSRV**

- The workspace uses Rust 2021 edition and `rust-version = "1.85"` with `resolver = "2"`.

## Import Organization

The observed import order, consistent across crates:

1. **Standard library imports** -- `std::collections::HashMap`, `std::sync::{Arc, Mutex}`, `std::path::{Path, PathBuf}`
2. **Third-party crate imports** -- `serde::{Deserialize, Serialize}`, `tokio::io::{AsyncReadExt, AsyncWriteExt}`, `tracing::warn`, `thiserror::Error`
3. **Internal workspace crate imports** -- `lattice_llm::ResolvedModel`, `lattice_tools::ToolExecutor`
4. **Current-crate module imports** -- `crate::config::Config`

Cross-crate dependencies use workspace names: `lattice-llm = { workspace = true }` in crate `Cargo.toml` files.

## Error Handling

**Patterns**

- Core libraries and cross-crate APIs use typed error enums with `thiserror::Error`, for example `LatticeError`, `SessionError`, `ProfileError`.
- Each error variant carries typed, structured fields (not free-form strings).
- Some crates define module-level `Result<T>` type aliases.
- CLI layers use `anyhow::Result` to aggregate user-facing command failures.
- Tool denials use structured `ToolOutcome` rather than plain strings.

**Do**

- Return `Result<T, SpecificError>` from library code.
- Use `anyhow::bail!()` or `anyhow::anyhow!()` in CLI glue.
- Preserve typed metadata for tool execution outcomes.

**Avoid**

- Do not use `panic!` for recoverable production-path errors.
- Do not drive decisions by parsing arbitrary error strings.

## Async and Concurrency

- **Tokio** is the primary runtime. The CLI entry uses `#[tokio::main]`.
- Async traits use `#[async_trait]` from the `async_trait` crate on all async trait surfaces:
  - `LlmClient` and `ToolCallExecutor` in `lattice-agent`
  - `ToolExecutor` in `lattice-tools`
- Long-lived shared state uses `Arc<Mutex<...>>`.
- New async code should avoid holding a `std::sync::MutexGuard` across `.await`.

## Module Design

**Module Declarations**

- Crate roots declare modules and re-export stable public APIs.
- Private modules use `mod foo;` (no `pub`).
- Public modules use `pub mod foo;`.
- No `mod.rs` files. Single-file modules use the Rust 2018+ path convention.

**Re-export Pattern**

- Each crate's `lib.rs` is the canonical export surface. No wildcard re-exports.
- Use `pub use` to promote internal types to the public API, listing items explicitly.

**Layering (Dependency Flow)**

The documented dependency flow is:

```
hooks -> llm -> session/profile/tools -> agent -> api -> cli
```

Do not make lower crates depend on upper crates.

**New Code Placement**

- New model/transport primitives: `crates/lattice-llm/src/`.
- New agent loop behavior: `crates/lattice-agent/src/`.
- New API methods: `crates/lattice-api/src/`.
- New CLI command: `crates/lattice-cli/src/commands/`, plus registration in `crates/lattice-cli/src/main.rs`.
- New profile fields: `crates/lattice-profile/src/schema.rs`.
- New session features: `crates/lattice-session/src/`.
- New hook variant: `crates/lattice-hooks/src/`.

## Visibility Conventions

- Implementation modules default to private (`mod foo;`).
- Public API surfaces are explicitly lifted via `pub use` in `lib.rs`.
- Cross-crate visibility uses `pub(crate)` for types shared within a crate but not part of the public API.
- Test-only visibility uses `pub(super)` for helpers shared between test modules.

## Builder and Injectable Trait Patterns

**AgentLoop** uses injectable async traits for testability:

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, model: &ResolvedModel, messages: &[Message], tools: &[ToolDefinition]) -> Result<LlmResponse>;
}

#[async_trait]
pub trait ToolCallExecutor: Send + Sync {
    async fn execute(&self, tool_call: &ToolCall, project_root: &Path) -> Result<ToolResult>;
}
```

Production code provides real implementations; tests provide doubles.

## Feature Flags

- Most core crates use `default = []`.
- Conditional compilation uses `#[cfg(feature = "...")]` around module declarations, re-exports, and call sites.

## Serialization

- Persistent data, cross-process contracts, tool calls, and API outputs use `serde::{Serialize, Deserialize}`.
- JSON construction uses `serde_json::json!` macro in test fixtures and CLI JSON output.
- TOML config parsing uses `toml`; comment-preserving edits use `toml_edit`.
- `#[serde(default)]` is used on new fields to maintain backward compatibility.

## Logging

- Logging uses `tracing` with the standard levels: `trace`, `debug`, `info`, `warn`, `error`.
- Initialization is centralized in `crates/lattice-llm/src/logging.rs` with `init_logging()` and `init_debug_logging()`.
- Do not log API keys, tokens, or complete sensitive request bodies.

## Comments

- Use `//!` for module/crate-level documentation.
- Use `///` for public item documentation.
- Avoid comments that restate obvious code.

## Function Design

- Small pure helpers stay near their tests or in the same module.
- Public builder/config APIs use explicit config structures for multi-parameter construction.
- Library APIs return typed `Result<T, SpecificError>`; CLI APIs return `anyhow::Result<()>`.

## CLI Patterns

- Command definitions live in the `Commands` enum in `crates/lattice-cli/src/main.rs`.
- Commands use `clap::{Parser, Subcommand}` derive.
- Supported subcommands: `run`, `models`, `resolve`, `session`, `doctor`.
- Subcommand implementations live in `crates/lattice-cli/src/commands/`.

---

*Convention analysis: 2026-05-17 (post-Phase 11 deletion)*
