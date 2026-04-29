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

**Result**: 34/44 code review issues fixed (77% fix rate). All P0 and high-priority items cleared. 409+ tests pass, 0 fail.

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

## Phase 3: Typed plugin system (next)

- [ ] Plugin trait: `Input` / `Output` types, `to_prompt()`, `from_output()`, `should_handoff()`
- [ ] Output validation + retry framework (parse error -> retry N times -> fallback)
- [ ] Python glue layer: `importlib` loading, plugin registry, composition
- [ ] Built-in plugins: `code-review`, `refactor`, `test-gen`
- [ ] Plugin distribution via `pip` (`pip install artemis-code-review-plugin`)

**Target**: dogfooding -- use artemis plugins to develop artemis itself.

## Phase 4: Agent communication

- [ ] Handoff protocol: structured `{ target, payload, context_summary }`
- [ ] Agent routing: code-controlled dispatch based on output type + confidence
- [ ] YOLO mode: LLM-suggested handoff (with type boundaries enforced)
- [ ] Multi-agent composition: overlay merge of plugin sets

**Target**: compose vertical agents from plugins + route between them.

## Phase 5: Nix paradigm

- [ ] `artemis.toml` + `artemis.lock` -- declarative config, reproducible builds
- [ ] Content-addressed response cache: `sha256(prompt + model + params) -> response`
- [ ] Derivation-style task model: `InferenceTask { inputs -> build -> output }`
- [ ] Overlay pattern for catalog extension
- [ ] Sandboxed tool execution

**Target**: every inference is a derivation. Reproducible, cacheable, auditable.

## Current focus

The project is in **alpha / dogfooding** stage. Phase 1 and 2 are complete. Current priorities:

1. **Python API expansion**: expose `Message`, `Role`, `chat_complete()` in `artemis-python`
2. **Runtime correctness**: finish reason mapping, streaming timeout fix
3. **Agent productionization**: memory/token_pool integration

After Python API parity and runtime hardening, move to Phase 3 (typed plugin system).

## Timeline

```
Phase 1  ██████████  (complete)
Phase 2  ██████████  (complete)
Phase 3  ░░░░░░░░░░  (next)
Phase 4  ░░░░░░░░░░
Phase 5  ░░░░░░░░░░
```

No dates. Phases are sequential but scope adjusts based on dogfooding feedback.

## Related documents

- [Design vision and ideas](artemis-core/docs/ideas.md)
- [Architecture overview](artemis-core/docs/architecture.md)
- [Code review report (historical)](artemis-core/docs/code-review-report.md)
- [Current implementation review](artemis-core/docs/current-implementation-review.md)
- [Development guide](CLAUDE.md)
