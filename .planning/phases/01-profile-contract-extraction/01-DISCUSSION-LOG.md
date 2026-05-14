# Phase 1: Profile Contract Extraction - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-14
**Phase:** 1-Profile Contract Extraction
**Areas discussed:** Profile contract boundary, crate/module placement, compatibility strategy, `code` command strategy, tool capability vocabulary

---

## Profile Contract Boundary

| Option | Description | Selected |
|--------|-------------|----------|
| Stable user config | Profile expresses model, prompt, tools, coarse security intent, context toggles, and behavior knobs; runtime lowers it into internal spec. | ✓ |
| Semi runtime spec | Profile also carries session seed, memory/provider choices, and other lower-level runtime fields. | |
| Full runtime spec | Profile is essentially the runtime build spec. | |

**User's choice:** Stable user config.
**Notes:** User asked that the profile accept an agent profile file and wanted to define how that file is laid out and discovered. Decisions favored existing `agents/<name>/agent.toml` semantics, KISS schema, no `cwd`/`project_root` in profile, strict schema validation, default-agent generation with confirmation, and current coarse `sandbox_mode` only.

---

## Profile File Shape and Discovery

| Option | Description | Selected |
|--------|-------------|----------|
| New `.lattice/profiles/<name>/profile.toml` | Rename physical storage to profiles/profile.toml. | |
| Keep `.lattice/agents/<name>/agent.toml` | Preserve existing path semantics because it more clearly names runnable agent definitions. | ✓ |
| Single flat file | Store profiles as flat TOML files without package folders. | |

**User's choice:** Keep old path semantics.
**Notes:** User said the old path semantics are clearer. Each agent remains a package folder with `agent.toml` and optional neighboring files such as `prompt.md`. Project-local lookup overrides global lookup.

---

## Default Agent Behavior

| Option | Description | Selected |
|--------|-------------|----------|
| Built-in default only | Runtime uses an internal default if no file exists. | |
| Auto-generate default template with interaction | Missing default prompts the user, then writes project-local `.lattice/agents/default/agent.toml`; non-interactive mode errors. | ✓ |
| Require user to create default manually | No generation; tell user to run a setup command. | |

**User's choice:** Generate a default when no template is detected, but use interactive confirmation and project-local destination.
**Notes:** User also requested the default prompt include a note telling the user to modify the template.

---

## Crate or Module Placement

| Option | Description | Selected |
|--------|-------------|----------|
| New `lattice-profile` crate | Narrow crate for schema, loading, discovery, validation, defaults, errors, and pure lowering. | ✓ |
| `lattice-runtime::profile` module | Put profile code under runtime. | |
| `lattice-agent` module | Put profile parsing in the agent loop crate. | |
| Keep in `lattice-orchestrator` | Leave current profile/spec code in orchestrator. | |

**User's choice:** New narrow crate.
**Notes:** User said to avoid turning one crate into a dumping ground. `lattice-profile` should avoid runtime/orchestrator/agent/session/plugin dependencies and should handle clear errors, including `system_path` existence and path-escape checks.

---

## Compatibility Strategy

| Option | Description | Selected |
|--------|-------------|----------|
| Strict fail-fast | Reject old/unknown sections and fields; no compatibility layer. | ✓ |
| Warnings and ignore | Warn about old fields but continue. | |
| Automatic migration | Provide migration command or dry-run rewrite. | |

**User's choice:** Strict fail-fast; no migration.
**Notes:** User said the project has not launched and compatibility is unnecessary. User rejected migration because it only increases work. Errors must remain explicit and helpful.

---

## `code` Command Strategy

| Option | Description | Selected |
|--------|-------------|----------|
| Delete `lattice code` | Coding is just a normal agent profile composed from prompt + tools. | ✓ |
| Keep `code` as alias | Make `lattice code` a thin wrapper around `lattice run --agent code`. | |
| Rename to another profile command | Redesign CLI around a new agent subcommand. | |

**User's choice:** Delete `lattice code`.
**Notes:** User stated that a coding agent is nothing more than tools plus a specific prompt. Future usage should be generic runtime execution with an agent profile, e.g. `lattice run --agent code ...`.

---

## Tool Capability Vocabulary

| Option | Description | Selected |
|--------|-------------|----------|
| Keep `[capabilities]` and presets | Preserve current presets/bundles structure. | |
| `[tools]` only, keep bundles and enabled | Use bundles plus explicit enabled tool names; no profile-level capabilities section. | ✓ |
| Bundles only | Remove `enabled` and require every tool to be exposed through bundles. | |

**User's choice:** `[tools]` only with `bundles` and `enabled`.
**Notes:** User asked to confirm current bundles. Current bundles are `core`, `workspace`, `filesystem`, `editing`, `shell`, `planning`, and `web`. User chose to delete presets now while keeping the existing bundle scope for this phase. `enabled` remains for individual tools.

---

## the agent's Discretion

- Exact Rust module names inside `lattice-profile` are left to planning/implementation as long as the crate remains narrow.
- Exact name of the pure lowered spec type is flexible as long as it remains generic and free of plugin/handoff/orchestrator/subagent/coding-preset semantics.

## Deferred Ideas

None.
