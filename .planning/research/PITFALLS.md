# Pitfalls Research

**Domain:** Removing plugin/orchestrator/code-agent layers from a Rust LLM runtime
**Researched:** 2026-05-14
**Confidence:** HIGH

## Common Mistakes

### Deleting Orchestrator Before Extracting Profile

- **Risk:** `AgentTurnSpec`, `AgentProfile`, capability config, behavior config, profile loading, and assembly currently live in or depend on `lattice-orchestrator`.
- **Consequence:** Direct deletion causes widespread compile failure and encourages quick compatibility shims.
- **Prevention:** Extract profile/spec/assembly first, then remove pipeline/micro-agent/bus pieces.

### Keeping `AgentPreset::Coding`

- **Risk:** Runtime currently treats coding as a preset that changes prompt, tools, workspace providers, final report persistence, and tests.
- **Consequence:** The project still has a code-agent architecture even if `coding_agent.rs` is renamed.
- **Prevention:** Convert coding behavior into default profile/tool selection and generic runtime options.

### Removing Plugin Crates But Leaving TUI Plugin State

- **Risk:** TUI contains plugin mode, plugin handles, plugin status labels, `/plugins`, `/plugin`, and runtime plugin overlays.
- **Consequence:** Compile errors and confusing UI remnants.
- **Prevention:** Treat TUI plugin removal as a first-class phase, including tests and slash command help.

### Weakening Sandbox Binding

- **Risk:** Existing assembly path threads runtime sandbox config into tool executors through orchestrator assembly and runtime security code.
- **Consequence:** Refactoring assembly can accidentally fall back to permissive low-level defaults.
- **Prevention:** Add/keep regression tests proving production runtime-built executors use `RuntimeSecurity`.

### Breaking Session Manifest Persistence

- **Risk:** Runtime turn completion saves manifests through session store support; code-agent metadata helpers are mixed into current CLI paths.
- **Consequence:** Chat may persist while manifests/task metadata silently degrade.
- **Prevention:** Preserve generic turn manifest save/load helpers and test CLI default SQLite store behavior.

### Over-Refactoring Large Files

- **Risk:** `lattice-runtime/src/lib.rs`, `lattice-session/src/lib.rs`, `lattice-tools/src/executor.rs`, and TUI `app.rs` are large.
- **Consequence:** Combining architectural deletion with mechanical file splits makes review and regression diagnosis hard.
- **Prevention:** Split only where needed for dependency removal; defer broader modularization.

### Documentation Drift

- **Risk:** AGENTS/CLAUDE/README and codebase maps describe the old plugin/orchestrator layering.
- **Consequence:** Future agents may reintroduce removed layers.
- **Prevention:** Update architecture docs after code removal and add boundary checks to roadmap verification.

## Warning Signs

- `rg "lattice_orchestrator|lattice_plugin|lattice_plugins"` still finds active source imports after the refactor.
- `rg "PluginRegistry|RuntimePluginOverlay|Pipeline|MicroAgent|AgentPreset::Coding|coding_agent"` still finds runtime/CLI/TUI code.
- CLI help still exposes pipeline, agent registry, plugin, or orchestrator status commands.
- TUI statusline or help still mentions plugins.
- Runtime builder still has plugin directory loading or pipeline APIs.
- Tests pass only because old code is hidden behind feature flags rather than removed.

## Phase Guidance

- Profile extraction should happen before crate deletion.
- Runtime assembly should be migrated before CLI/TUI deletion, so the app has a working generic path.
- CLI/TUI cleanup should happen before final dependency deletion to avoid broken user-facing paths.
- Final cleanup should include source grep checks and docs updates.

## Sources

- `.planning/codebase/CONCERNS.md`
- `.planning/codebase/TESTING.md`
- `rg` scans of current source on 2026-05-14

---
*Pitfalls research for: Single-agent architecture cleanup*
*Researched: 2026-05-14*
