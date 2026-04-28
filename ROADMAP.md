# Roadmap

## Phase 1: Core stabilization (current)

Fix remaining issues from the second code review:

- [ ] Fix `run_with_fallback` — check `is_retryable()` before retrying non-retryable errors
- [ ] Fix `trim_conversation` — O(n²) removal, tool-call pair splitting, double clone
- [ ] Add size limits to tool call results (DoS prevention)
- [ ] Remove unused dependencies (`uuid`, `chrono`, `pyo3-async-runtimes`)
- [ ] Fix `ToolDefinition::set_parameters` — return error instead of silently ignoring
- [ ] Fix HTTP 408 classification as retryable
- [ ] Fix `extract_retry_after` for string-valued numbers
- [ ] Reduce provider boilerplate with macro (~1300 lines → ~75 lines)

**Target**: production-ready core, <10 known issues.

## Phase 2: Kernel separation

Split the monolithic crate:

```
artemis-core        # Only: catalog, router, provider, transport, streaming, retry, tokens, errors
artemis-agent-loop  # AgentLoop, budget tracking, fallback (separate crate)
artemis-python      # PyO3 bindings, engine, streaming_bridge (separate crate)
```

- [ ] Move `agent_loop` out of core
- [ ] Move `tool_boundary` up to agent layer
- [ ] Move `streaming_bridge` to python crate
- [ ] Shared tokio runtime handle instead of independent runtimes
- [ ] `ChatRequest` supports borrowed messages

**Target**: `artemis-core` is truly minimal — just model routing + inference. ~5k lines.

## Phase 3: Typed plugin system

- [ ] Plugin trait: `Input` / `Output` types, `to_prompt()`, `from_output()`, `should_handoff()`
- [ ] Output validation + retry framework (parse error → retry N times → fallback)
- [ ] Python glue layer: `importlib` loading, plugin registry, composition
- [ ] Built-in plugins: `code-review`, `refactor`, `test-gen`
- [ ] Plugin distribution via `pip` (`pip install artemis-code-review-plugin`)

**Target**: dogfooding — use artemis plugins to develop artemis itself.

## Phase 4: Agent communication

- [ ] Handoff protocol: structured `{ target, payload, context_summary }`
- [ ] Agent routing: code-controlled dispatch based on output type + confidence
- [ ] YOLO mode: LLM-suggested handoff (with type boundaries enforced)
- [ ] Multi-agent composition: overlay merge of plugin sets

**Target**: compose vertical agents from plugins + route between them.

## Phase 5: Nix paradigm

- [ ] `artemis.toml` + `artemis.lock` — declarative config, reproducible builds
- [ ] Content-addressed response cache: `sha256(prompt + model + params) → response`
- [ ] Derivation-style task model: `InferenceTask { inputs → build → output }`
- [ ] Overlay pattern for catalog extension
- [ ] Sandboxed tool execution

**Target**: every inference is a derivation. Reproducible, cacheable, auditable.

## Timeline

```
Phase 1  ████████░░  (in progress)
Phase 2  ░░░░░░░░░░
Phase 3  ░░░░░░░░░░
Phase 4  ░░░░░░░░░░
Phase 5  ░░░░░░░░░░
```

No dates. Phases are sequential but scope adjusts based on dogfooding feedback.

## Related documents

- [Design vision and ideas](artemis-core/docs/ideas.md)
- [Architecture overview](artemis-core/docs/architecture.md)
- [Code review report](artemis-core/docs/code-review-report.md)
- [Development guide](CLAUDE.md)
