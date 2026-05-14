# Integration Map

## Snapshot

LATTICE has four main integration surfaces: model providers and transports, CLI/TUI to runtime wiring, plugin and agent manifests, and local system capabilities such as sandboxing, sessions, policy, and filesystem/git awareness. The top-level `Runtime` composes model routing, agent state, tool execution, plugin overlays, session storage, and security configuration.

Evidence: `README.md`, `AGENTS.md`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/runtime.rs`.

## LLM Provider Integration

- The provider catalog is embedded in `crates/lattice-core/src/catalog/data.json` and lazily loaded by `Catalog::get()` through `include_str!`.
- Model resolution returns `ResolvedModel`, which includes canonical id, provider, base URL, API protocol, provider-specific model id, context length, provider-specific fields, and credential status.
- `ModelRouter` supports catalog aliases, provider override, credentialed and credentialless provider priority, `provider/model` syntax, and custom model fallback.
- Credentials are loaded by the CLI-level `CredentialStore` and passed into the runtime builder. The intended direction is explicit credential passing rather than mutating global environment variables inside the runtime.
- Evidence: `crates/lattice-core/src/catalog/loader.rs`, `crates/lattice-core/src/catalog/types.rs`, `crates/lattice-core/src/router.rs`, `crates/lattice-cli/src/credentials.rs`, `crates/lattice-cli/src/runtime.rs`.

## Transport And Protocol Integration

| Protocol | Default transport | Notes |
| --- | --- | --- |
| `chat_completions` | `ChatCompletionsTransport` | Generic OpenAI-compatible provider path. |
| `anthropic_messages` | `AnthropicTransport` | Anthropic Messages format. |
| `gemini_generate_content` | `GeminiTransport` | Gemini Generate Content format. |
| `codex_responses` | Not registered by default | The enum variant exists; the dispatcher can register a custom transport. |
| custom | Not registered by default | `ApiProtocol::Custom(String)` lets the catalog express unknown protocols. |

Evidence: `crates/lattice-core/src/catalog/types.rs`, `crates/lattice-core/src/transport/dispatcher.rs`.

## Configuration Files And Environment Variables

- Default config path is `dirs::config_dir()/lattice/config.toml`; otherwise the fallback is `lattice.toml`.
- Config sections:
  - `core.default_model`, `core.stream`, `core.save_sessions`
  - `ui.theme`, `ui.show_reasoning`
  - `security.sandbox_mode`, allowlists, hook chain, Landlock, audit, and size limits
  - `providers.<name>.api_key`, `providers.<name>.base_url`
- Provider API keys should be expressed as `$ENV_NAME`; literal keys are supported but produce a warning.
- `.env` is ignored by `.gitignore`.
- The CLI depends on `dotenvy`.
- README examples mention `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, and `DEEPSEEK_API_KEY`.
- Evidence: `crates/lattice-cli/src/config.rs`, `crates/lattice-cli/src/credentials.rs`, `README.md`, `.gitignore`.

## CLI Integration

- `lattice-cli` provides the `lattice` binary.
- Common commands include:
  - `lattice run`
  - `lattice code`
  - `lattice resolve`
  - `lattice models`
  - `lattice tui`
  - `lattice sessions`
  - `lattice config`
  - `lattice debug`
  - `lattice doctor`
- The CLI connects config loading, credential loading, security config conversion, and runtime construction.
- Evidence: `crates/lattice-cli/Cargo.toml`, `crates/lattice-cli/src/main.rs`, `crates/lattice-cli/src/commands/`, `crates/lattice-cli/src/runtime.rs`.

## TUI Integration

- The TUI uses `ratatui` and `crossterm` for terminal UI and event handling.
- The TUI contains a slash command registry with commands such as `/model`, `/provider`, `/status`, `/permissions`, `/approve`, `/plugins`, `/plugin`, and `/sessions restore`.
- Bash-prefixed commands enter the TUI `bash_command` path and call `RuntimeSecurity::execute_bash`, so they are constrained by sandbox and security configuration.
- The TUI connects to runtime turn execution and displays tokens, reasoning, tool calls, approval proposals, plugin mode state, and session state.
- Evidence: `crates/lattice-cli/src/frontend/tui/state.rs`, `crates/lattice-cli/src/frontend/tui/commands.rs`, `crates/lattice-cli/src/frontend/tui/app.rs`, `crates/lattice-cli/src/frontend/tui/bash_command.rs`.

## Session And SQLite Integration

- `lattice-session` provides the `SessionStore` abstraction.
- The session layer supports file-backed sessions and a SQLite CAS session store.
- The session graph supports branch, checkout, tag, reset, diff, stash, reflog, garbage collection, and turn manifest reverse indexes.
- Runtime turns can receive a session store and persist or restore conversation and task-state metadata at completion boundaries.
- Agent-level optional storage:
  - `sqlite-memory`: `rusqlite` plus FTS5 memory recall.
  - `blob-store`: `sqlx` SQLite storage for large context blobs, exposed in prompts as references.
- Evidence: `crates/lattice-session/src/lib.rs`, `crates/lattice-runtime/src/turn.rs`, `crates/lattice-agent/src/memory/sqlite.rs`, `crates/lattice-agent/src/blob.rs`.

## Sandbox, Policy, And Approval Integration

- `lattice-policy` is the canonical layer for approval, denial, risk, and validation types.
- `lattice-sandbox` performs path, command, and URL checks and re-exports policy types for backward-compatible imports.
- `SandboxConfig` supports:
  - read/write allowlists
  - URL allowlists
  - command allowlists
  - dynamic approved rules
  - `PolicyAuthority`
  - `AuditSink`
  - `LandlockConfig`
  - command timeouts and max read/write/http response sizes
- `RuntimeSecurityConfig` converts runtime-level sandbox mode, allowlists, audit directory, hook chain, Landlock flag, and limits into `RuntimeSecurity`.
- Approval flow can emit proposals, and the TUI can feed approvals back through `/approve`.
- Evidence: `crates/lattice-policy/src/lib.rs`, `crates/lattice-sandbox/src/config.rs`, `crates/lattice-runtime/src/lib.rs`, `crates/lattice-cli/src/frontend/tui/commands.rs`.

## Plugin And Agent Manifest Integration

- `lattice-plugin` provides the typed `Plugin` trait, erased runner, bundle support, manifest loader, registry, watcher, and DAG runner.
- Official plugins live in `lattice-plugins`; default features enable code review, refactor, test generation, security audit, documentation generation, PPTX, deep research, image generation, knowledge base, plan generation, and verification.
- `examples/plugins/hello/plugin.toml` is a plugin manifest example.
- `examples/agents/*/agent.toml` are agent profile examples.
- The orchestrator/profile layer lowers agent profiles, tools, plugins, prompt specs, and output schemas into runtime turn specs.
- Evidence: `crates/lattice-plugin/src/lib.rs`, `crates/lattice-plugin/src/manifest.rs`, `crates/lattice-plugin/src/registry.rs`, `crates/lattice-orchestrator/src/profile.rs`, `crates/lattice-orchestrator/src/assembly.rs`, `examples/plugins/hello/plugin.toml`, `examples/agents/`.

## Filesystem, Git, And Workspace Integration

- The prompt layer reads workspace and project instructions and has tests for filtering directories such as `.git`, `target`, and `node_modules`.
- Agent final reports and TUI activity code handle git status, diff display, and file diff reads.
- CLI session paths are derived through `dirs`.
- Local paths such as `.env`, `.planning/`, `.agents/`, `.codex/`, and `findings/` are excluded by `.gitignore`.
- Several CLI tests create temporary git repositories to validate `code` and runtime turn behavior.
- Evidence: `crates/lattice-prompt/src/workspace_provider.rs`, `crates/lattice-agent/src/final_report.rs`, `crates/lattice-cli/src/frontend/tui/activity.rs`, `crates/lattice-cli/tests/code_agent_eval.rs`, `.gitignore`.

## External Commands And Scripts

- Standard Rust toolchain commands are documented in AGENTS/CLAUDE/README: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --all --check`, and `cargo bench`.
- `crates/lattice-core/build.rs` can call `python3-config --ldflags --embed` for libpython linkage in test binaries; missing command support is treated as a no-op.
- `scripts/rewrite-links.py` and `scripts/generate-summary.py` support wiki/mdBook link handling.
- `tests/test_rewrite_links.py` covers script behavior.
- Evidence: `AGENTS.md`, `crates/lattice-core/build.rs`, `scripts/`, `tests/test_rewrite_links.py`.

## Network And Local Service Integration

- Provider HTTP calls use `reqwest`, with transport layers normalizing requests and responses.
- `axum` is an optional feature in `lattice-core` and `lattice-orchestrator`, intended for HTTP/WebSocket extension surfaces; default crate features are empty.
- Local and credentialless providers are represented through catalog/provider defaults. Router tests use Ollama-style providers as credentialless examples.
- Evidence: `Cargo.toml`, `crates/lattice-core/Cargo.toml`, `crates/lattice-orchestrator/Cargo.toml`, `crates/lattice-core/src/router.rs`.

## Current Notes

- The root instruction include list references `RTK.md`, but no `RTK.md` file exists at the repository root.
- This map records the missing file and does not create a placeholder.
- `.planning/` is ignored by `.gitignore`; these documents need a forced add when they are meant to be reviewed or committed.
- `ApiProtocol::CodexResponses` exists at the type level, but the default transport dispatcher does not register an implementation. Any integration plan using it should first confirm the intended protocol path.
