# LATTICE

## What This Is

LATTICE is a Rust LLM runtime for running one well-instrumented agent through a stable CLI/TUI and programmatic runtime. The current project direction is to make the single-agent path solid before reintroducing optional higher-level features such as plugins, subagents, or orchestration.

## Core Value

A user can run one generic agent with reliable model routing, prompt/context assembly, sandboxed tools, session persistence, and CLI/TUI feedback without immature extension layers in the execution path.

## Current Milestone: v1.0 Single Agent Core

**Goal:** Reduce LATTICE to a coherent generic single-agent runtime and remove plugin/orchestrator/pipeline/subagent/code-agent-specific architecture from the current codebase.

**Target features:**
- A generic profile-to-runtime-to-agent turn path with no `code_agent` special case.
- Runtime-owned session, sandbox, prompt, tool, and approval wiring for one agent.
- Removal of plugin, official-plugin, pipeline, micro-agent, handoff, and orchestrator user-visible/runtime code paths.
- CLI/TUI commands and state model aligned around one generic agent.
- Compile-time and regression checks that prove removed layers no longer appear in the active workspace.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

- ✓ Existing Rust workspace provides model routing, prompt compilation, sandbox/tool execution, session storage, runtime turn streaming, and a CLI/TUI surface — codebase map, 2026-05-14.

### Active

<!-- Current scope. Building toward these. -->

- ✓ Generic single-agent profile and turn specification become first-class core runtime inputs — Validated in Phases 1-3, 2026-05-15.
- ✓ `lattice-agent` remains a generic agent loop and does not know about profile files, session stores, CLI commands, plugins, subagents, or orchestration — Validated in Phases 1-3, 2026-05-15.
- ✓ `lattice-runtime` is the only application-level assembly layer for profile, model routing, prompt, tools, sandbox, policy, memory/context, and session persistence — Validated in Phases 1-3, 2026-05-15.
- ✓ `lattice-cli` and TUI route through the generic runtime turn path rather than `code_agent`, plugin mode, or pipeline/orchestrator paths — Validated in Phases 1-3, 2026-05-15.
- ✓ Plugin, official plugin, orchestrator, pipeline, micro-agent, subagent, and handoff code is removed from the current workspace rather than hidden behind unused commands — Validated in Phase 4, 2026-05-15.
- ✓ Verification covers build closure, dependency boundary cleanup, and absence of removed concepts from active runtime/CLI/TUI code — Validated in Phase 4, 2026-05-15.

### Out of Scope

<!-- Explicit boundaries. Includes reasoning to prevent re-adding. -->

- Plugins and official plugin bundles — immature extension layer; defer until single-agent core is stable.
- Subagents, micro-agents, handoff, pipelines, and orchestrator bus — multi-agent composition is not part of this milestone.
- A specialized `code_agent` architecture — coding is a profile/tool capability, not an agent type.
- New provider transports, new model catalog work, or new tools unrelated to the refactor — this milestone is architectural convergence, not feature expansion.
- Large mechanical splits of `lattice-session`, `lattice-tools`, or TUI files unless required to complete the single-agent cutover — avoid unrelated churn.

## Context

The existing workspace already contains the ingredients for the desired core path: `lattice-core`, `lattice-prompt`, `lattice-policy`, `lattice-sandbox`, `lattice-session`, `lattice-hooks`, `lattice-tools`, `lattice-agent`, `lattice-runtime`, and `lattice-cli`.

Current codebase maps show that `lattice-plugin`, `lattice-plugins`, and `lattice-orchestrator` add plugin DAGs, official plugin bundles, pipeline handoff/fork, event bus, micro-agent bus, websocket/status surfaces, and profile assembly. The current runtime and CLI still import those layers for `AgentTurnSpec`, `AgentProfile`, `AgentPreset::Coding`, `AgentRegistry`, `Pipeline`, `PluginRegistry`, `MicroAgent`, and plugin overlays. This milestone exists to preserve the durable profile/runtime/agent/session/tool seams while deleting immature extension layers from active code.

The user explicitly wants one generic agent, not a `code_agent`, and wants plugin/subagent/orchestration code absent from the current codebase until the single-agent path is solid.

## Constraints

- **Architecture**: Keep lower crates independent of upper crates — `agent` must not depend on runtime/CLI/profile loading, and session must not know about agents.
- **Scope**: Remove immature extension layers from code, not only from CLI help or default config.
- **Compatibility**: Preserve a working CLI/TUI main path for `run`/interactive single-agent turns.
- **Security**: Runtime tool executors must remain bound to runtime sandbox/policy configuration after orchestration/plugin removal.
- **Persistence**: Session saving and turn manifest behavior must remain available through the runtime path.
- **Reviewability**: Prefer phased commits and targeted tests because this is a broad dependency rewrite.

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Build one generic agent path before reintroducing extensions | A specialized coding agent or plugin/orchestrator layer will keep polluting runtime, profile, and TUI types | ✓ Done — Phases 1-4 |
| Remove plugin/orchestrator/pipeline/subagent code from the current workspace | Hidden unused layers still affect dependencies, tests, naming, and architecture decisions | ✓ Done — Phase 4 deleted 3 crates, 66 wiki files, 2 guard tests |
| Treat coding behavior as profile/tool capability, not an agent subtype | The agent loop should stay generic; tools and prompt decide behavior | ✓ Done — Phases 1-3 |
| Make `profile` a first-class main-path concept | The useful part of orchestrator today is declarative agent config and lowering, not orchestration | ✓ Done — Phases 2-3 |

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
*Last updated: 2026-05-14 after starting milestone v1.0 Single Agent Core*
