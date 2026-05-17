<!-- refreshed: 2026-05-17 -->
# Technology Stack

**Analysis Date:** 2026-05-17 (post-Phase 11 deletion)

## Snapshot

- **Project:** LATTICE v0.1.0 -- a model-centric LLM engine with CLI.
- **Workspace:** 8 crates, single Rust workspace.
- **Language:** Rust, edition 2021, MSRV 1.85.
- **License:** Apache-2.0.
- **Binary:** `lattice` defined in `crates/lattice-cli/Cargo.toml`.
- **API entry:** `SessionApi` and `run_turn`/`stream_turn` at `crates/lattice-api/src/lib.rs`.

## Workspace Crates

| Crate | Role | Key Dependencies |
|-------|------|-----------------|
| `lattice-hooks` | Lifecycle hook points (pre/post agent turn, tool call, inference, session) | tokio, serde, async-trait, tracing, sha2, thiserror |
| `lattice-llm` | Transport adapters, streaming, model resolution, routing, token estimation, errors, retry, logging | reqwest, tokio, tiktoken-rs, serde, serde_json, url, uuid, chrono, rand, regex, indexmap, tracing, thiserror, blake3 |
| `lattice-session` | Git-style conversation graph with SQLite persistence and branch refs | rusqlite, serde, serde_json, sha2, chrono, uuid, thiserror |
| `lattice-profile` | Agent profile specification, discovery, and defaults | serde, serde_json, toml, thiserror |
| `lattice-tools` | Tool schemas, enabled-tool resolver, executor with project-root containment, edit operations | reqwest, tokio, blake3, regex, url, tracing, thiserror |
| `lattice-agent` | Multi-turn async `AgentLoop` with injectable `LlmClient`/`ToolCallExecutor`, agent state, memory | tokio, reqwest, rusqlite (optional), regex, sha2, thiserror |
| `lattice-api` | Thin public facade for session/turn operations | tokio, serde, serde_json, thiserror |
| `lattice-cli` | Binary entry point (clap), subcommands (run, models, resolve, session, doctor), config, credentials | clap, dirs, dotenvy, colored, anyhow, zeroize, base64, sha2, toml_edit |

## Core Language And Runtime

- **Primary language:** Rust, edition 2021. Minimum supported Rust version: 1.85.
- **Async runtime:** Tokio v1 (full features).
- **Async trait bridging:** `async-trait` v0.1 for trait methods that are async.
- **Concurrency primitives:** `futures` v0.3 for composing async streams.
- **HTTP client:** `reqwest` v0.12 with `rustls-tls` (no OpenSSL dependency), `stream`, `json`.
- **CLI argument parsing:** `clap` v4 with `derive` and `env` features.
- **Error handling:** `thiserror` v2 for library-level typed errors; `anyhow` v1 for CLI-level error propagation.
- **Observability:** `tracing` v0.1 with `tracing-subscriber` v0.3.
- **Serialization:** `serde` v1 + `serde_json` v1 for JSON, `toml` v0.8 for TOML, `toml_edit` v0.22 for config mutations.
- **Encoding:** `sha2` v0.10, `blake3` v1 for content hashing, `base64` v0.22 for encoding.
- **Data structures:** `indexmap` v2 for order-preserving maps, `url` v2 for URL parsing, `uuid` v1 (v4).
- **Date/time:** `chrono` v0.4 for timestamp serialization.

## Model Routing And Transport

### Transport Layer (`crates/lattice-llm/src/transport/`)

Three transports implement a unified `Transport` trait:

| Transport | Protocol | Native Format |
|-----------|----------|---------------|
| `ChatCompletionsTransport` | OpenAI / OpenAI-compatible | `chat.completions` / `chat.completions.chunk` |
| `AnthropicTransport` | Anthropic Messages | `/v1/messages` |
| `GeminiTransport` | Gemini Generate Content | `models/{model}:generateContent` |

### Model Catalog (`crates/lattice-llm/src/catalog/`)

- Embedded catalog data at `data.json`, compiled via `include_str!`.
- Contains model entries with canonical IDs, providers, context lengths, and aliases.
- `ModelRouter` resolves model names to `ResolvedModel` with provider routing, credential resolution, and alias expansion.

### Streaming (`crates/lattice-llm/src/streaming/`)

Three SSE parsers: `OpenAiSseParser`, `AnthropicSseParser`, `GeminiSseParser`.
Unified `StreamEvent` enum: `Token`, `Reasoning`, `ToolCallStart`, `ToolCallDelta`, `ToolCallEnd`, `Done`, `Error`.

## Configuration And Credentials

### Config Loading (`crates/lattice-cli/src/config.rs`)

- Primary path: `dirs::config_dir()/lattice/config.toml`.
- Fallback path: `lattice.toml` (current directory).
- TOML format with sections: `core`, `ui`, `providers`.

### Credential Store (`crates/lattice-cli/src/credentials.rs`)

- Loads credentials from config `providers.<name>.api_key` entries and known environment variable names.
- Credentials are zeroized on `Drop`.
- Debug output redacts all credentials.

### Credential Injection

Credentials are passed explicitly into API calls, not via global `std::env::set_var`. `ModelRouter::with_credentials()` receives them and checks them before falling back to environment variables.

## Persistence

### Session Store (`crates/lattice-session/`)

- Uses `rusqlite` v0.32 with `bundled` feature (embedded SQLite).
- Implements: git-style conversation graph (branch, checkout, reset, diff), commit objects, turn manifests, profile snapshots.
- `SessionGraphStore` trait provides the abstraction; `SqliteSessionGraphStore` is the primary implementation.

### Agent Memory (optional, `lattice-agent`)

- **Feature:** `sqlite-memory` -- uses `rusqlite` with FTS5 full-text search for memory recall.

## Security

### Tool Safety

v1.1 tool safety is intentionally simple: project-root containment, no `..` traversal, file-size limits, and profile-gated optional shell/web tools. No sandbox crate or policy model exists in the current release.

### SSRF Protection (`crates/lattice-llm/src/security.rs`)

- `validate_base_url()` enforces HTTPS-only for non-localhost, no userinfo, DNS resolution check against private IPs.
- `send_streaming_request()` re-validates custom chat endpoints.

### Credential Safety (`crates/lattice-llm/src/errors.rs`)

- Error response bodies are scrubbed for credential patterns before storage/display.
- `ResolvedModel` debug/serialization hides `api_key`.

## CLI

### CLI (`crates/lattice-cli/`)

- **Argument parsing:** `clap` v4 with derive macros and env var fallback.
- **Binary entry:** `crates/lattice-cli/src/main.rs` -> `lattice` binary.
- **Subcommands:** `run`, `models`, `resolve`, `session`, `doctor`.
- **Output:** `colored` v2 for terminal coloring; `anyhow` v1 for error propagation.

## Tests And Tooling

### Build and Verification

```bash
cargo build                          # Full workspace build
cargo test                           # All tests
cargo clippy -- -D warnings          # Linting
cargo fmt --all --check              # Formatting
cargo bench                          # Benchmarks
./scripts/source-loc-budget.sh --max 20000  # LOC budget gate
./scripts/release-boundary-check.sh        # Boundary and docs drift gate
```

### Release Verification Scripts

- `scripts/source-loc-budget.sh`: counts every Rust line under `crates/*/src/**/*.rs` and fails above the `--max` threshold.
- `scripts/release-boundary-check.sh`: checks removed crate/concept imports, unsupported command files, lockfile entries, and docs drift.

### Test Utilities

- `tempfile` v3 for isolated test directories.
- `crates/lattice-cli/tests/release_smoke.rs`: isolated CLI smoke with temp HOME/XDG/cwd.
- `crates/lattice-cli/tests/cli_cutover.rs`: CLI surface boundary tests.

---

*Stack analysis: 2026-05-17 (post-Phase 11 deletion)*
