# Stack Research

**Domain:** Rust LLM single-agent runtime refactor
**Researched:** 2026-05-14
**Confidence:** HIGH

## Recommended Stack

### Core Technologies

| Technology | Version | Purpose | Why Recommended |
|------------|---------|---------|-----------------|
| Rust workspace | 2021 edition, MSRV 1.85 | Main implementation and crate boundaries | Existing project is a Rust workspace; the milestone should simplify internal crates rather than introduce new languages. |
| Tokio | workspace `1`, full feature | Async runtime for model calls, runtime turns, and tool execution | Already used across runtime, agent, core transports, and tests. |
| Serde / TOML / JSON | workspace versions | Profile/config/session serialization | Existing profile, config, session, and CLI JSON paths depend on these contracts. |
| thiserror / anyhow | workspace versions | Typed library errors and CLI-facing errors | Matches existing convention: typed errors in crates, `anyhow` in CLI glue. |
| rusqlite | workspace `0.32`, bundled | Session CAS store and manifest persistence | Existing session store uses it; avoid persistence stack churn during architecture cleanup. |

### Supporting Libraries

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| ratatui / crossterm | workspace versions | TUI frontend | Preserve existing TUI while removing plugin/code-agent-specific state. |
| tracing | workspace `0.1` | Runtime and tool execution observability | Keep diagnostics during large refactor. |
| tempfile | workspace `3` | Integration tests and runtime fixtures | Useful for migration tests around sessions, tool execution, and CLI/TUI guards. |
| similar / syntect / pulldown-cmark | workspace versions | TUI rendering support | Keep only if still used after plugin/code-agent UI removal. |

### Development Tools

| Tool | Purpose | Notes |
|------|---------|-------|
| `cargo check --workspace` | Fast compile closure | Primary gate after dependency deletion. |
| `cargo test -p <crate>` | Targeted regression checks | Prefer crate-specific tests while dependency graph is in motion. |
| `cargo clippy --workspace -- -D warnings` | Final warning gate | Run after compile closure is restored. |
| `cargo fmt --all --check` | Formatting gate | Avoid unrelated formatting churn until final cleanup. |
| `rg` | Boundary verification | Use to assert removed terms no longer appear in active source. |

## What Not To Add

| Avoid | Why | Use Instead |
|-------|-----|-------------|
| New plugin framework replacements | This milestone is about deleting immature extension layers, not replacing them. | Generic profile + tools. |
| New orchestration or task graph crates | Would recreate the same coupling under another name. | One runtime-owned agent handle path. |
| Async SQL migration for sessions | Persistence churn is orthogonal and risky. | Keep current `rusqlite` session store. |
| New provider transports | The main blocker is architecture, not provider capability. | Keep existing model router/transport behavior. |

## Stack Impact

The target stack should remove `lattice-plugin`, `lattice-plugins`, and `lattice-orchestrator` from the workspace. It should add a lightweight `lattice-profile` crate or equivalent runtime-owned module only if that keeps profile data independent from runtime and agent implementation. If a new crate is introduced, it should depend only on lower-level crates needed for declarative configuration: likely `lattice-core`, `lattice-prompt`, `lattice-tools`, `serde`, `toml`, and `thiserror`.

## Sources

- `.planning/codebase/STACK.md` — current workspace stack and crate map.
- `.planning/codebase/ARCHITECTURE.md` — current runtime/orchestrator/plugin data flow.
- `.planning/codebase/CONCERNS.md` — plugin default-feature and runtime security concerns.
- `Cargo.toml` and crate manifests — actual workspace dependency surface.

---
*Stack research for: Rust LLM single-agent runtime refactor*
*Researched: 2026-05-14*
