# Technology Stack Map

## Snapshot

- Project type: Rust workspace for a model-centric LLM engine.
- Product scope: model routing, agent execution, plugin runtime, federated orchestration, CLI, and TUI.
- Rust edition: workspace-wide `edition = "2021"`.
- Minimum Rust version: `rust-version = "1.85"`.
- License: Apache-2.0.
- Main binary: `lattice`, defined by `crates/lattice-cli`.
- Top-level library entry: `lattice-runtime::Runtime`.
- Evidence: `Cargo.toml`, `README.md`, `AGENTS.md`, `crates/lattice-cli/Cargo.toml`, `crates/lattice-runtime/src/lib.rs`.

## Workspace Crates

| Crate | Role | Evidence |
| --- | --- | --- |
| `lattice-hooks` | Lifecycle hook points for prompt, tool, agent, and session events. | `crates/lattice-hooks/Cargo.toml` |
| `lattice-core` | Model catalog, provider routing, transport adapters, streaming, tokens, and common errors. | `crates/lattice-core/Cargo.toml`, `crates/lattice-core/src/catalog/types.rs` |
| `lattice-session` | Conversation graph, git-style refs, file store, SQLite CAS store, and turn manifests. | `crates/lattice-session/Cargo.toml`, `crates/lattice-session/src/lib.rs` |
| `lattice-prompt` | Prompt providers, system stack, template rendering, workspace context, and project instructions. | `crates/lattice-prompt/Cargo.toml` |
| `lattice-sandbox` | Path, command, URL, Landlock, audit, and sandbox checks. | `crates/lattice-sandbox/Cargo.toml`, `crates/lattice-sandbox/src/config.rs` |
| `lattice-policy` | Canonical approval grants, denial reports, risk classification, and validation. | `crates/lattice-policy/Cargo.toml`, `crates/lattice-policy/src/lib.rs` |
| `lattice-tools` | Tool schemas, capability bundles, registry, executor, and structured edit engine. | `crates/lattice-tools/Cargo.toml` |
| `lattice-agent` | Async agent loop, state, memory, context manager, tool result handling, and manifests. | `crates/lattice-agent/Cargo.toml` |
| `lattice-plugin` | Typed plugin trait, erased runner, loader, registry, DAG runner, and built-in Superpowers bridge. | `crates/lattice-plugin/Cargo.toml` |
| `lattice-plugins` | Official plugin implementations exposed through feature gates. | `crates/lattice-plugins/Cargo.toml` |
| `lattice-orchestrator` | Profiles, agent assembly, pipelines, agent registry, event bus, and handoff rules. | `crates/lattice-orchestrator/Cargo.toml` |
| `lattice-runtime` | Top-level runtime builder and shared wiring policy for routing, sessions, security, and turns. | `crates/lattice-runtime/Cargo.toml` |
| `lattice-cli` | Binary entry point, commands, config, credentials, sessions, and TUI frontend. | `crates/lattice-cli/Cargo.toml` |

## Core Language And Runtime

- The primary implementation language is Rust.
- Async execution is based on Tokio. The workspace uses `tokio = { features = ["full"] }`.
- Many crates use `async-trait`, `futures`, and `tokio`.
- HTTP transport uses `reqwest` with `rustls-tls`, `stream`, and `json` features.
- CLI parsing uses `clap` derive/env support.
- CLI-level errors use `anyhow`; library-level errors generally use `thiserror`.
- Observability uses `tracing` and `tracing-subscriber` with env-filter, fmt, json, time, and local-time features.
- Serialization uses `serde`, `serde_json`, and `toml`; config editing uses `toml_edit`.
- Evidence: root `Cargo.toml` and crate-level `Cargo.toml` files.

## Model Routing And Transport

- `lattice-core` embeds `crates/lattice-core/src/catalog/data.json` with `include_str!("data.json")`.
- `ApiProtocol` includes `OpenAiChat`, `AnthropicMessages`, `GeminiGenerateContent`, `CodexResponses`, and `Custom(String)`.
- `TransportDispatcher` registers Chat Completions, Anthropic Messages, and Gemini Generate Content by default.
- `CodexResponses` exists at the type level, and custom transports can be registered, but the default dispatcher tests show it is not pre-registered.
- `ResolvedModel` carries canonical id, provider, base URL, provider model id, context length, provider-specific metadata, and credential status.
- API keys are hidden from `ResolvedModel` debug and serialization output.
- Evidence: `crates/lattice-core/src/catalog/types.rs`, `crates/lattice-core/src/catalog/loader.rs`, `crates/lattice-core/src/transport/dispatcher.rs`.

## Configuration And Credentials

- Default config path is `dirs::config_dir()/lattice/config.toml`; if unavailable, the fallback path is `lattice.toml`.
- `Config` is split into `core`, `ui`, `security`, and `providers`.
- The default model is `sonnet`; the default sandbox mode is `project`.
- `CredentialStore` loads credentials from provider config entries and known environment variables.
- `api_key = "$ENV_NAME"` reads an environment variable; literal API keys are accepted but produce a warning.
- Known credential variables include `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `MISTRAL_API_KEY`, `XAI_API_KEY`, `OPENROUTER_API_KEY`, `MINIMAX_API_KEY`, `QWEN_API_KEY`, `NOUS_API_KEY`, and `GITHUB_TOKEN`.
- Credential values are zeroized on drop, and debug output is redacted.
- Evidence: `crates/lattice-cli/src/config.rs`, `crates/lattice-cli/src/credentials.rs`.

## Persistence And Data

- `lattice-session` depends directly on `rusqlite` and implements sessions, branch refs, HEAD state, reflog, commit objects, turn manifests, and artifacts.
- `lattice-agent` has an optional `sqlite-memory` feature that uses `rusqlite` for FTS5-backed memory recall.
- `lattice-agent` also has an optional `blob-store` feature that uses `sqlx` with SQLite for large context blobs.
- The workspace `sqlx` dependency enables `runtime-tokio` and `sqlite`.
- Evidence: `crates/lattice-session/Cargo.toml`, `crates/lattice-session/src/lib.rs`, `crates/lattice-agent/src/memory/sqlite.rs`, `crates/lattice-agent/src/blob.rs`.

## Security And Isolation

- `lattice-sandbox` owns read/write allowlists, URL allowlists, command allowlists, dynamic approvals, policy authority integration, command timeout, and audit sinks.
- Linux builds can use the `landlock` crate; Unix builds use `libc` for platform-level resource handling.
- `RuntimeSecurityConfig` can enable Landlock, hook chains, audit logging, and size limits.
- `lattice-policy` is the canonical crate for approval, denial, risk, and validation types.
- `lattice-sandbox` re-exports policy types for compatibility with older imports.
- `SandboxConfig::default()` documents empty allowlists as unrestricted. Project-level restrictions should be created through project dirs or runtime `project`/`strict` security modes.
- Evidence: `crates/lattice-sandbox/src/config.rs`, `crates/lattice-policy/src/lib.rs`, `crates/lattice-runtime/src/lib.rs`.

## CLI And TUI

- CLI commands cover `run`, `code`, `resolve`, `models`, `sessions`, `config`, `debug`, `doctor`, and agent/profile workflows.
- The TUI uses `ratatui`, `crossterm`, and `unicode-width`.
- Markdown rendering uses `pulldown-cmark`; syntax highlighting uses `syntect`; diff display uses `similar`.
- TUI modules are split across app/update/ui/widgets/composer/markdown/pager/search/activity/plugin_mode/agent_runner concerns.
- Evidence: `crates/lattice-cli/Cargo.toml`, `crates/lattice-cli/src/frontend/tui/`.

## Plugins And Extensions

- `lattice-plugin` provides the `Plugin` trait, erased runner, loader, manifest parsing, registry, watcher, and DAG runner.
- `lattice-plugins` exposes official plugins through feature gates.
- Default plugin features are all enabled: `code-review`, `refactor`, `test-gen`, `security-audit`, `doc-gen`, `pptx-gen`, `deep-research`, `image-gen`, `knowledge-base`, `plan-gen`, and `verification`.
- Examples include plugin manifests and agent manifests.
- Evidence: `crates/lattice-plugin/src/lib.rs`, `crates/lattice-plugin/src/manifest.rs`, `crates/lattice-plugins/Cargo.toml`, `examples/plugins/hello/plugin.toml`, `examples/agents/`.

## Tests And Tooling

- Standard verification commands are `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --all --check`.
- Benchmarks use Criterion and appear in `benches/benchmarks.rs` and `crates/lattice-core/benches/benchmarks.rs`.
- Python script tests live in `tests/test_rewrite_links.py`; helper scripts live in `scripts/`.
- `crates/lattice-core/build.rs` tries `python3-config --ldflags --embed` to link test binaries against libpython. If the command fails, the build script returns without emitting link flags.
- Evidence: `AGENTS.md`, `Cargo.toml`, `crates/lattice-core/build.rs`, `tests/test_rewrite_links.py`.

## Notes

- The root-level instruction include list references `RTK.md`, but no `RTK.md` file exists at the repository root.
- This map records the missing file and does not create a placeholder.
- `.planning/` is ignored by `.gitignore`, so map files require `git add -f .planning/codebase/*.md` when they need to be committed.
