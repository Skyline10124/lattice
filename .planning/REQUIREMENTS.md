# Requirements: LATTICE v1.1 Cut Off Cleverness

**Defined:** 2026-05-16
**Core Value:** A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.

## v1.1 Requirements

### Simplification

- [ ] **SIMP-01**: Developer can trace the normal `run` flow as one explicit chain: load profile, checkout session branch, build agent, call LLM, execute tools, commit turn, update branch, render result.
- [ ] **SIMP-02**: Active Rust source under `crates/*/src` is reduced to roughly 20k lines or less, excluding tests, examples, benches, and planning docs.
- [ ] **SIMP-03**: Deleted or merged abstractions are absent from active public APIs rather than hidden behind compatibility wrappers.

### Crate Topology

- [ ] **CRATE-01**: Workspace active members are limited to `lattice-llm`, `lattice-session`, `lattice-profile`, `lattice-agent`, `lattice-tools`, `lattice-api`, `lattice-cli`, and `lattice-hooks`.
- [ ] **CRATE-02**: `lattice-core` is renamed or replaced by `lattice-llm` for provider transports, streaming events, and model alias resolution.
- [ ] **CRATE-03**: `lattice-runtime` is renamed or replaced by `lattice-api` as a thin public facade.
- [ ] **CRATE-04**: `lattice-sandbox`, `lattice-policy`, and `lattice-prompt` are removed or merged into narrower modules that directly serve the v1.1 main path.
- [ ] **CRATE-05**: Active source no longer imports removed crate names or exposes removed concepts in root workspace manifests.

### LLM and Models

- [ ] **LLM-01**: `lattice-llm` exposes `chat(messages, tools, options)` for non-streaming model calls.
- [ ] **LLM-02**: `lattice-llm` exposes `stream(messages, tools, options)` for token/tool/usage event streams.
- [ ] **LLM-03**: Model aliases resolve to provider, provider model name, optional base URL, and API key environment variable.
- [ ] **LLM-04**: Supported provider classes are OpenAI, Anthropic, Gemini, and OpenAI-compatible services.
- [ ] **LLM-05**: The router does not contain broad catalog discovery, automatic model selection, or hidden fallback policy.

### Session Graph

- [ ] **SESS-01**: A session has a stable `session_id`, branch table, HEAD pointer, commits, messages, and refs.
- [ ] **SESS-02**: Each turn creates an immutable commit with parent commit IDs, message IDs, actor profile snapshot, resolved model/provider metadata, and tool call results.
- [ ] **SESS-03**: Branch operations update refs only and never re-run a model implicitly.
- [ ] **SESS-04**: Session commands support `status`, `list`, `log`, `checkout <branch|commit>`, `branch <name> [from]`, `diff [from] [to]`, and `reset <commit>`.
- [ ] **SESS-05**: SQLite persistence stores commits, messages, branch refs, and profile/model/tool metadata needed to reopen a session.

### Profiles

- [ ] **PROF-01**: `agent.toml` parses directly into `AgentProfile` without a separate lowering pipeline.
- [ ] **PROF-02**: Default profile templates exist for reader, editor, and dev modes with explicit tool/safety intent.
- [ ] **PROF-03**: Profile drift fingerprints, source provenance warnings, and orchestrator-style profile indirection are removed from the v1.1 main path.

### Agent and Tools

- [x] **AGENT-01**: The agent loop reads current session state and user input, builds messages, calls the LLM, executes requested tools sequentially, and repeats until a final assistant response is produced.
- [x] **AGENT-02**: The loop returns generated messages, tool results, and usage/statistics needed for the session commit.
- [x] **TOOL-01**: Built-in tools cover reading files, reading multiple files, writing files, applying patches, grepping content, finding files, listing directories, and writing TODO markers.
- [x] **TOOL-02**: Optional `web_search` and `bash` tools are available only when the active profile enables them.
- [x] **TOOL-03**: File tools enforce project-root containment, reject `..` traversal, and enforce file-size limits.
- [x] **TOOL-04**: Complex sandbox, approval, and policy decisions are not exposed as v1.1 runtime features.

### API and CLI

- [x] **API-01**: `lattice-api` exposes `create_session(profile, opts)`, `open_session(session_id)`, `run_turn(session_id, input, profile)`, and `stream_turn(session_id, input, profile)`.
- [x] **API-02**: `lattice-api` exposes basic session operations for checkout, branch, log, diff, and reset.
- [x] **API-03**: `lattice-api` does not expose long-running runtime handles, policy objects, prompt compiler internals, or sandbox approval plumbing.
- [ ] **CLI-01**: CLI keeps only `run`, `models`, `resolve`, `session ...`, `doctor`, and help as supported v1.1 commands.
- [ ] **CLI-02**: CLI `run <prompt> [--model|-m] [--profile] [--session]` creates or resumes a session and commits the turn.
- [ ] **CLI-03**: `models --auth` and `resolve <model>` display explicit alias/provider/auth information.
- [ ] **CLI-04**: Old or advanced surfaces such as TUI, `run --pipeline`, plugin commands, orchestrator commands, `new agent`, `validate`, `merge`, `rebase`, and `stash` are removed or hidden from supported help.

### Verification

- [ ] **VER-01**: `cargo check --workspace` succeeds after crate rename/removal.
- [ ] **VER-02**: Focused tests cover LLM routing, profile parsing, session graph operations, agent tool loop, API facade, and CLI commands.
- [ ] **VER-03**: Boundary checks prove removed crates/concepts are absent from active source and manifests.
- [ ] **VER-04**: CLI smoke checks prove `run`, `models --auth`, `resolve`, `session status`, `session log`, and `doctor` work or fail with clear missing-credential messages.
- [ ] **VER-05**: A source-size check reports `crates/*/src` Rust LOC at roughly 20k or less.
- [ ] **VER-06**: README, AGENTS.md, CLAUDE.md, and codebase maps describe the v1.1 architecture and deferred features accurately.

## Future Requirements

### v1.2 and Later

- **FUT-01**: Rebuild a minimal TUI on top of `lattice-api`.
- **FUT-02**: Reintroduce richer prompt compilation with explicit layer priorities and token budgets.
- **FUT-03**: Design a new security and sandbox model on top of the simplified tool/API substrate.
- **FUT-04**: Add swarm or multi-agent collaboration using the session graph as the coordination substrate.
- **FUT-05**: Reconsider plugins and third-party tool ecosystems after API/session boundaries are stable.

## Out of Scope

| Feature | Reason |
|---------|--------|
| TUI rewrite in v1.1 | The current TUI is coupled to runtime details being removed; rebuild later on `lattice-api`. |
| Plugin/orchestrator/swarm features | These require a stable session graph and API first. |
| Complex sandbox or approval policy | v1.1 intentionally keeps tool restrictions simple and transparent. |
| Rich prompt compiler layers | Prompt richness is deferred until the main path is small and stable. |
| Merge/rebase/stash session commands | Pointer-level branch commands are enough for v1.1. |
| Broad model catalog or automatic routing | Explicit aliases keep routing understandable. |
| Backward-compatible internal API shims | Compatibility shims would preserve the cleverness this milestone removes. |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| SIMP-01 | Phase 6 | Pending |
| SIMP-02 | Phase 11 | Pending |
| SIMP-03 | Phase 11 | Pending |
| CRATE-01 | Phase 6 | Pending |
| CRATE-02 | Phase 6 | Pending |
| CRATE-03 | Phase 6 | Pending |
| CRATE-04 | Phase 6 | Pending |
| CRATE-05 | Phase 11 | Pending |
| LLM-01 | Phase 7 | Pending |
| LLM-02 | Phase 7 | Pending |
| LLM-03 | Phase 7 | Pending |
| LLM-04 | Phase 7 | Pending |
| LLM-05 | Phase 7 | Pending |
| SESS-01 | Phase 8 | Pending |
| SESS-02 | Phase 8 | Pending |
| SESS-03 | Phase 8 | Pending |
| SESS-04 | Phase 8 | Pending |
| SESS-05 | Phase 8 | Pending |
| PROF-01 | Phase 7 | Pending |
| PROF-02 | Phase 7 | Pending |
| PROF-03 | Phase 7 | Pending |
| AGENT-01 | Phase 9 | Complete |
| AGENT-02 | Phase 9 | Complete |
| TOOL-01 | Phase 9 | Complete |
| TOOL-02 | Phase 9 | Complete |
| TOOL-03 | Phase 9 | Complete |
| TOOL-04 | Phase 9 | Complete |
| API-01 | Phase 9 | Complete |
| API-02 | Phase 9 | Complete |
| API-03 | Phase 9 | Complete |
| CLI-01 | Phase 10 | Pending |
| CLI-02 | Phase 10 | Pending |
| CLI-03 | Phase 10 | Pending |
| CLI-04 | Phase 10 | Pending |
| VER-01 | Phase 11 | Pending |
| VER-02 | Phase 11 | Pending |
| VER-03 | Phase 11 | Pending |
| VER-04 | Phase 11 | Pending |
| VER-05 | Phase 11 | Pending |
| VER-06 | Phase 11 | Pending |

**Coverage:**
- v1.1 requirements: 40 total
- Mapped to phases: 40
- Unmapped: 0

---
*Requirements defined: 2026-05-16*
*Last updated: 2026-05-16 after milestone initialization*
