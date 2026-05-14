# Architecture Research

**Domain:** Generic single-agent runtime
**Researched:** 2026-05-14
**Confidence:** HIGH

## Recommended Architecture

```text
CLI / TUI
   |
   v
lattice-runtime
   |  owns assembly, handles, turns, sessions, security
   v
lattice-profile
   |  declarative profile + pure turn/build spec
   v
lattice-agent
   |  generic loop, prompt consumption, tool calls, manifests
   +--> lattice-core       model routing / streaming
   +--> lattice-prompt     prompt providers / compilation
   +--> lattice-tools      schemas / executor
   +--> lattice-sandbox    path / command / URL checks
   +--> lattice-policy     approval / risk / denials
   +--> lattice-session    runtime-owned persistence
   +--> lattice-hooks      lifecycle hooks
```

`lattice-profile` may be a new crate or a runtime-adjacent module during migration. Its responsibility is profile data, validation, and lowering. It should not construct live agents or know about CLI/TUI.

## Component Responsibilities

| Component | Responsibility | Required Change |
|-----------|----------------|-----------------|
| `lattice-agent` | Generic turn loop, state, prompt registry consumption, tool call execution, manifests | Keep generic; remove assumptions exposed through coding preset tests if they require agent specialization. |
| `lattice-profile` | Agent config schema, profile loading, validation, pure turn/build spec | Extract from `lattice-orchestrator`; remove plugin/handoff/orchestrator fields. |
| `lattice-runtime` | Single assembly layer and handle/turn/session/security API | Stop depending on orchestrator/plugin crates; own profile-to-agent assembly. |
| `lattice-cli` | Command/TUI interaction | Route through generic runtime; remove plugin/orchestrator/code-agent modules and commands. |
| `lattice-tools` | Tool schemas, capability bundles, executor | Keep coding-capable bundles as tools, not as an agent preset. |
| `lattice-session` | Conversation graph and persistence | Stay unaware of agent/profile. |

## Data Flow

```text
Profile file or CLI defaults
    -> Profile validation/lowering
    -> Runtime resolves model and builds tool/sandbox/prompt/session context
    -> Runtime creates/reuses Agent handle
    -> Agent runs one multi-turn loop
    -> Runtime persists session + manifests
    -> CLI/TUI renders RuntimeTurnEvent + TurnCompletion
```

## Migration Pattern

1. Extract pure data contracts first: profile structs, capability config, behavior config, and turn spec.
2. Move agent assembly out of orchestrator and into runtime/profile assembly.
3. Cut CLI/TUI to generic runtime path before deleting crates.
4. Delete plugin/orchestrator crates only after compile closure no longer depends on them.
5. Rename tests and fixtures from `coding_agent` to generic agent/runtime terms.

## Anti-Patterns

### Hide But Keep

**What people do:** Remove commands but leave plugin/orchestrator crates and runtime fields.
**Why it is wrong:** Dormant code still shapes dependencies, tests, types, and future design.
**Do this instead:** Remove workspace members and active imports.

### Coding Preset As Architecture

**What people do:** Preserve `AgentPreset::Coding` as the “real” path.
**Why it is wrong:** It makes one product mode privileged and prevents a generic agent core.
**Do this instead:** Use profile/tool/prompt configuration for coding behavior.

### Agent Reads The World

**What people do:** Let `Agent` load profiles, sessions, or CLI configuration.
**Why it is wrong:** It collapses runtime orchestration into the agent loop.
**Do this instead:** Runtime assembles; agent executes.

## Integration Points

| Boundary | Communication | Notes |
|----------|---------------|-------|
| CLI/TUI -> Runtime | Runtime APIs and event streams | UI never owns live `Agent`. |
| Runtime -> Profile | Pure data structs and validation/lowering | Profile has no live runtime dependencies. |
| Runtime -> Agent | Constructed `Agent` plus tools/prompt/memory/security | Runtime owns assembly policy. |
| Runtime -> Session | `SessionStore` trait | Runtime persists turns/manifests; session stays generic. |
| Runtime -> Tools/Sandbox/Policy | Explicit sandbox-bound executor | Must preserve current security guarantees. |

## Sources

- `.planning/codebase/ARCHITECTURE.md`
- `.planning/codebase/STRUCTURE.md`
- `.planning/codebase/CONCERNS.md`
- Current `rg` evidence in runtime/CLI/orchestrator/plugin crates.

---
*Architecture research for: Generic single-agent runtime*
*Researched: 2026-05-14*
