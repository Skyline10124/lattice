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

```text
lattice-core        # Pure Rust: catalog, router, provider, transport, streaming, retry, tokens, errors
lattice-agent       # Agent state, tool boundary, retry (separate crate)
lattice-memory      # Memory trait + InMemoryMemory (shared trait crate)
lattice-token-pool  # TokenPool trait + UnlimitedPool (shared trait crate)
lattice-python      # PyO3 bindings (resolver only, for now)
```

- [x] Move agent logic out of core into `lattice-agent`
- [x] Move tool boundary up to agent layer
- [x] Create `lattice-memory` and `lattice-token-pool` trait crates
- [x] Create `lattice-python` with PyO3 bindings (resolver)
- [x] `lattice-core` is pure Rust (rlib only, no PyO3 dependency)
- [x] Transport trait unified, shared `reqwest::Client`
- [x] HTTPS enforced for non-localhost base URLs
- [x] Catalog base_url properly falls back to provider_defaults

**Result**: Clean separation. lattice-core is truly minimal -- just model routing + inference.

> **Note**: `lattice-memory` and `lattice-token-pool` were later merged back into their dependents
> (`lattice-agent` and `lattice-harness`) during Phase 3 for simplicity.

## Phase 3: Dogfooding + Agent runtime (complete)

- [x] Agent runtime: `Agent::run()` with auto tool loop
- [x] 7 built-in tools: read_file, grep (pure Rust regex), write_file, list_directory, bash, patch, web_search
- [x] Context trimming: `AgentState::trim_messages`
- [x] Sandbox safety: `SandboxConfig` (paths, commands, domains)
- [x] Async Memory trait + `SqliteMemory` (FTS5) with auto-save in `Agent::run()`
- [x] `EntryKind`: SessionLog, Fact, Decision, ProjectContext
- [x] `AgentDispatcher` trait + `agent_call:name` tool
- [x] `lattice-harness`: AgentProfile (TOML), AgentRegistry, AgentRunner, Pipeline, Python handoff
- [x] `lattice-plugin`: Plugin trait (Input/Output), Behavior trait (Strict/Yolo), PluginRunner, PluginHooks, CodeReviewPlugin
- [x] `lattice-cli`: run/print/resolve/models subcommands
- [x] `lattice-tui`: Ratatui TUI with Agent streaming
- [x] Credential error on missing keys

**Result**: 8 crates, 7 tools, ~150+ tests. Dogfooding validated.

## Phase 4: Typed plugin system + DAG orchestration (complete)

- [x] Plugin trait: `Input` / `Output` types
- [x] `Behavior` trait: Strict / Yolo
- [x] `PluginRunner`, `PluginHooks`, `PluginConfig`
- [x] `CodeReviewPlugin` (built-in)
- [x] `output_schema()` on Plugin trait
- [x] `ErasedPlugin` trait + blanket impl for type-erased plugin storage
- [x] `PluginBundle`, `PluginMeta`, `BehaviorMode`, `to_behavior()`
- [x] `PluginRegistry`: register/get/list plugins with duplicate detection
- [x] `ErasedPluginRunner` + shared `run_plugin_loop()`
- [x] `PluginDagRunner`: intra-agent DAG orchestration with slot/edge routing
- [x] `ToolRegistry` + `merge_tool_definitions()`
- [x] `PluginsConfig` / `PluginSlotConfig` / `AgentEdgeConfig` TOML deserialization
- [x] Pipeline branching: `[plugins]` in profile → PluginDagRunner, else AgentRunner
- [x] 9 builtin plugins: CodeReview, Refactor, TestGen, SecurityAudit, DocGen, PptxGen, DeepResearch, ImageGen, KnowledgeBase
- [x] CLI integration: `--pipeline` auto-loads builtin plugins
- [x] Integration tests for edge matching, error types, behavior roundtrip

**Target**: compose vertical agents from plugins + route between them.

## Phase 5: Async + Rust native refactoring (complete)

Full asyncification and elimination of C-isms across the stack:

- [x] `ToolExecutor` trait async via `#[async_trait]`
- [x] All tools converted to tokio: `tokio::fs`, `tokio::process`, `reqwest::Client`
- [x] grep replaced with pure Rust `regex` based recursive search (no more `Command::new("grep")`)
- [x] `ToolError` enum (thiserror)
- [x] `PluginAgent` trait async
- [x] `Agent::run()` and `run_chat()` merged into single async fn
- [x] `SHARED_RUNTIME` + `run_async()` bridge deleted
- [x] `AgentRunner::run()` async, `MEMORY_RT` deleted
- [x] `Pipeline::run()` async, sync `run_fork()` deleted (-511 lines)
- [x] `run_plugin_loop` async, `std::thread::sleep` → `tokio::time::sleep`
- [x] All callers cascaded (debug, print, tui, micro_agent)
- [x] `SandboxConfig.read_allowlist` enforcement, `check_url` SSRF fixes
- [x] `active-issues.md` updated with 22 deep scan findings

**Result**: Fully async stack. Zero blocking calls. Pure Rust tool execution. No global runtime.

## Phase 6: Prompt engine + Context assembly

- [ ] `PromptSection`, `Layer`, `TokenBudget` data structures
- [ ] `ContextProvider` trait + registry
- [ ] `Compiler`: sort → budget → trim → render
- [ ] `SystemProvider`, `InputProvider`, `MemoryProvider`, `EventsProvider`
- [ ] `Agent::run()` 集成 compiler，将 prompt 构造下沉到 lattice-agent
- [ ] Plugin system_prompt / to_prompt 转化为通过 ContextProvider 接入
- [ ] LRU 事件老化策略
- [ ] Bus 自动发现 context provider (`bus.discover("context-provider")`)
- [ ] Token budget 固定配比可配置化

**Target**: 声明式 prompt 组装，所有 LLM 输入都经过引擎统一编排。

详见 [Prompt Engine Spec](docs/specs/2026-05-04-prompt-engine.md)。

## Phase 7: Nix paradigm

- [ ] `LATTICE.toml` + `LATTICE.lock` -- declarative config, reproducible builds
- [ ] Content-addressed response cache: `sha256(prompt + model + params) -> response`
- [ ] Derivation-style task model: `InferenceTask { inputs -> build -> output }`
- [ ] Overlay pattern for catalog extension
- [ ] Sandboxed tool execution (CubeSandbox)

**Target**: every inference is a derivation. Reproducible, cacheable, auditable.

## Current focus

The project is in **alpha / dogfooding** stage. Phases 1-5 are complete. DS-P0/P1/P2/P3 security + engineering improvements are all addressed. The 2026-05-03 CLI audit (13 issues) is fully resolved — sessions, stats, config set, `--continue-session`, `--no-save`, `--provider`, `list agents`, and `bus` default dir are all real implementations.

Current priorities:

1. **Phase 6 (Prompt engine)**: 声明式 prompt 组装，ContextProvider → Compiler 链路落地
2. **Phase 7 (Nix paradigm)**: 等 prompt 引擎稳定后再推进，避免设计冲突
3. **DS-P3 items**: remaining deep scan findings from `docs/review/active-issues.md`
4. **iOS embedding**: verify `aarch64-apple-ios` cross-compilation, finalize runtime handle injection

## Current workspace (8 crates)

```text
lattice-core        Pure Rust: catalog, router, provider, transport, streaming, retry, tokens, errors
lattice-agent       Agent state, tool boundary, memory (merged former lattice-memory + lattice-token-pool)
lattice-bus         Micro-agent pub/sub + RPC communication bus
lattice-harness     Pipeline, AgentRunner, PluginDagRunner, SqliteMemory, TOML profiles
lattice-plugin      Plugin trait, 9 builtins, ErasedPluginRunner, PluginRegistry
lattice-cli         CLI: run, resolve, models, doctor, bus, config, sessions, stats, debug, validate, list, new, tui
lattice-tui         Terminal UI (ratatui)
lattice-python      PyO3 bindings (pip: lattice-core)
```

## Timeline

```text
Phase 1  ██████████  (complete)
Phase 2  ██████████  (complete)
Phase 3  ██████████  (complete)
Phase 4  ██████████  (complete)
Phase 5  ██████████  (complete)
Phase 6  ░░░░░░░░░░
Phase 7  ░░░░░░░░░░
```

No dates. Phases are sequential but scope adjusts based on dogfooding feedback.

## Related documents

- [Design vision and ideas](docs/design/ideas.md)
- [Code review report](docs/review/code-review-2026-05-01.md)
- [Active issues](docs/review/active-issues.md)
- [Debug guide](docs/debug/guide.md)
- [Development guide](CLAUDE.md)
- [Facade API spec](docs/specs/2026-05-02-facade-api.md)
- [Async + Rust native spec](docs/specs/2026-05-02-async-rust-native-refactor.md)
