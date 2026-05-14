# LATTICE

## What This Is

LATTICE is a Rust workspace LLM engine with model routing, agent execution, prompt lifecycle control, plugin runtime, session persistence, and orchestrator coordination. The platform is not only a code agent: the code agent is the first flagship main agent built on reusable capability foundations.

## Core Value

Clear dependency direction and capability boundaries -- each layer owns its own business, and powerful agent abilities are opt-in capabilities rather than global assumptions.

## Current Milestone: v1.3 Generic Agent Runtime & Session Graph

**Goal:** Generalize the v1.2 flagship code-agent foundation into runtime-owned generic agent instances while preserving a focused git-style session graph.

**Target features:**
- Replace `code_agent`-specific internal naming and APIs with generic agent/profile/runtime abstractions, keeping `lattice code` as a product preset.
- Move live agent ownership, turn execution, streaming, task state, security, and session binding into `lattice-runtime`.
- Make CLI and TUI frontend code submit turns through runtime handles/events instead of owning `Arc<Mutex<Agent>>` directly.
- Refine `lattice-session` around durable conversation graph semantics while retaining core commit, branch, checkout, diff, merge, and cherry-pick operations.
- Decouple `lattice-agent` turn manifests from branch and git commit concepts; runtime/session layers record graph and workspace metadata.

## Current State

**Shipped:** v1.2 CC/Codex Capability Foundation & Code Agent (2026-05-12)

- Capability bundles and presets make powerful agent abilities opt-in.
- Structured patch editing supports multi-file edits with diagnostics, hashes, sandbox preflight, rollback, and changed-file manifests.
- Permission and approval flow returns typed denial reports and applies scoped approvals through shared runtime security state.
- Agent task/recovery state persists todos, changed files, tool failures, verification results, and unverified work across turns and manifests.
- `Runtime::build_code_agent` is the shared factory for the flagship code agent.
- Deterministic code-agent evals cover dirty worktree safety, project instructions, targeted reads, patch recovery, verification honesty, profile isolation, and gap-analysis linkage.
- v1.2 roadmap, requirements, and audit artifacts are archived under `.planning/milestones/`.

**Validated in Phase 15: Generic Agent Runtime Naming & Profile Assembly (2026-05-13)**

- Runtime exposes generic `AgentSpec`, `AgentPreset`, and `Runtime::build_agent` concepts; legacy `CodeAgentSpec` / `build_code_agent` are removed from crate source.
- `lattice code` uses `AgentSpec + AgentPreset::Coding` while preserving the v1.2 eval behavior anchors.
- `code-agent` remains only as a compatibility alias for capability parsing/deserialization or as historical/user-facing wording.

**Previously shipped:** v1.1 Prompt Lifecycle & Session Runtime (2026-05-10), v1.0 Tech Debt Cleanup & Architecture Refactoring (2026-05-08)

- `lattice-sandbox` extracted as neutral crate, breaking circular dependency.
- `lattice-core` decoupled as true leaf crate.
- Tool modules consolidated into `lattice-tools`.
- Target DAG achieved: `core -> {prompt,hooks,session,sandbox,tools,plugin} -> agent -> orchestrator -> runtime -> CLI`.

## Requirements

### Validated

- CI passes clean with zero clippy warnings -- v1.0
- Dead code removed (3122 lines mod.rs, stale tests, release.yml) -- v1.0
- once_cell migrated to std::sync::LazyLock -- v1.0
- Security hardening (audit logging, credential zeroization, async HTTP) -- v1.0
- lattice-sandbox extracted with trait-abstracted hooks/audit -- v1.0
- Tool modules consolidated into lattice-tools -- v1.0
- lattice-core is true leaf crate -- v1.0
- Duplicate DefaultToolExecutor removed -- v1.0
- lattice-bus renamed to lattice-orchestrator -- v1.0
- lattice-registry merged into orchestrator -- v1.0
- Target DAG verified -- v1.0
- Agent loop uses typed prompt contribution path -- v1.1
- Agent profile loads PromptSpec and built-in provider factories -- v1.1
- Static PromptSectionSpec becomes PromptContribution through registry pipeline -- v1.1
- Declarative hook builder supports add/remove/set-budget/abort -- v1.1
- TurnManifestObject emitted with hook/tool decisions and PromptSnapshot metadata -- v1.1
- Capability bundles/presets separate platform abilities from agent-specific behavior -- v1.2
- Code-agent profile composes filesystem/editing/planning/shell/coding capabilities without leaking them to all agents -- v1.2
- Apply-patch style editing engine supports multi-file edits with sandbox checks and diagnostics -- v1.2
- Permission and approval model explains risky commands and supports scoped approvals -- v1.2
- Task state persists todos, tool failures, verification status, and unverified work across turns/sessions -- v1.2
- Main code agent uses project instructions, workspace context, safe edit flow, verification discipline, and concise final reporting -- v1.2
- Eval harness proves code-agent behavior and profile isolation -- v1.2
- Generic agent specs and factories replace code-agent-specific internal APIs while preserving the `lattice code` behavior surface -- v1.3 Phase 15

### Active

- [ ] Runtime owns live agent instances, turn lifecycle, streaming, security state, task state, and session persistence handoff -- v1.3
- [ ] CLI/TUI become runtime adapters rather than direct `Agent` owners or runners -- v1.3
- [ ] Session graph interfaces distinguish core durable graph operations from optional advanced git-style helpers -- v1.3
- [ ] Agent manifests and configuration no longer require branch names or parent git commit IDs inside `lattice-agent` -- v1.3
- [ ] Regression and eval coverage proves v1.2 code-agent behavior survives the generic runtime migration -- v1.3

### Out of Scope

- Turning all of Lattice into a code-agent-only product -- code agent is the first main profile, not the whole platform.
- Remote/cloud task execution -- depends on stable local agent/session foundations.
- Full IDE extension or app server parity -- future product surface after local CLI/TUI loop is strong.
- External client protocol/app-server work -- deferred until local runtime ownership and session graph boundaries are clean.
- Full MCP client/server ecosystem -- deferred unless needed for code-agent parity after local tools are solid.
- Replacing existing plugin/orchestrator abstractions with Codex/Claude Code clones -- preserve Lattice architecture.
- Flattening sessions into a plain transcript store -- session keeps core git-style graph management.
- Removing core session graph operations such as branch, checkout, diff, merge, or cherry-pick -- these remain part of the base capability.
- Hooks moved to agent layer -- lattice-hooks stays as standalone crate.
- BlobStore type relocation -- deferred until needed.

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Bottom-up phase ordering | Clean CI first before structural changes | Good |
| Phase 1 combines CLEAN + SEC | Must unblock CI before any structural work | Good |
| Sandbox as neutral crate | Break agent/tools circular dependency | Good |
| ARCH-04 in Phase 4, not 3 | Keeps core decoupling focused | Good |
| Registry traits as stubs in orchestrator | Preserve design intent | Good |
| Hooks stays standalone | User decision, overrides PROJECT.md aspiration | Good |
| Code agent is flagship profile, not product boundary | Lattice remains a model-centric engine; code agent proves capability depth | Good |
| CC/Codex powers become capability bundles | Reuse across future agents while keeping non-coding profiles isolated | Good |
| Editing engine belongs in lattice-tools, profile selection belongs in runtime/orchestrator | Keeps implementation local while making exposure explicit | Good |
| Final report truth comes from task state, not model prose | Prevents unverified or failed work from being reported as passed | Good |
| Approval retry is explicit, proposal-driven, and non-replaying | Keeps user approval separate from agent retry semantics | Good |
| Runtime owns live agent instances | Frontends should adapt runtime events and handles, not hold concrete Agent locks | Pending v1.3 |
| Session remains a graph, not a transcript-only store | Branch, checkout, diff, merge, and cherry-pick are essential conversation management primitives | Pending v1.3 |
| `code_agent` becomes a preset surface, not the internal generic name | The platform should serve general agent needs while preserving the flagship coding command | Validated in Phase 15 |

## Context

The v1.2 direction was informed by local Codex and Claude Code source analysis plus existing `.planning/research/CC_CODEX_GAP_ANALYSIS.md`. That milestone closed the foundation gaps: capability isolation, editing semantics, permission UX, task/session state, recovery, and behavioral evals. v1.3 turns that flagship code-agent foundation into generic runtime/session architecture rather than adding external app-server or protocol clients.

Current codebase state:
- Rust workspace with crates for core, prompt, hooks, session, sandbox, tools, agent, orchestrator, runtime, plugin, plugins, and CLI.
- `lattice code` is assembled through the generic runtime `AgentSpec` path with `AgentPreset::Coding`, reusable capabilities, prompt providers, task state, runtime security, and authorized tools.
- Structured patch editing and changed-file manifests now live in `lattice-tools`; final reporting and recovery state live in `lattice-agent`.
- `Runtime::build_agent` now assembles the flagship coding preset, but runtime does not yet own reusable live agent instances or turn lifecycle.
- The TUI currently holds `Arc<Mutex<Agent>>` and runs agent streams through frontend runner code; v1.3 should invert that ownership.
- `lattice-session` already models conversation refs, HEAD, lineage, commits, branch operations, diff, merge, cherry-pick, and advanced helpers; v1.3 should refine boundaries without deleting the core graph model.
- The remaining product frontier is less about adding new external surfaces and more about making the local runtime/session foundation generic enough for future TUI, MCP, IDE, app-server, and multi-agent workstream UX.

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
*Last updated: 2026-05-13 after Phase 15 completion*
