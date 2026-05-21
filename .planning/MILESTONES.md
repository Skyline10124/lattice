# Milestones

## v1.1 Cut Off Cleverness

**Shipped:** 2026-05-20
**Phases:** 6 (Phase 6–11) | **Plans:** 29 | **Commits:** 34
**LOC:** 58.5k → 18.1k (69% reduction)
**Timeline:** 4 days (2026-05-16 → 2026-05-20)
**Tests:** 504 passing at release gate

### Delivered

Rewrote the active runtime around a minimal session-driven chain and removed abstractions that did not directly serve it. The result is an 8-crate, 18k-line Rust workspace with functional LLM calls, a Git-like session graph, injectable agent loop, and a focused CLI surface.

### Key Accomplishments

1. Established 8-crate workspace topology (lattice-llm, session, profile, agent, tools, api, cli, hooks)
2. Implemented functional `chat()`/`stream()` LLM APIs with direct `agent.toml` profile parsing
3. Built Git-like session graph with SQLite persistence, immutable commits, branches, and pointer operations
4. Wired AgentLoop with injectable LLM/tool seams and durable `run_turn`/`stream_turn` API facade
5. Cut CLI to v1.1 surface (run, models, resolve, session, doctor) with SessionApi-backed commands
6. Deleted 40k+ LOC — from ~58.5k down to 18.1k active Rust source

### Archives

- [v1.1-ROADMAP.md](milestones/v1.1-ROADMAP.md)
- [v1.1-REQUIREMENTS.md](milestones/v1.1-REQUIREMENTS.md)
