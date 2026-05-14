# Phase 1: Profile Contract Extraction - Context

**Gathered:** 2026-05-14
**Status:** Ready for planning

<domain>
## Phase Boundary

This phase establishes a small, strict, generic agent profile contract for LATTICE v1.0. It extracts the useful declarative agent definition pieces from the old orchestrator/profile path while removing plugin, handoff, orchestrator, pipeline, subagent, and coding-preset semantics. The phase produces the profile schema, loader/discovery behavior, validation/error model, default-agent template rules, and pure profile-to-turn/build spec lowering needed by later runtime and CLI cutover phases.

This phase does **not** build live agents, execute tools, resolve models, persist sessions, preserve the old `code_agent` path, or provide migration compatibility for pre-v1 profiles.

</domain>

<decisions>
## Implementation Decisions

### Profile File Contract
- **D-01:** A profile is a stable user-facing configuration file for a single runnable agent, not a full runtime spec and not a runtime assembly script.
- **D-02:** Keep the existing and clearer physical semantics: each agent is an agent package folder at `.lattice/agents/<name>/agent.toml` or `~/.lattice/agents/<name>/agent.toml`. Do not introduce `.lattice/profiles/` or `profile.toml` in v1.
- **D-03:** The `agent.toml` file defines one generic agent profile. Neighbor files such as `prompt.md` are allowed inside the same package folder.
- **D-04:** Relative paths inside `agent.toml`, such as `prompt.system_path`, resolve relative to the directory containing that `agent.toml`, not relative to the process cwd.
- **D-05:** Explicit file paths take highest priority. Named lookup checks the project-local agent before the user-global agent. Project-local agents override global agents with the same name.
- **D-06:** When no agent is specified, LATTICE uses the `default` agent. It first looks for `.lattice/agents/default/agent.toml`, then `~/.lattice/agents/default/agent.toml`.
- **D-07:** If no default agent exists, an interactive terminal prompts before creating `.lattice/agents/default/agent.toml`. Non-interactive environments fail with a clear error instead of writing files unexpectedly.
- **D-08:** The generated default agent template must include prompt text and comments telling the user it is an auto-generated template and should be edited for the project, including role, prompt, tools, and security settings.

### Minimal v1 Schema
- **D-09:** The v1 schema is strict allowlist only. Unknown sections or fields are errors. This project has not shipped, so compatibility with old profile fields is not required.
- **D-10:** The allowed top-level sections are `schema_version`, `[agent]`, `[prompt]`, `[tools]`, `[security]`, `[context]`, and `[behavior]`.
- **D-11:** `[agent]` carries stable identity fields such as `name`, `model`, and optional `description`. It must not carry `preset`, `type = "code_agent"`, `skippable`, or `tags`.
- **D-12:** `[prompt]` supports either inline `system` or `system_path`, but not both. `system_path` must exist, must be a file, and must not escape the agent package directory.
- **D-13:** `[security]` only carries the current coarse-grained runtime security intent: `sandbox_mode = "project" | "strict" | "permissive"`. Do not add profile-level approval mode, allowlists, Landlock, audit, or raw approval grants in Phase 1.
- **D-14:** `[context]` may carry simple context toggles such as `project_instructions = true` and `workspace_context = true`. It must not carry `cwd` or `project_root`; cwd/project root come from the runtime invocation context.
- **D-15:** `[behavior]` may carry simple generic turn controls such as `max_turns` and `reasoning_effort`. It must not carry old orchestrator behavior fields such as `behavior_type`, `confidence_threshold`, `max_retries`, or `enforce_sandbox`.

### Crate Boundary
- **D-16:** Create a new narrow `lattice-profile` crate instead of piling schema, loading, discovery, validation, defaults, and lowering into `lattice-runtime`, `lattice-agent`, or the old `lattice-orchestrator` crate.
- **D-17:** `lattice-profile` owns strict schema types, file/package loading, discovery, default template generation, clear profile errors, `system_path` checks, and pure data lowering into the generic turn/build spec.
- **D-18:** `lattice-profile` must not depend on `lattice-runtime`, `lattice-orchestrator`, `lattice-agent`, `lattice-session`, `lattice-plugin`, or CLI/TUI state. It must not construct a live `Agent`, read session stores, execute tools, or load plugins.
- **D-19:** Runtime remains responsible for environment-dependent semantic validation: model alias/provider/credentials, actual tool bundle and tool name existence, sandbox construction, and agent handle construction.
- **D-20:** `lattice-agent` remains a generic agent loop and does not depend on `lattice-profile` or know about profile files.

### Compatibility and Errors
- **D-21:** There is no compatibility layer, warning-ignore mode, dry-run migrator, or automatic migration command for old profiles. Old fields fail fast.
- **D-22:** Explicitly forbidden profile concepts include `[capabilities]`, `[handoff]`, `[orchestrator]`, `[plugins]`, `[pipeline]`, `[subagents]`, `[micro_agents]`, `[memory]`, `agent.preset`, `agent.type = "code_agent"`, `agent.skippable`, `agent.tags`, and old behavior fields.
- **D-23:** Profile errors must be concrete and user-actionable: include the file path, section/field/value that failed, why it is rejected, and the new schema shape to use instead.
- **D-24:** Tests should cover strict rejection of unknown sections/fields and every explicitly forbidden legacy section/field.

### CLI and Coding Experience
- **D-25:** Delete the `lattice code` subcommand rather than preserving it as an alias. Coding is not a special agent type.
- **D-26:** A coding-capable agent is just a normal agent profile with a coding-oriented prompt and selected tool bundles/tools. Users run it through the generic runtime path, for example `lattice run --agent code "fix the failing tests"`.
- **D-27:** Remove or rewrite `coding_agent`, `AgentPreset::Coding`, `code_agent`, coding-preset runtime branches, and tests/evals that only exercise the specialized coding-agent path.

### Tool Capability Vocabulary
- **D-28:** Agent profile v1 exposes tools only through `[tools]`; do not keep `[capabilities]` in the profile schema.
- **D-29:** `[tools]` keeps both `bundles` and `enabled`. Final exposed tools are the union of tools expanded from bundles plus explicitly enabled tool names, deduplicated by runtime/tooling.
- **D-30:** Delete the presets concept aggressively now. Remove `ToolPreset`, `CapabilitySelection.presets`, `coding`, `code-agent`, `reader`, and `minimal` preset paths rather than carrying them forward into the generic profile contract.
- **D-31:** Bundle vocabulary can keep the current `lattice-tools` scope for this phase: `core`, `workspace`, `filesystem`, `editing`, `shell`, `planning`, and `web`.
- **D-32:** `lattice-profile` only validates that `bundles` and `enabled` are arrays of strings. Runtime/`lattice-tools` validates actual bundle/tool existence and produces the semantic errors.

### the agent's Discretion
- The planner may choose exact Rust module names inside `lattice-profile` as long as the crate remains narrow and does not pull in runtime/agent/orchestrator dependencies.
- The planner may choose the exact pure-data spec type name (`AgentTurnSpec`, `AgentBuildSpec`, or similar) as long as it does not encode plugin, handoff, orchestrator, subagent, or coding-preset semantics.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project and Phase Scope
- `.planning/PROJECT.md` — milestone scope, single-agent core framing, out-of-scope extension layers, and key decisions.
- `.planning/REQUIREMENTS.md` — PROF-01, PROF-02, PROF-03 and cross-phase traceability.
- `.planning/ROADMAP.md` — Phase 1 goal, success criteria, and notes about extracting from orchestrator profile/spec code.
- `.planning/STATE.md` — current project position and accumulated decisions.

### Current Source Inputs
- `crates/lattice-orchestrator/src/profile.rs` — old TOML-backed `AgentProfile` schema; source to extract useful fields from and reject legacy plugin/handoff/orchestrator semantics.
- `crates/lattice-orchestrator/src/agent_spec.rs` — current pure-ish lowered turn spec and old `AgentPreset::Coding`; useful as input but must be rewritten without coding preset or handoff semantics.
- `crates/lattice-orchestrator/src/registry.rs` — current folder-based `agents/<name>/agent.toml` registry loading behavior.
- `crates/lattice-orchestrator/src/lattice_dir.rs` — current project/global agent discovery and project-local override behavior.
- `crates/lattice-cli/src/commands/mod.rs` — current `LATTICE_AGENTS_DIR` and default `~/.lattice/agents` resolution.
- `crates/lattice-cli/src/commands/new_agent.rs` — current `agent.toml` template generation that must be replaced with the new v1 schema/template.
- `crates/lattice-cli/src/coding_agent.rs` — specialized coding-agent path to remove or rewrite through generic profile execution.
- `crates/lattice-cli/src/main.rs` — `Commands::Code`, `Run`, and CLI routing to update.
- `crates/lattice-runtime/src/lib.rs` — current runtime security modes, runtime handle/spec usage, and old orchestrator/plugin/coding-preset coupling to cut over later.
- `crates/lattice-tools/src/capabilities.rs` — current tool bundles and presets; presets should be removed, bundles initially retained.
- `crates/lattice-cli/src/security.rs` — current CLI-to-runtime security config mapping and supported sandbox mode behavior.
- `crates/lattice-cli/src/frontend/tui/app.rs` — TUI `/permissions` evidence that supported sandbox modes are `project`, `strict`, and `permissive`.

### Codebase Maps
- `.planning/codebase/STACK.md` — current workspace crate map and tool/security/profile extension layers.
- `.planning/codebase/ARCHITECTURE.md` — layered architecture and current orchestration/plugin/runtime coupling.
- `.planning/codebase/CONVENTIONS.md` — Rust workspace conventions, crate boundaries, error handling, tests, and formatting expectations.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `AgentRegistry::load_dir` currently scans subdirectories for `agent.toml`; this pattern should be reused in `lattice-profile` instead of inventing a new file shape.
- `LatticeDir::discover_with_global` already encodes project-local-over-global behavior; `lattice-profile` should preserve that behavior without retaining orchestrator config.
- `RuntimeSecurityConfig` and `build_runtime_security` already define the only current sandbox modes: `project`, `strict`, and `permissive`.
- `ToolBundle` already defines the current bundle names and tool expansion: `core`, `workspace`, `filesystem`, `editing`, `shell`, `planning`, and `web`.

### Established Patterns
- User-facing config and profile files are TOML with serde parsing and typed validation errors.
- Lower crates should not depend on upper crates. `lattice-profile` should sit below runtime/CLI and remain independent of runtime assembly.
- Runtime owns application assembly and security/tool wiring. Profile lowering should remain pure data conversion and defer environment-dependent checks.
- CLI uses `anyhow` for user-facing errors; lower crates should expose typed errors that CLI/runtime can render clearly.

### Integration Points
- Add `crates/lattice-profile` to the workspace and migrate profile schema/loading/lowering there.
- Update CLI run routing to accept/use a generic `--agent` or equivalent selection through the new profile loader.
- Update runtime APIs to consume the new pure profile-derived spec without importing `lattice_orchestrator` types.
- Update `lattice-tools` capability selection to remove presets and support explicit bundle plus enabled-tool inputs.
- Remove or rewrite tests that assert `ToolPreset::Coding`, `AgentPreset::Coding`, `code_agent`, or old `[capabilities]` behavior.

</code_context>

<specifics>
## Specific Ideas

- Keep the user-facing path names `agents` and `agent.toml` because that semantic is clearer than introducing `profiles`/`profile.toml`.
- The default generated prompt should explicitly tell the user to edit `.lattice/agents/default/agent.toml` and customize the role, project instructions, tools, and security settings.
- Prefer a small crate boundary now to avoid growing `lattice-runtime` or `lattice-agent` into a profile/config/loading/validation dumping ground.
- Be aggressive about deleting presets now; bundle names can keep their current scope for this phase.

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

*Phase: 1-Profile Contract Extraction*
*Context gathered: 2026-05-14*
