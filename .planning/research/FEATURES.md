# Feature Research

**Domain:** Generic single-agent runtime
**Researched:** 2026-05-14
**Confidence:** HIGH

## Table Stakes

### Generic Agent Execution

- One `Agent` execution loop that is not specialized as a coding agent.
- One runtime turn API that builds/reuses a runtime-owned handle and streams events.
- Generic profile/turn spec input describing model, prompt, tools, behavior, working directory, and security intent.
- No plugin, subagent, pipeline, handoff, or orchestrator path in active runtime execution.

### Profile And Configuration

- Profile is declarative data, not live runtime state.
- Profile lowering produces a pure turn/build spec.
- Coding behavior is expressed by enabled tools and prompt text, not an `AgentPreset::Coding` branch.
- Profile schema excludes `[plugins]`, `[handoff]`, and `[orchestrator]` sections for this milestone.

### Runtime Assembly

- Runtime owns model resolution, tool capability selection, tool executor construction, sandbox/policy binding, prompt providers, session seeding, memory/context, and turn persistence.
- Runtime handle keys do not include plugin overlay or coding preset fields.
- Runtime does not expose pipeline, micro-agent, event bus, plugin registry, plugin directory loading, or runtime-backed orchestrator executor APIs.

### CLI/TUI

- CLI `run` and TUI use the same generic runtime path.
- The existing `code` command is either removed or becomes a thin alias/profile selection with no separate `coding_agent.rs` architecture.
- TUI removes `/plugins`, `/plugin`, plugin menus, plugin statusline, plugin handles, and plugin overlay state.
- TUI continues to support model/provider selection, permissions/approval, session restore, transcript rendering, and tool/event display.

### Verification

- Workspace no longer contains `lattice-plugin`, `lattice-plugins`, or `lattice-orchestrator` members.
- Active source no longer imports `lattice_plugin`, `lattice_plugins`, or `lattice_orchestrator`.
- Active source no longer contains `RuntimePluginOverlay`, `PluginRegistry`, `Pipeline`, `MicroAgent`, `AgentPreset::Coding`, or `coding_agent` modules.
- Targeted tests cover runtime turn creation, CLI run path, TUI runtime guards, tool exposure, sandbox binding, and session persistence.

## Differentiators

- The core design treats “coding” as a profile/tool composition, not a product-specific agent type.
- The runtime becomes easier to embed because it has one active control path.
- Removed layers become future external milestones, not dormant code in the main workspace.

## Anti-Features

| Anti-feature | Why excluded |
|--------------|--------------|
| Plugin DAGs | Reintroduce plugin-specific prompt/tool/schema coupling before the agent core is stable. |
| Official plugin bundles | Expand feature surface and security review matrix. |
| Pipeline handoff/fork | Multi-agent composition is explicitly out of scope. |
| Micro-agent bus / websocket status | Belongs to orchestration, not single-agent runtime. |
| Specialized code-agent command architecture | Locks the runtime around one use case and creates preset branches. |

## Sources

- User milestone clarification: one generic agent, not `code_agent`; plugin/subagent/orchestrator code should not exist for now.
- `.planning/codebase/ARCHITECTURE.md` — current data flow and plugin/pipeline coupling.
- `rg` scans of `crates/lattice-runtime` and `crates/lattice-cli` on 2026-05-14.

---
*Feature research for: Generic single-agent runtime*
*Researched: 2026-05-14*
