# LATTICE

## What This Is

LATTICE is a small, session-centered Rust LLM runtime. One readable execution path is the product: load a profile, open a session branch, call an LLM, run simple tools, commit the turn, and show the result through the CLI.

## Core Value

A user can run one session-centered generic agent through a small, understandable Rust runtime with simple model routing, simple tools, commit-backed history, and a CLI surface that does not hide extra architecture.

## Current Milestone: v1.2-alpha Evented Runtime

**Goal:** Make the v1.1 turn transaction live-observable while preserving git-like commit history and keeping `lattice-api` a thin facade.

**Target features:**
- Streaming-first LLM execution through `LlmClient::stream_chat` and deterministic fake streaming tests.
- Clean internal and public turn event vocabularies with `Completed` / `Error` / `Cancelled` terminal statuses.
- AgentLoop delta processing for token, reasoning, model, and tool activity without adding a public accumulator or runtime crate.
- Live `stream_turn` implemented with a single spawned task, bounded channel, ordered event forwarding, and durable turn commits.
- CLI rendering that consumes public `TurnEvent` values after the API stream contract stabilizes.

## Current State

**Shipped:** v1.1 Cut Off Cleverness (2026-05-20); Phase 14 AgentLoop streaming delta engine (2026-05-21)
**Codebase:** 8 crates, 18.5k LOC Rust, 340+ tests passing
**Stack:** Rust, tokio, reqwest, rusqlite, clap, serde

The v1.1 rewrite established the minimal session-driven chain and removed all abstractions that did not directly serve it. The runtime went from ~58.5k LOC across 14+ crates to 18.1k LOC across 8 focused crates.

The v1.2-alpha milestone starts from that small substrate. The design source is `.planning/alpha-v1.2-evented-runtime-design.md`; the implementation should follow its conservative split: `lattice-agent` owns the model/tool loop, `lattice-api` owns session transaction boundaries and public event mapping, `lattice-session` owns durable history, and `lattice-cli` consumes the public stream.

## Requirements

### Validated

- ✓ v1.0 established coherent single-agent runtime and removed plugin/orchestrator layers — v1.0
- ✓ 8-crate workspace topology (llm, session, profile, agent, tools, api, cli, hooks) — v1.1
- ✓ Functional `chat()`/`stream()` LLM calls with direct provider dispatch — v1.1
- ✓ Git-like session graph with SQLite persistence, immutable commits, and pointer branches — v1.1
- ✓ Direct `agent.toml` profile parsing without lowering pipelines — v1.1
- ✓ AgentLoop with injectable LLM/tool seams and durable turn commits — v1.1
- ✓ Thin `lattice-api` facade with `run_turn`/`stream_turn` — v1.1
- ✓ CLI surface narrowed to run, models, resolve, session, doctor — v1.1
- ✓ Active source reduced to 18.1k LOC (under 20k budget) — v1.1
- ✓ All removed abstractions absent from active source — v1.1
- ✓ Use `Completed`, `Error`, and `Cancelled` across `AgentTurnStatus`, `RunTurnStatus`, and commit status vocabulary — Phase 13
- ✓ Make AgentLoop consume streaming LLM events, finalize transcript deltas, and preserve partial assistant output on error/cancellation — Phase 14
- ✓ AgentLoop consumes `stream_chat` with per-chunk delta accumulation, all-or-nothing tool call validation, and partial content preservation on error — Phase 14
- ✓ 13 behavioral streaming delta tests proving token/reasoning/tool_call assembly, error handling, and backward compatibility — Phase 14

### Active

Active items are milestone requirements, not completed implementation.

- Make `AgentTurnEvent` carry process events only; derive terminal public events in `lattice-api` from `AgentTurnResult`.
- Expose `TurnEvent` variants for turn start, model activity, deltas, tools, terminal outcomes, committing, committed, and commit failure.
- Make `stream_turn` return a live event stream before commit, use a bounded channel, and keep receiver drop separate from cancellation.
- Render live public events through CLI `run` after the API stream shape is stable.

### Out of Scope

- TUI rewrite — rebuild on `lattice-api` in a future milestone
- Plugins, orchestration, swarm behavior — require stable session/API foundation first
- Complex sandbox or approval policy — v1.1 keeps tool restrictions simple
- Rich prompt compiler layers — deferred until main path is stable
- Merge/rebase/stash session commands — pointer operations are enough for now
- Broad model catalog or automatic routing — explicit aliases keep routing understandable
- Broad backward-compatible internal API shims — v1.1 is a simplifying rewrite; narrow migration fallbacks are allowed when they keep staged PRs small

## Context

v1.1 shipped after a 4-day focused rewrite starting 2026-05-16. The milestone was framed as "cut off cleverness": delete or merge abstractions until LATTICE is a session-driven runtime small enough for contributors to read and change.

The 8 active crates follow a strict layered dependency flow: `hooks → llm → session/profile/tools → agent → api → cli`. Lower crates do not know about CLI state.

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Treat LLM calls as functions | Deep runtime object graph obscures real execution path | ✓ Good — `chat()`/`stream()` are clear and testable |
| Make session graph the durable center | Branches, commits, turn history are stable foundation | ✓ Good — natural substrate for future collaboration |
| Replace `lattice-runtime` with thin `lattice-api` | Public surface should not preserve long-running handles | ✓ Good — thin facade, no policy leakage |
| Pause TUI for v1.1 | Old TUI would force compatibility with removed layers | ✓ Good — clean break, rebuild later |
| Defer complex security policy | Redesigned model should build on simplified substrate | ✓ Good — simple tool restrictions are honest |
| Injectable LLM/tool traits for testing | Makes agent loop fully testable without credentials | ✓ Good — 504 tests run without API keys |
| Start from `main` not cleanup branch | v1.1 should build on v1.0 single-agent baseline | ✓ Good — clean starting point |
| Live events are observations, not history | Git-like history should remain transcript commits, not event logs | Active — v1.2-alpha stores transcript delta and terminal metadata only |
| Keep `lattice-api` thin during streaming work | The facade should coordinate session transactions, not become a runtime crate | Active — live path maps events and commits directly |
| Use `Error`, not `Failed`, as terminal status | Align API, agent, and commit vocabularies with persisted status strings | Active — event names may still use `TurnFailed` for public observations |

## Constraints

- **Architecture**: Layered dependency direction: hooks → llm → session/profile/tools → agent → api → cli
- **Scope**: Delete or merge layers that do not serve the main path
- **Persistence**: Session commits and branch refs are durable; branch ops never implicitly re-run a model
- **Security**: Simplified tool restrictions; no pretense of full policy/sandbox
- **CLI**: CLI is the only maintained frontend
- **Budget**: Active `crates/*/src` Rust LOC should stay manageable
- **Event History**: Runtime events are not durable history; commits store transcript deltas and terminal metadata
- **Alpha Scope**: No durable event log, runtime crate, public accumulator, TUI, parallel tools, or in-flight tool cancellation

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
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-05-21 after Phase 14 completion*
