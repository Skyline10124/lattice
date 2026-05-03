# LATTICE Project Status

**Updated**: 2026-05-03
**Focus**: Security hardening, alpha fixes, DS-P1/P2 engineering improvements

## Recent Sprints

### Phase 4: Security hardening + Alpha fixes (complete)

14 P0-P3 security fixes across all crates:
- P0: Critical (sandbox escape, SSRF, TOCTOU, credential leaks)
- P1-P2: Core security (redirect policy, write allowlist, sensitive file canonical matching)
- P3: Quality (env var validation, module isolation)

### DS-P1/P2 Engineering improvements (complete)

14 engineering items covering documentation, correctness, and defaults:
- Context length values filled for all catalog models
- chat() retry documentation clarified, Agent runtime model documented
- Pipeline fork cancellation with timeout + task abort
- PluginRunner system prompt + tools injection + timeout enforcement
- Bus: agent identity tracking, AtLeastOnce documented as not-yet-implemented
- Memory clone_box default removed (dangerous silent data loss)
- CLI credential consistency across all commands
- SSE EOF residual buffer flush
- Python stream_chat() API
- TUI CLI integration (`lattice tui`)
- Gemini strategy documentation
- Retry-After HTTP header consumption
- README/STATUS/ROADMAP synced to workspace structure

### Pre-existing Issues (NOT from these fixes)
- `router::tests` — 8 tests fail due to global Mutex poisoning (PoisonError cascade from one panic). These are a pre-existing concurrency bug in the router's credential cache.

## Current Workspace (8 crates)

```
lattice-core        Pure Rust: resolve, chat, streaming, retry
lattice-agent       Agent, tool execution, memory (merged lattice-memory + lattice-token-pool)
lattice-bus         Micro-agent pub/sub + RPC bus
lattice-harness     Pipeline, AgentRunner, PluginDagRunner, SqliteMemory
lattice-plugin      Plugin trait, 9 builtin plugins, ErasedPluginRunner
lattice-cli         CLI: run, resolve, models, doctor, debug, validate, tui
lattice-tui         Terminal UI (ratatui)
lattice-python      PyO3 bindings (pip: lattice-core)
```