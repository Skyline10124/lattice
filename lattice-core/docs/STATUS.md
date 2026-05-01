# LATTICE Status — 2026-04-30

## What works (tested end-to-end)
- resolve("any-model") → automatic provider + credential routing
- chat() / chat_complete() → tested with deepseek, minimax, opencode-go (14 models)
- Thinking mode → deepseek-v4-pro, minimax M2.7
- CLI: LATTICE run "prompt" + LATTICE -p "prompt"
- TUI: cargo run -p lattice-tui

## What works (code complete, not tested end-to-end)
- 17 tools (read_file, grep, write_file, patch, bash, web_search, agent_call, etc.)
- Harness: AgentProfile (TOML), AgentRegistry, AgentRunner, Pipeline
- Context trimming (AgentState::trim_messages)
- Sandbox (SandboxConfig)
- Memory: async trait, SqliteMemory (FTS5), auto-save
- Plugin: Plugin trait, Behavior (Strict/Yolo), PluginRunner
- AgentDispatcher (agent_call:name)

## Known issues
- Agent.send_message() hangs in sync code. Use #[tokio::main] + chat_complete().
- Python binding is resolver-only (no chat/streaming)
- lattice-tui has warnings (cosmetic)
- Some catalog providers untested

## Crates (12)
lattice-core, lattice-agent, lattice-memory, lattice-token-pool,
lattice-plugin, lattice-python, lattice-harness, lattice-cli, lattice-tui
