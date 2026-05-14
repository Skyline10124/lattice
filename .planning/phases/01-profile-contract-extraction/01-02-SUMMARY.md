---
phase: "01-profile-contract-extraction"
plan: "02"
tasks_completed: 3
created: "2026-05-14T11:25:00Z"
has_checkpoints: false
key-files:
  created:
    - crates/lattice-profile/src/discovery.rs
    - crates/lattice-profile/src/defaults.rs
    - crates/lattice-profile/src/spec.rs
    - crates/lattice-profile/src/lowering.rs
  modified:
    - crates/lattice-profile/src/lib.rs
    - crates/lattice-profile/src/error.rs
---

## Plan 01-02 Summary: Discovery, defaults, and pure lowering

### What was built

**Discovery (discovery.rs):** `ProfileSource` (Path/Name/Default) and `DiscoveryPaths`
implement D-02/D-05/D-06 semantics. Explicit paths load directly. Named lookup checks
`.lattice/agents/<name>/agent.toml` before `~/.lattice/agents/<name>/agent.toml`.
Default uses name "default" with the same project-first/global-second order. Missing
named profiles return `ProfileNotFound`; missing default returns `MissingDefault`
with template text and proposed path (D-07: no filesystem writes).

**Defaults (defaults.rs):** `default_agent_template(name, model)` generates strict v1
TOML with comments telling users to edit role, prompt, tools, and security (D-08).
Template includes only allowed sections: `[agent]`, `[prompt]`, `[tools]`, `[security]`,
`[context]`, `[behavior]`. Zero legacy fields or preset strings.

**Spec (spec.rs):** `GenericAgentSpec` and nested spec types carry pure profile-derived
data with no runtime/agent/orchestrator/plugin/session dependencies. No `cwd`,
`project_root`, `handoff`, `orchestrator`, `preset`, or `Code` fields (D-18, D-19).

**Lowering (lowering.rs):** `lower_profile_to_spec()` converts a `LoadedAgentProfile`
into a `GenericAgentSpec`. Validates non-empty name, model, and system prompt. Pure
data copy — no provider resolution, credential checks, tool expansion, or sandbox
construction.

### Task Completion

| Task | Status | Commits |
|------|--------|---------|
| Task 1: Discovery | Complete | feat: agent discovery and v1 default template |
| Task 2: Default template | Complete | (bundled with Task 1) |
| Task 3: Pure spec and lowering | Complete | feat: GenericAgentSpec and profile lowering |

### Tests

- **55 tests pass** in `lattice-profile` (46 prior + 9 new for discovery/defaults/lowering)
- Source guard tests verify no forbidden imports/symbols in production code
- All discovery priority and override semantics covered

### Self-Check: PASSED

- `cargo test -p lattice-profile` — 55 passed
- No forbidden operations or symbols in production code
- `cargo check` — 0 errors
