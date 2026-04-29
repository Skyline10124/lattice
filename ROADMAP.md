# Roadmap

## Phase 1: Core stabilization (complete)

Fix remaining issues from the second code review:

- [x] Fix `run_with_fallback` -- check `is_retryable()` before retrying non-retryable errors
- [x] Fix `trim_conversation` -- O(n^2) removal, tool-call pair splitting, double clone
- [x] Add size limits to tool call results (DoS prevention)
- [x] Remove unused dependencies (`uuid`, `chrono`, `pyo3-async-runtimes`)
- [x] Fix `ToolDefinition::set_parameters` -- return error instead of silently ignoring
- [x] Fix HTTP 408 classification as retryable
- [x] Fix `extract_retry_after` for string-valued numbers
- [x] Reduce provider boilerplate with macro (~1300 lines -> ~75 lines)

**Result**: 34/44 code review issues fixed (77% fix rate). All P0 and high-priority items cleared. Tests pass.

## Phase 2: Kernel separation (complete)

Split the monolithic crate into a 5-crate workspace:

```
artemis-core        # Pure Rust: catalog, router, provider, transport, streaming, retry, tokens, errors
artemis-agent       # Agent state, tool boundary, retry (separate crate)
artemis-memory      # Memory trait + InMemoryMemory (shared trait crate)
artemis-token-pool  # TokenPool trait + UnlimitedPool (shared trait crate)
artemis-python      # PyO3 bindings (resolver only, for now)
```

- [x] Move agent logic out of core into `artemis-agent`
- [x] Move tool boundary up to agent layer
- [x] Create `artemis-memory` and `artemis-token-pool` trait crates
- [x] Create `artemis-python` with PyO3 bindings (resolver)
- [x] `artemis-core` is pure Rust (rlib only, no PyO3 dependency)
- [x] Transport trait unified, shared `reqwest::Client`
- [x] HTTPS enforced for non-localhost base URLs
- [x] Catalog base_url properly falls back to provider_defaults

**Result**: Clean separation. artemis-core is truly minimal -- just model routing + inference.

## Phase 3: Dogfooding + Agent runtime (complete)

- [x] Agent runtime: `Agent::run()` with auto tool loop
- [x] 17 built-in tools: read_file, grep, write_file, list_directory, run_test, run_clippy, bash, patch, run_command, list_processes, web_search, web_fetch, browser_navigate, browser_screenshot, browser_console, execute_code, agent_call
- [x] Context trimming: `AgentState::trim_messages`
- [x] Sandbox safety: `SandboxConfig` (paths, commands, domains)
- [x] Async Memory trait + `SqliteMemory` (FTS5) with auto-save in `Agent::run()`
- [x] `EntryKind`: SessionLog, Fact, Decision, ProjectContext
- [x] `AgentDispatcher` trait + `agent_call:name` tool
- [x] `artemis-harness`: AgentProfile (TOML), AgentRegistry, AgentRunner, Pipeline, Python handoff
- [x] `artemis-plugin`: Plugin trait (Input/Output), Behavior trait (Strict/Yolo), PluginRunner, PluginHooks, CodeReviewPlugin
- [x] `artemis-cli`: run/print/resolve/models subcommands
- [x] `artemis-tui`: Ratatui TUI with Agent streaming
- [x] Credential error on missing keys (P2-1)

**Result**: 9 crates, 17 tools, ~440+ tests. Dogfooding validated.

## Phase 4: Typed plugin system (in progress)

- [x] Plugin trait: `Input` / `Output` types
- [x] `Behavior` trait: Strict / Yolo
- [x] `PluginRunner`, `PluginHooks`, `PluginConfig`
- [x] `CodeReviewPlugin` (built-in)
- [ ] `to_prompt()` / `from_output()` trait formalization
- [ ] Output validation + retry framework (parse error -> retry N times -> fallback)
- [ ] Python glue layer: `importlib` loading, plugin registry, composition
- [ ] Plugin distribution via `pip` (`pip install artemis-code-review-plugin`)
- [ ] Handoff protocol: structured `{ target, payload, context_summary }`
- [ ] Agent routing: code-controlled dispatch based on output type + confidence
- [ ] Multi-agent composition: overlay merge of plugin sets

**Target**: compose vertical agents from plugins + route between them.

> **Known limitation**: `Agent.send_message()` currently requires `#[tokio::main]` context. Sync usage hangs. See issue in `artemis-agent` `run_chat()`.

## Phase 5: Nix paradigm

- [ ] `artemis.toml` + `artemis.lock` -- declarative config, reproducible builds
- [ ] Content-addressed response cache: `sha256(prompt + model + params) -> response`
- [ ] Derivation-style task model: `InferenceTask { inputs -> build -> output }`
- [ ] Overlay pattern for catalog extension
- [ ] Sandboxed tool execution

**Target**: every inference is a derivation. Reproducible, cacheable, auditable.

## Current focus

The project is in **alpha / dogfooding** stage. Phases 1-3 are complete. Current priorities:

1. **Phase 4 completion**: `to_prompt()`/`from_output()` formalization, Python glue layer, pip distribution
2. **Handoff protocol**: agent-to-agent communication with structured payloads
3. **Multi-agent composition**: overlay merge of plugin sets, Pipeline chaining

After Phase 4, move to Phase 5 (Nix paradigm).

## Timeline

```
Phase 1  ██████████  (complete)
Phase 2  ██████████  (complete)
Phase 3  ██████████  (complete)
Phase 4  ██████░░░░  (in progress)
Phase 5  ░░░░░░░░░░
```

No dates. Phases are sequential but scope adjusts based on dogfooding feedback.

## Related documents

- [Design vision and ideas](artemis-core/docs/ideas.md)
- [Architecture overview](artemis-core/docs/architecture.md)
- [Code review report (historical)](artemis-core/docs/code-review-report.md)
- [Current implementation review](artemis-core/docs/current-implementation-review.md)
- [Development guide](CLAUDE.md)
