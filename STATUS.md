# LATTICE Project Status

**Updated**: 2026-05-05
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
- Centralized builtin plugin registry (11 builtins) and TUI partial-stream error surfacing

### Verification Snapshot
- `cargo fmt --check --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-targets` pass on the current worktree.
- Passing tests do not mean production-ready; alpha issues remain tracked in `docs/review/active-issues.md`.

## Current Workspace (8 crates)

```
lattice-core        Pure Rust: resolve, chat, streaming, retry
lattice-agent       Agent, tool execution, memory (merged lattice-memory + lattice-token-pool)
lattice-bus         Micro-agent pub/sub + RPC bus
lattice-harness     Pipeline, AgentRunner, PluginDagRunner, SqliteMemory
lattice-plugin      Plugin trait, 11 builtin plugins, PluginRegistry, ErasedPluginRunner
lattice-cli         CLI: run, print, resolve, models, doctor, bus, config, sessions, stats, debug, validate, list, new, tui
lattice-tui         Terminal UI (ratatui)
lattice-python      PyO3 bindings (pip: lattice-core)
```
