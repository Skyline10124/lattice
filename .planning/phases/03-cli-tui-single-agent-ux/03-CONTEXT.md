# Phase 3: CLI/TUI Single-Agent UX - Context

**Gathered:** 2026-05-15
**Status:** Ready for planning

<domain>
## Phase Boundary

This phase makes the user-facing CLI and TUI reflect one generic single-agent path. The primary CLI entry point becomes a real profile-backed runtime path: `lattice run --agent <name|path> "<prompt>"`, with omitted `--agent` resolving to the `default` profile. The TUI uses the same generic profile/runtime/session path and removes plugin, pipeline, orchestrator, and specialized coding-agent UX surfaces.

This phase does not delete `lattice-plugin`, `lattice-plugins`, or `lattice-orchestrator` workspace crates; that is Phase 4 scope. It also does not add multi-profile switching or profile rebind/fork UX inside the TUI. Phase 03 is the CLI/TUI adapter cleanup that makes the active user surface line up with the Phase 1 profile contract and Phase 2 session-bound runtime path.

</domain>

<decisions>
## Implementation Decisions

### CLI Profile Runner

- **D-01:** `lattice run` is the only main interactive/generic agent run chain. It must route through a real `lattice-profile` profile and runtime session; do not keep the current generated temporary `"run"` profile path as an alternate main path.
- **D-02:** `lattice run --agent <name|path>` is the profile selection UX. `--agent foo` resolves `.lattice/agents/foo/agent.toml` before `~/.lattice/agents/foo/agent.toml`; `--agent ./path/to/agent.toml` loads that explicit file.
- **D-03:** Omitting `--agent` means `ProfileSource::Default`: look for `.lattice/agents/default/agent.toml`, then `~/.lattice/agents/default/agent.toml`.
- **D-04:** If `default` is missing in an interactive terminal, CLI prompts before creating project-local `.lattice/agents/default/agent.toml` from `lattice_profile::default_agent_template`.
- **D-05:** If `default` is missing in a non-interactive environment, CLI fails clearly and does not create files.
- **D-06:** When an interactive user confirms default creation, create the template, print a clear note telling the user to edit `.lattice/agents/default/agent.toml`, do not open an editor, and continue the current run using the generated template.
- **D-07:** `--model` and `--provider` remain runtime overrides for the selected profile, but the selected profile remains the source of prompt/tools/security/context/behavior semantics unless a lower layer already defines override behavior.
- **D-08:** Existing explicit `quick` mode can remain the no-tool pipe-friendly path, but it is not the main agent runtime path and must not recreate a second profile runner chain.

### Code Command Removal

- **D-09:** Delete the `lattice code` subcommand rather than preserving it as an alias.
- **D-10:** `code` has no special runtime meaning. `lattice run --agent code "..."` only works if a normal `code` profile exists through the standard profile discovery path.
- **D-11:** Do not provide a built-in coding preset, hidden `code` default profile, special coding template, or fallback tool bundle. Coding behavior is ordinary profile prompt plus tool selection.
- **D-12:** Delete `crates/lattice-cli/src/coding_agent.rs`. Move still-useful helper behavior into neutral modules named around generic runtime turns, model inventory, manifest task-state loading, or rendering.
- **D-13:** Rename tests, fixtures, and guard references away from `coding_agent`, `code_agent`, and coding-preset terminology. Preserve only coverage that proves the generic profile runner uses runtime turn completion.
- **D-14:** Delete old `code` command tests. If coverage is still valuable, migrate it to profile/runtime tests using a temporary v1 `agent.toml`; do not turn `code` into an official special profile in tests.

### TUI Single-Agent Surface

- **D-15:** Delete `/plugins` and `/plugin` from the TUI command registry and suggestions. Do not keep replacement plugin commands and do not keep "plugin removed" compatibility commands.
- **D-16:** Remove plugin registry initialization, plugin directory handling, plugin prepared submissions, plugin menu items, plugin sessions, plugin statusline labels, plugin prompt rewriting, and plugin-specific TUI tests from the active TUI path.
- **D-17:** Replace the statusline/welcome/composer plugin display with a current agent/profile label, such as `agent:default` or `agent:<name>`.
- **D-18:** TUI runs one active profile/session in Phase 03. It may be started with a selected profile through the CLI entry path, but the TUI must not introduce `/agent`, profile switching, or a `profile_sessions` map in this phase.
- **D-19:** TUI must continue to support the core single-agent interactions required by the roadmap: model/provider display, prompt streaming, tool visibility/detail, approvals/permissions, sessions, search, trace/transcript, and queue behavior.

### Session-Bound Profile Snapshot UX

- **D-20:** Restoring a session uses the session-bound profile snapshot by default. Current profile files must not silently alter an existing session.
- **D-21:** If the source profile file exists but its fingerprint/hash differs from the session-bound snapshot, surface a non-blocking warning/debug message and continue using the snapshot.
- **D-22:** `--agent` only affects new sessions. If the user passes `--agent other` together with `--continue` or `--session <id>` and the selected session is already bound to a different snapshot, ignore `--agent`, warn, and continue with the session-bound snapshot.
- **D-23:** Do not implement profile rebind, fork, or in-place profile switching in Phase 03.
- **D-24:** CLI warnings for profile drift or ignored `--agent` go to stderr. TUI warnings are system messages. Runtime execution should continue when the session snapshot is usable.
- **D-25:** `/status` should show the current agent/profile name and snapshot hash so users can understand which immutable profile snapshot is active.

### Verification And Cleanup Boundary

- **D-26:** Phase 03 cleans the active CLI/TUI surface only. Do not delete `crates/lattice-plugin`, `crates/lattice-plugins`, or `crates/lattice-orchestrator`; Phase 4 owns workspace layer removal.
- **D-27:** Add or extend hard guard tests over active `crates/lattice-cli` source and CLI tests to fail on `Commands::Code`, `coding_agent`, `code_agent`, active `pipeline` or `orchestrator status` command exposure, `plugins_dir`, `/plugins`, `/plugin`, `PluginRegistry`, `plugin_sessions`, `plugin_label`, and `RuntimePluginOverlay`.
- **D-28:** Phase 03 guards should not fail merely because obsolete plugin/orchestrator crates still exist in the workspace. They should fail only when CLI/TUI active paths still import or expose those concepts.
- **D-29:** Migrate `frontend_runtime_guards.rs` into a single-agent UX guard and add or migrate profile runner tests that prove `lattice run --agent <name|path>` uses the generic runtime turn path.

### the agent's Discretion

- The planner may choose exact module names for neutralized helpers, but the names must not include `coding_agent` or `code_agent`.
- The planner may decide whether the default-creation prompt lives near CLI argument handling, profile-runtime preparation, or a small prompt helper, as long as `lattice-profile` remains side-effect-free and CLI owns the file write after confirmation.
- The planner may choose the exact warning wording, but messages must clearly distinguish "using session-bound snapshot" from "using current profile file".

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project and Phase Scope

- `.planning/PROJECT.md` — v1.0 single-agent core framing and explicit removal of immature extension layers from the active user/runtime path.
- `.planning/REQUIREMENTS.md` — CLI-01 through CLI-05 plus cross-phase traceability.
- `.planning/ROADMAP.md` — Phase 3 goal, success criteria, and boundary with Phase 4 workspace layer removal.
- `.planning/STATE.md` — current project position and accumulated Phase 1/2 decisions.

### Prior Phase Decisions

- `.planning/phases/01-profile-contract-extraction/01-CONTEXT.md` — profile file contract, default profile discovery/creation decisions, deletion of `lattice code`, and no special coding-agent semantics.
- `.planning/phases/01-profile-contract-extraction/01-DISCUSSION-LOG.md` — audit-only evidence that missing default generation requires interactive confirmation and project-local destination.
- `.planning/phases/01-profile-contract-extraction/01-RESEARCH.md` — verified profile discovery flow and CLI-owned default creation boundary.
- `.planning/phases/01-profile-contract-extraction/01-02-PLAN.md` — no-write `MissingDefault` contract in `lattice-profile`, later CLI prompting in Phase 3.
- `.planning/phases/02-runtime-assembly-cutover/02-CONTEXT.md` — session-owned runtime execution, immutable profile snapshots, no silent profile switching, and deferred rebind/fork UX.

### Current Source Inputs

- `crates/lattice-profile/src/discovery.rs` — `ProfileSource::{Path, Name, Default}` lookup semantics and `MissingDefault` behavior.
- `crates/lattice-profile/src/defaults.rs` — strict v1 generated default profile template text.
- `crates/lattice-profile/src/error.rs` — `ProfileError::MissingDefault { dir, template }` contract.
- `crates/lattice-cli/src/profile_runtime.rs` — existing profile-to-runtime session preparation helpers and generated-profile compatibility path to replace for `run`.
- `crates/lattice-cli/src/main.rs` — current command tree, `Run`, `Code`, `Tui`, orchestrator status, pipeline, and plugin-dir arguments to remove or reshape.
- `crates/lattice-cli/src/commands/run.rs` — current generated `"run"` profile path, pipeline stub, runtime turn submission, and completion rendering.
- `crates/lattice-cli/src/coding_agent.rs` — specialized coding-agent module to delete after moving generic helpers.
- `crates/lattice-cli/src/frontend/tui/mod.rs` — current TUI startup, plugin registry setup, plugin-dir handling, and options shape.
- `crates/lattice-cli/src/frontend/tui/app.rs` — current plugin sessions, prepared plugin submission, status helpers, and single-turn runtime session state.
- `crates/lattice-cli/src/frontend/tui/commands.rs` — slash command registry with `/plugins` and `/plugin`.
- `crates/lattice-cli/src/frontend/tui/state.rs` — slash suggestions and menu kind definitions.
- `crates/lattice-cli/src/frontend/tui/view_model.rs` — current plugin label statusline projection to replace with agent/profile label.
- `crates/lattice-cli/src/frontend/tui/widgets/welcome.rs` — welcome text that currently reports plugin state.
- `crates/lattice-cli/src/frontend/tui/widgets/composer.rs` — composer help row that currently advertises plugins.
- `crates/lattice-cli/src/frontend/tui/agent_runner.rs` — current generated-profile session build path and plugin-session tests to neutralize.
- `crates/lattice-cli/tests/frontend_runtime_guards.rs` — existing frontend runtime guard to extend into single-agent UX guard.
- `crates/lattice-cli/tests/code_agent_eval.rs` — old code-agent tests to delete or migrate to generic profile runner coverage.

### Codebase Maps

- `.planning/codebase/INTEGRATIONS.md` — current CLI/TUI, profile/plugin, session, sandbox, and runtime integration surfaces.
- `.planning/codebase/STRUCTURE.md` — crate/file ownership map for `lattice-cli`, `lattice-profile`, `lattice-runtime`, and TUI files.
- `.planning/codebase/CONCERNS.md` — TUI state-machine fragility and security/session persistence concerns relevant to scoped changes.
- `.planning/codebase/ARCHITECTURE.md` — layered runtime/CLI architecture and legacy plugin/orchestrator coupling to keep out of active CLI/TUI paths.

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets

- `lattice_profile::ProfileSource`, `DiscoveryPaths`, `resolve_agent_profile`, and `lower_profile_to_spec` already provide the real profile loading path needed by `run` and TUI.
- `crates/lattice-cli/src/profile_runtime.rs` already bridges a loaded profile into `Runtime::prepare_agent_session`; it should become the main CLI/TUI preparation path rather than a dead helper beside generated profiles.
- `ProfileError::MissingDefault { dir, template }` already carries enough information for CLI to prompt, create the project-local default profile, and then retry the run.
- `lattice_profile::default_agent_template()` already emits a strict v1 profile with comments telling the user to edit role, prompt, tools, and security settings.
- `RuntimeAgentSession`, `TurnRequest`, `RuntimeTurnEvent`, `TurnCompletion`, and `submit_session_turn` are already the runtime-owned frontend path and should remain the TUI/CLI execution surface.

### Established Patterns

- Profile loading/discovery is read-only and side-effect-free in `lattice-profile`; CLI owns user interaction and file writes.
- Runtime/session own execution and persistence. CLI/TUI adapt user input into runtime requests and should not re-own agent state.
- Phase 2 made session-bound profile snapshots the security boundary. CLI/TUI warnings should expose drift without mutating session behavior.
- Existing guard tests use source-string checks to prevent frontends from reintroducing live-agent ownership or obsolete runtime handle concepts; Phase 03 can extend this pattern for single-agent UX cleanup.

### Integration Points

- Add `--agent` to `Commands::Run` and `Commands::Tui` and thread it into profile-runtime preparation for new sessions.
- Remove `Commands::Code`, `orchestrator_status` dispatch, `pipeline`, `agents_dir`, `plugins_dir`, and `tools_dir` user-facing run arguments from active CLI command paths.
- Replace generated profile creation in `run.rs` and TUI `agent_runner.rs` with real `ProfileSource` resolution, while preserving model/provider override and runtime security construction.
- Move generic helpers out of `coding_agent.rs` before deleting the file: successful completion gating, authenticated model/provider listing, task-state manifest seed loading, and streaming/rendering if still needed.
- Delete TUI plugin modules and state fields or isolate them out of active compilation if Phase 4 still needs crate-level code elsewhere. The active TUI UX must not initialize plugin registries or expose plugin commands.

</code_context>

<specifics>
## Specific Ideas

- Treat `lattice run` as the same conceptual path users see in TUI: choose one profile, bind or restore one session snapshot, stream one generic agent turn.
- Creation of `.lattice/agents/default/agent.toml` should be a small prompt-and-write flow: ask, write, tell the user to edit later, continue current run. It must not open an editor.
- `code` should disappear as an implementation concept. If a user wants a coding agent, they create a normal `code` profile.
- Statusline visibility should move from plugin count/name to agent/profile identity, because profile identity is now the important runtime UX signal.

</specifics>

<deferred>
## Deferred Ideas

- Profile switching, rebind, fork, and multi-profile session management are future work after the immutable profile snapshot model is fully in place.
- Workspace deletion of `lattice-plugin`, `lattice-plugins`, and `lattice-orchestrator` belongs to Phase 4.
- Any future coding-oriented template can be considered later as a normal `lattice new agent` template feature, not as a special runtime path.

</deferred>

---

*Phase: 3-CLI/TUI Single-Agent UX*
*Context gathered: 2026-05-15*
