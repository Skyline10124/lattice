# Roadmap: v1.1 Cut Off Cleverness

**Created:** 2026-05-16
**Phase numbering:** Continues from v1.0. v1.0 ended at Phase 5, so v1.1 starts at Phase 6.
**Requirements:** 40 total, 40 mapped, 0 unmapped.

## Milestone Goal

Rewrite the active runtime around the minimal session-driven chain and remove or merge abstractions that do not directly serve that chain.

## Phase Summary

| # | Phase | Goal | Requirements | Success Criteria |
|---|-------|------|--------------|------------------|
| 6 | Crate Topology and Main-Path Skeleton | 3/3 | Complete   | 2026-05-16 |
| 7 | Functional LLM and Direct Profiles | 4/4 | Complete   | 2026-05-16 |
| 8 | Session Graph Core | 4/4 | Complete   | 2026-05-16 |
| 9 | Agent Loop, Tools, and API Facade | 4/4 | Complete    | 2026-05-16 |
| 10 | CLI Cutover | Reduce supported user-facing commands to the v1.1 CLI surface. | CLI-01..CLI-04 | 5 |
| 11 | Deletion, Budget, and Release Verification | Delete compatibility leftovers, prove boundaries, meet the source-size budget, and update docs. | SIMP-02, SIMP-03, CRATE-05, VER-01..VER-06 | 6 |

## Dependency Shape

Phase 6 -> Phase 7 and Phase 8 -> Phase 9 -> Phase 10 -> Phase 11.

Phase 7 and Phase 8 can be planned independently after Phase 6 because LLM/profile work and session graph work should stay decoupled until the API/agent integration phase.

## Phase Details

### Phase 6: Crate Topology and Main-Path Skeleton

**Goal:** Establish the eight-crate target and a compiling skeleton for the simplified main path.

**Requirements:** SIMP-01, CRATE-01, CRATE-02, CRATE-03, CRATE-04

**Success criteria:**

1. Workspace manifests define the target crate topology or a clearly staged rename path toward it.
2. `lattice-llm` and `lattice-api` exist as active crates or crate-renamed replacements.
3. Removed/merged crates have explicit migration homes for the small pieces still needed by v1.1.
4. A skeleton main path exists in code as profile -> session -> agent -> llm -> tools -> commit -> render, even if later phases fill behavior.
5. `cargo check --workspace` succeeds or the phase documents narrowly-scoped follow-up blockers owned by Phase 7/8.

**Plans:** 3/3 plans complete

Plans:
**Wave 1**

- [x] 06-01-PLAN.md — Add workspace topology and standalone `lattice-llm` stub contract.

**Wave 2** *(blocked on Wave 1 completion)*

- [x] 06-02-PLAN.md — Add thin `lattice-api` facade, skeleton turn/session APIs, and migration homes.

**Wave 3** *(blocked on Wave 2 completion)*

- [x] 06-03-PLAN.md — Add isolated CLI `run-v11` smoke path and final boundary verification.

**Notes:**

- Do not preserve compatibility wrappers just to keep old names alive.
- Keep this phase mechanical enough that LLM/profile and session graph work can proceed in parallel afterward.

### Phase 7: Functional LLM and Direct Profiles

**Goal:** Implement small LLM call/router APIs and direct `agent.toml` profile parsing.

**Requirements:** LLM-01, LLM-02, LLM-03, LLM-04, LLM-05, PROF-01, PROF-02, PROF-03

**Success criteria:**

1. `chat()` and `stream()` are the primary provider call APIs and accept messages, tools, and options directly.
2. Model alias resolution reports provider, model, base URL, and API-key environment without hidden catalog behavior.
3. OpenAI, Anthropic, Gemini, and OpenAI-compatible provider paths are represented with focused tests.
4. `agent.toml` parses directly into `AgentProfile`.
5. Reader, editor, and dev templates exist and no main-path profile lowering/drift machinery remains.

**Notes:**

- Prefer moving existing transport code into the simpler crate shape over rewriting providers from scratch.
- Do not add new provider classes during this phase.

### Phase 8: Session Graph Core

**Goal:** Make sessions the durable graph of commits, branches, messages, HEAD, and refs.

**Requirements:** SESS-01, SESS-02, SESS-03, SESS-04, SESS-05

**Success criteria:**

1. Session metadata includes `session_id`, branch table, HEAD pointer, commits, messages, and refs.
2. Turn commits are immutable and store parents, messages, actor profile snapshot, model/provider metadata, and tool results.
3. Branch operations are pointer operations and test that they do not invoke model calls.
4. Session operations cover status, list, log, checkout, branch, diff, and reset.
5. SQLite persistence round-trips the graph state needed to reopen and continue a session.

**Notes:**

- Keep merge/rebase/stash out of the command and API surface.
- Preserve logical commit identity semantics; do not tie commit identity to storage implementation details.

### Phase 9: Agent Loop, Tools, and API Facade

**Goal:** Connect session state, LLM calls, simple tools, and `lattice-api` turn execution.

**Requirements:** AGENT-01, AGENT-02, TOOL-01, TOOL-02, TOOL-03, TOOL-04, API-01, API-02, API-03

**Success criteria:**

1. The agent loop consumes session state and user input, calls the LLM, executes requested tools sequentially, and stops on final assistant output.
2. Turn output includes generated messages, tool results, and usage/statistics needed for commits.
3. Built-in file/search/TODO tools work through project-root and file-size restrictions.
4. Optional web and shell tools are profile-gated.
5. `lattice-api` exposes create/open/run/stream and basic session operations without policy/prompt/sandbox internals.
6. Integration tests prove `run_turn` commits a full turn and `stream_turn` exposes useful events.

**Plans:** 4/4 plans complete

Plans:
**Wave 1**

- [x] 09-01-PLAN.md — Add current-HEAD transcript reads and turn usage/terminal metadata in `lattice-session`.
- [x] 09-02-PLAN.md — Add explicit enabled-tool resolution and project-root executor path in `lattice-tools`.

**Wave 2** *(blocked on Wave 1 completion)*

- [x] 09-03-PLAN.md — Add stable `lattice-agent::AgentLoop` with fake-LLM tests and sequential tool execution.

**Wave 3** *(blocked on Wave 2 completion)*

- [x] 09-04-PLAN.md — Wire durable `lattice-api` `run_turn`/`stream_turn` commits and public event/result mapping.

**Notes:**

- This is the first phase that should wire Phase 7 and Phase 8 outputs together.
- Keep tool restrictions transparent; do not recreate the old approval system.

### Phase 10: CLI Cutover

**Goal:** Reduce supported user-facing commands to the v1.1 CLI surface.

**Requirements:** CLI-01, CLI-02, CLI-03, CLI-04

**Success criteria:**

1. CLI help lists only supported v1.1 commands: `run`, `models`, `resolve`, `session ...`, `doctor`, and help.
2. `run <prompt> [--model|-m] [--profile] [--session]` creates or resumes a session and commits a turn through `lattice-api`.
3. `models --auth` and `resolve <model>` expose alias/provider/auth resolution clearly.
4. Session subcommands expose status, list, log, checkout, branch, diff, and reset.
5. TUI, pipeline, plugin, orchestrator, validation, merge, rebase, and stash surfaces are absent from supported help and command dispatch.

**Notes:**

- If a short-lived compatibility stub is needed, it must point users at the supported CLI path and not call old runtime code.
- Keep command output concise and scriptable.

### Phase 11: Deletion, Budget, and Release Verification

**Goal:** Delete compatibility leftovers, prove boundaries, meet the source-size budget, and update docs.

**Requirements:** SIMP-02, SIMP-03, CRATE-05, VER-01, VER-02, VER-03, VER-04, VER-05, VER-06

**Success criteria:**

1. `cargo check --workspace` and focused tests pass for LLM routing, profiles, sessions, tools, API, and CLI.
2. CLI smoke checks cover `run`, `models --auth`, `resolve`, `session status`, `session log`, and `doctor`.
3. Boundary greps show removed crates/concepts absent from active source and manifests.
4. Active Rust source under `crates/*/src` is roughly 20k lines or less.
5. README, AGENTS.md, CLAUDE.md, and codebase maps describe v1.1 accurately.
6. Final verification records deferred TUI, security, prompt, swarm, and plugin work as future milestones.

**Notes:**

- This phase should be deletion-heavy. If the source-size budget is missed, the phase is not complete.
- Use targeted checks first, then broader workspace gates once the rename/removal churn settles.

## Coverage

| Requirement | Phase | Status |
|-------------|-------|--------|
| SIMP-01 | Phase 6 | Pending |
| SIMP-02 | Phase 11 | Pending |
| SIMP-03 | Phase 11 | Pending |
| CRATE-01 | Phase 6 | Pending |
| CRATE-02 | Phase 6 | Pending |
| CRATE-03 | Phase 6 | Pending |
| CRATE-04 | Phase 6 | Pending |
| CRATE-05 | Phase 11 | Pending |
| LLM-01 | Phase 7 | Pending |
| LLM-02 | Phase 7 | Pending |
| LLM-03 | Phase 7 | Pending |
| LLM-04 | Phase 7 | Pending |
| LLM-05 | Phase 7 | Pending |
| SESS-01 | Phase 8 | Pending |
| SESS-02 | Phase 8 | Pending |
| SESS-03 | Phase 8 | Pending |
| SESS-04 | Phase 8 | Pending |
| SESS-05 | Phase 8 | Pending |
| PROF-01 | Phase 7 | Pending |
| PROF-02 | Phase 7 | Pending |
| PROF-03 | Phase 7 | Pending |
| AGENT-01 | Phase 9 | Complete | 2026-05-16 |
| AGENT-02 | Phase 9 | Complete | 2026-05-16 |
| TOOL-01 | Phase 9 | Complete | 2026-05-16 |
| TOOL-02 | Phase 9 | Complete | 2026-05-16 |
| TOOL-03 | Phase 9 | Complete | 2026-05-16 |
| TOOL-04 | Phase 9 | Complete | 2026-05-16 |
| API-01 | Phase 9 | Complete | 2026-05-16 |
| API-02 | Phase 9 | Complete | 2026-05-16 |
| API-03 | Phase 9 | Complete | 2026-05-16 |
| CLI-01 | Phase 10 | Pending |
| CLI-02 | Phase 10 | Pending |
| CLI-03 | Phase 10 | Pending |
| CLI-04 | Phase 10 | Pending |
| VER-01 | Phase 11 | Pending |
| VER-02 | Phase 11 | Pending |
| VER-03 | Phase 11 | Pending |
| VER-04 | Phase 11 | Pending |
| VER-05 | Phase 11 | Pending |
| VER-06 | Phase 11 | Pending |

**Coverage status:** 40 / 40 requirements mapped.

---
*Roadmap created: 2026-05-16 for v1.1 Cut Off Cleverness*
