# LATTICE

## What This Is

LATTICE is a Rust LLM runtime being narrowed into a small, session-centered agent platform. v1.1 focuses on making one readable execution path the product: load a profile, open a session branch, call an LLM, run simple tools, commit the turn, and show the result through the CLI.

## Core Value

A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.

## Current Milestone: v1.1 Cut Off Cleverness

**Goal:** Rewrite the active runtime around the minimal session-driven chain and remove or merge abstractions that do not directly serve that chain.

**Target features:**
- A single main path: load profile -> checkout session branch -> build agent -> call LLM -> execute tools -> commit turn -> update branch -> render result.
- Functional LLM calls via `chat()` and `stream()`, backed by a simple alias router for OpenAI, Anthropic, Gemini, and OpenAI-compatible providers.
- A Git-like session graph where commits are immutable, branches are pointers, and branch operations do not re-run models.
- A direct `agent.toml` profile model without lowering pipelines, source drift fingerprints, or profile/orchestrator indirection.
- Eight active crates: `lattice-llm`, `lattice-session`, `lattice-profile`, `lattice-agent`, `lattice-tools`, `lattice-api`, `lattice-cli`, and `lattice-hooks`.
- A thin `lattice-api` facade that owns create/open/run/stream/session operations without long-running runtime handles or policy logic.
- A CLI-only v1.1 surface: `run`, `models`, `resolve`, `session ...`, `doctor`, and help.
- Active Rust source in `crates/*/src` reduced to roughly 20k lines or less, with unnecessary layers removed rather than hidden.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

- Done: v1.0 established a coherent generic single-agent runtime and removed plugin/orchestrator/pipeline/subagent/code-agent-specific workspace layers.
- Done: The current codebase has working ingredients for model routing, session persistence, prompt/context assembly, tool execution, runtime turn streaming, and CLI/TUI feedback.
- Done (Phase 11): All active runtime requirements validated — 8-crate workspace, 19,961 LOC, passing release gate (fmt/clippy/check/504 tests/smoke/boundary).

### Active

<!-- Current scope. Building toward these. -->

(All v1.1 requirements now validated — no remaining active items for this milestone.)

### Out of Scope

<!-- Explicit boundaries. Includes reasoning to prevent re-adding. -->

- TUI rewrite for v1.1 - the old TUI is too coupled to the current runtime; a new minimal TUI belongs in v1.2.
- Plugins, plugin bundles, orchestration, subagents, DAG runners, handoff, and swarm behavior - these require a stable session/API foundation first.
- Complex sandbox, approval, or policy systems - v1.1 uses simple local tool restrictions and leaves a redesigned security model for a later milestone.
- Rich prompt compiler layers, priority systems, and prompt budget orchestration - useful later, but they obscure the v1.1 main path.
- Advanced Git-like operations such as merge, rebase, and stash - branch pointer operations are enough for this milestone.
- Broad model catalog or automatic routing heuristics - explicit provider aliases are enough for v1.1.
- Backward compatibility with removed internal APIs - this branch is allowed to be a simplifying rewrite.

## Context

The v1.1 source brief is the shared document at `https://chatgpt.com/s/6a07f93198b4819187cd314f85214cbe`. It frames v1.1 as "cut off cleverness": delete or merge abstractions until LATTICE is a session-driven runtime small enough for contributors to read and change.

The active branch for this milestone is `cut-off-cleverness`, created from `main` on 2026-05-16. The current `main` history already includes the v1.0 single-agent merge, so this milestone starts from a plugin/orchestrator-free baseline but still has more crates, wrappers, prompt/security layers, CLI/TUI surfaces, and source volume than the v1.1 target.

Current source size is about 58.5k lines under `crates/*/src`. The milestone target is roughly 20k lines or less, so deletion and consolidation are first-class deliverables, not cleanup after the fact.

## Constraints

- **Architecture**: Preserve the layered dependency direction: hooks/core-like LLM/session/profile/tools/agent/api/cli. Lower crates must not learn about CLI state.
- **Scope**: Delete or merge layers that do not serve the main path; do not hide them behind unused exports.
- **Persistence**: Session commits and branch refs are durable state; branch operations move pointers and never implicitly re-run a model.
- **Security**: v1.1 must be honest about its simplified tool restrictions and not pretend to provide the removed policy/sandbox approval model.
- **CLI**: CLI is the only maintained frontend in v1.1; TUI is deferred unless needed as a thin compatibility stub.
- **Budget**: Active `crates/*/src` Rust LOC should end at roughly 20k or less.
- **Verification**: Every requirement must map to one roadmap phase and have a concrete verification gate.

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Start `cut-off-cleverness` from `main` | v1.1 should build on the v1.0 single-agent baseline, not on a planning-artifact cleanup branch | Validated — 6 phases completed on this branch |
| Treat LLM calls as functions | A deep runtime object graph obscures the real execution path | Validated — `lattice-llm` provides `chat()`/`stream()`/`stream_raw()` |
| Make session graph the durable center | Branches, commits, and turn history are the stable foundation for future collaboration features | Validated — `lattice-session` graph store with SQLite persistence |
| Replace `lattice-runtime` with thin `lattice-api` | The public surface should assemble the main path without preserving long-running handles or policy logic | Validated — `lattice-api` facade with `run_turn`/`stream_turn` |
| Pause TUI for v1.1 | The old TUI would force compatibility with the runtime layers being removed | Validated — TUI deleted, deferred to future milestone |
| Defer complex security policy | A redesigned security model should be built on the simplified API/session substrate later | Validated — `lattice-sandbox`/`lattice-policy` removed, simple tool restrictions only |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `$gsd-transition`):
1. Requirements invalidated? -> Move to Out of Scope with reason
2. Requirements validated? -> Move to Validated with phase reference
3. New requirements emerged? -> Add to Active
4. Decisions to log? -> Add to Key Decisions
5. "What This Is" still accurate? -> Update if drifted

**After each milestone** (via `$gsd-complete-milestone`):
1. Full review of all sections
2. Core Value check -- still the right priority?
3. Audit Out of Scope -- reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-05-18 after completing Phase 11 — all v1.1 requirements validated*
