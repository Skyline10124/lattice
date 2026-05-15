---
phase: 03
slug: cli-tui-single-agent-ux
status: complete
created: 2026-05-15
---

# Phase 03 - Research: CLI/TUI Single-Agent UX

## Research Question

What do we need to know to plan Phase 03 well?

Phase 03 must turn the current CLI/TUI adapter state into one visible generic single-agent UX. Phase 02 already moved CLI/TUI runtime execution onto `RuntimeAgentSession` and `Runtime::submit_session_turn`, but it intentionally left model-driven temporary profiles, `lattice code`, pipeline/orchestrator command exposure, and TUI plugin UI surfaces for this phase.

## Current State

### Phase 02 Deliverables Now Available

- `crates/lattice-cli/src/profile_runtime.rs` centralizes profile discovery, lowering, source fingerprinting, runtime security, and `Runtime::prepare_agent_session`.
- `Runtime::submit_session_turn` is the active execution path for CLI/TUI turns. It persists status-bearing `TurnCompletion` results and exposes `profile_drift_warning`.
- `lattice-session` owns immutable `ProfileSnapshot` records. Existing sessions reuse the locked snapshot; when the current source fingerprint differs, `ProfileDrift.warning` is `"profile changed on disk; current session remains locked to snapshot <hash>"`.
- Phase 02 explicitly kept generated temporary profiles for `run`, `debug`, and `code` so Phase 03 can reshape UX without touching runtime internals.

### Active CLI Surfaces Still To Clean

- `crates/lattice-cli/src/main.rs` still imports `mod coding_agent`, exposes `Commands::Code`, exposes `Commands::Orchestrator`, and keeps `Run` flags for `--pipeline`, `--agents-dir`, `--plugins-dir`, and `--tools-dir`.
- `Commands::Run` does not yet have `--agent`. It still accepts model/provider and routes through `commands/run.rs`.
- `commands/run.rs` still calls `prepare_generated_profile_runtime_session` with `profile_name: "run"` and generated prompt/tool bundles. This is the second profile runner chain Phase 03 must remove.
- `coding_agent.rs` still owns coding-specific options/build/render helpers, final report rendering, task-state seed loading, authenticated model/provider helpers, and completion status gating.
- `tests/code_agent_eval.rs`, `test_support.rs`, and several inline tests still use `coding_agent` naming.
- `commands/agent_inventory.rs`, `list_agents.rs`, and `validate.rs` still tolerate legacy orchestrator profiles. Phase 03 does not need to delete all inventory commands unless they expose user-facing orchestrator/pipeline/plugin execution semantics, but guard coverage should identify any active legacy behavior that contradicts CLI-02.

### Active TUI Surfaces Still To Clean

- `frontend/tui/mod.rs` still accepts `plugin_dir`, initializes `app.plugin_registry`, and reports plugin-dir loading as unavailable.
- `frontend/tui/app.rs` stores `plugin_registry`, `plugin_sessions`, `prepared_submission`, and `RunPluginContext`-based submission flow. It can build plugin-named generated profiles such as `plugin:CodeReview`.
- `frontend/tui/commands.rs` still registers `/plugins` and `/plugin`, including plugin list, plugin menu, and prepared plugin prompt behavior.
- `frontend/tui/state.rs` still includes slash suggestions for `/plugins` and `/plugin`.
- `frontend/tui/view_model.rs` still projects `plugin_label` as either `plugin:<name>` or `plugins:<count>`.
- `widgets/welcome.rs`, `widgets/composer.rs`, `widgets/statusline.rs`, `ui.rs`, and `interaction.rs` still surface plugin labels, plugin menu selection, or plugin shortcut text.
- `frontend/tui/plugin_mode.rs` and `crates/lattice-cli/src/plugins.rs` still compile active plugin helper code. Phase 03 can delete these from the TUI path; Phase 04 owns workspace crate deletion.

## Implementation Strategy

### 1. Make `run --agent` The Main Profile Runtime Path

Plan should replace generated `"run"` profile submission with:

```text
optional --agent value
  -> ProfileSource::Default | ProfileSource::Name | ProfileSource::Path
  -> prepare_profile_runtime_session
  -> Runtime::submit_session_turn
```

Concrete planner requirements:

- Add `--agent <name|path>` to `Commands::Run`.
- Omitted `--agent` means `ProfileSource::Default`.
- Name lookup must use project-local `.lattice/agents/<name>/agent.toml` before `~/.lattice/agents/<name>/agent.toml`.
- Explicit paths must load that path directly.
- `--model` and `--provider` remain overrides, but they must not create a temporary profile or become alternate prompt/tool/security semantics.
- `quick` mode can remain a no-tool pipe-friendly path because it is not the main agent runtime chain.

The tricky part is missing default creation:

- `lattice-profile` deliberately returns `ProfileError::MissingDefault { dir, template }` and does not write files.
- CLI must detect interactive terminal before writing `.lattice/agents/default/agent.toml`.
- Interactive missing-default flow should prompt, create the project-local default profile from `lattice_profile::default_agent_template`, print an edit hint, and continue the current run using the generated profile.
- Non-interactive missing-default flow must fail clearly and must not create files.

### 2. Preserve Session Snapshot Semantics In CLI Warnings

`Runtime::submit_session_turn` already opens persistent turns through the session store and returns `profile_drift_warning` in `TurnCompletion`.

Plan should make CLI behavior explicit:

- If a restored session has a locked snapshot, runtime uses the locked `opened.profile_snapshot.canonical_spec`.
- CLI should print `completion.profile_drift_warning` to stderr for `run` when present.
- If `--agent other` is passed with `--continue` or `--session <id>` and the restored session is already snapshot-bound to a different profile, behavior should remain runtime-locked. The UX requirement is a warning, not a runtime rebind.
- Planner should avoid any task that mutates existing `ProfileSnapshot` or adds rebind/fork behavior.

### 3. Delete `code` As A Command And Neutralize Reusable Helpers

`coding_agent.rs` still contains reusable pieces:

- `require_successful_runtime_completion`
- `load_latest_manifest_metadata`
- `authenticated_models`
- `authenticated_providers`
- runtime event renderer / final report rendering used by tests

Plan should first move reusable pieces into neutral modules, then delete `coding_agent.rs`.

Recommended split:

- `crates/lattice-cli/src/runtime_turn.rs` or `turn_render.rs`: completion status gating, event renderer, JSON/non-JSON completion rendering for generic runtime turns.
- `crates/lattice-cli/src/model_inventory.rs`: authenticated model/provider helpers used by TUI `/model` and `/provider`.
- `crates/lattice-cli/src/session_seed.rs`: manifest metadata to `TaskState` seed loading.

After helper migration:

- Remove `mod coding_agent` from `main.rs` and `lib.rs`.
- Remove `Commands::Code` and its dispatch.
- Delete `crates/lattice-cli/src/coding_agent.rs`.
- Rename `tests/code_agent_eval.rs` or replace it with generic profile runner tests.
- Rename `CodingAgentRuntimeTurnFixture` and `run_coding_agent_runtime_turn` in `test_support.rs` to generic profile names.

Do not replace `code` with a built-in alias. `lattice run --agent code "..."` works only when a normal `code` profile exists.

### 4. Remove Pipeline, Orchestrator, Plugin Execution From CLI User Surface

Phase 03 should remove active user-facing command flags and dispatch, not merely leave unavailable stubs:

- Delete `Commands::Orchestrator` and its `orchestrator_status::run` dispatch.
- Remove `Commands::Run.pipeline`, `agents_dir`, `plugins_dir`, and `tools_dir`.
- Delete `run::run_pipeline` after the CLI enum no longer references it.
- Remove or isolate `commands/orchestrator_status.rs` from the active module tree if no other command uses it.
- Keep profile management commands only if they are generic v1 profile commands. Any legacy orchestrator registry behavior in `agent_inventory.rs` and `validate.rs` should be planned either for Phase 03 cleanup if user-facing, or explicitly deferred to Phase 04 if it is crate/workspace removal. Current CLI-02 specifically mentions agent registry validation/listing, so planning should treat old registry validation/listing as active UX cleanup, not Phase 04-only dead code.

### 5. Convert TUI To One Active Agent/Profile Surface

Plan should replace generated `"coding"` and `plugin:*` profile usage with one selected profile/runtime session.

Required target:

- Add `--agent <name|path>` to `Commands::Tui` and `TuiOptions`.
- Omitted `--agent` means `ProfileSource::Default`.
- TUI starts with one profile label, e.g. `agent:default` or `agent:<name>`.
- TUI has exactly one active `RuntimeAgentSession` cache for the selected profile/session in Phase 03. No `/agent`, no switching UI, no `profile_sessions` map.
- Model/provider display, prompt streaming, tool details, approvals, permissions, session restore, search, transcript, and queue behavior must remain.
- TUI should show `completion.profile_drift_warning` as a system message, which it already mostly does in `apply_runtime_completion`.

Files likely modified:

- `frontend/tui/mod.rs`: remove `plugin_dir`; accept selected `ProfileSource` or a neutral `AgentSelection`; stop initializing plugin registry.
- `frontend/tui/app.rs`: remove `plugin_registry`, `plugin_sessions`, `prepared_submission`, `RunPluginContext`, `submit_prepared`, `prepare_plugin_prompt`, `plugin_*` helpers, and plugin-specific submission logic. Replace `normal_session` with a neutral `active_agent_session` or `runtime_session`.
- `frontend/tui/agent_runner.rs`: build sessions through `prepare_profile_runtime_session` instead of `prepare_generated_profile_runtime_session`. Keep generated-profile helper only if non-TUI code still needs it during the same plan wave, but final Phase 03 should leave TUI on real profile discovery.
- `frontend/tui/commands.rs`: delete `/plugins` and `/plugin` with no compatibility commands.
- `frontend/tui/state.rs`: remove `/plugins` and `/plugin` suggestions and remove `MenuKind::Plugin` if present.
- `frontend/tui/view_model.rs`: replace `plugin_label` with `agent_label`.
- `widgets/statusline.rs`, `widgets/welcome.rs`, `widgets/composer.rs`, `ui.rs`, `interaction.rs`: replace plugin display/shortcuts/menu handling with agent/profile label where needed.
- Delete `frontend/tui/plugin_mode.rs` if no active TUI code needs it.

### 6. Guard And Test Strategy

Phase 03 needs source-level boundary guards plus focused runtime tests.

Existing guard pattern:

- `crates/lattice-cli/tests/frontend_runtime_guards.rs` strips Rust comments and checks active source for forbidden strings.

Plan should migrate this into a single-agent UX guard with precise scopes:

- Active CLI/TUI sources must not contain `Commands::Code`, `coding_agent`, `code_agent`, `AgentPreset::Coding`, active `pipeline` command fields, active `orchestrator status` command exposure, `plugins_dir`, `/plugins`, `/plugin`, `PluginRegistry`, `plugin_sessions`, `plugin_label`, or `RuntimePluginOverlay`.
- Guard must not scan `crates/lattice-plugin`, `crates/lattice-plugins`, or `crates/lattice-orchestrator` because Phase 04 owns workspace layer removal.
- Guard should tolerate historical `.planning` docs and test names only if tests intentionally assert absence. Prefer active-source helpers to avoid comment-only false positives.

Focused tests to plan:

- `run_agent_profile_selection_uses_project_local_before_global`
- `run_agent_explicit_path_loads_profile`
- `run_missing_default_noninteractive_fails_without_writing`
- `run_missing_default_interactive_creates_project_local_default_and_continues` if there is an injectable confirmation/prompt seam
- `run_restored_session_warns_when_profile_source_changed`
- `tui_view_model_shows_agent_label`
- `tui_slash_registry_has_no_plugin_commands`
- `tui_status_includes_agent_and_snapshot_hash`
- `frontend_runtime_guards` updated to single-agent UX guard

Targeted command sequence:

```sh
cargo test -p lattice-cli profile_runtime
cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture
cargo test -p lattice-cli run_agent -- --nocapture
cargo test -p lattice-cli tui_agent -- --nocapture
cargo check -p lattice-cli --bin lattice
cargo fmt --all --check
git diff --check
```

## Validation Architecture

Phase 03 validation should be guard-heavy and source-backed because the risk is stale user-facing surfaces remaining after compile still passes.

Required validation dimensions:

1. Profile runner identity: `run` and TUI must use `ProfileSource` and `prepare_profile_runtime_session`, not generated temporary profiles.
2. Removed command surface: CLI enum/dispatch must not expose `Code`, pipeline, orchestrator status, plugin loading, or plugin execution.
3. TUI plugin removal: slash commands, suggestions, statusline labels, menu handling, prepared plugin prompts, and plugin sessions must be absent from active TUI code.
4. Session snapshot UX: `profile_drift_warning` must be surfaced and restored sessions must continue locked to the snapshot.
5. Core TUI retention: model/provider display, prompt streaming, tool visibility, approvals, permissions, sessions, search, trace/transcript, and queue behavior must remain covered by existing or updated tests.

## Risks And Mitigations

| Risk | Why It Matters | Mitigation |
|------|----------------|------------|
| Removing `coding_agent.rs` breaks shared renderer/test helpers | The file currently contains neutral runtime helpers mixed with coding UX. | Plan helper extraction before command deletion. |
| TUI plugin removal accidentally removes tool visibility or approvals | Plugin UI and tool UI are adjacent in TUI state. | Keep `ActivityState`, tool display, approval task-state propagation, and permissions code out of plugin cleanup unless directly needed. |
| `--agent` with restored session appears to switch profiles | Runtime correctly locks snapshots, but user needs a visible warning. | Surface `profile_drift_warning` in CLI stderr and TUI system messages; add tests. |
| Guard false positives block Phase 04 leftovers | Plugin/orchestrator crates still exist by design. | Scope guards to active `lattice-cli` files and CLI/TUI tests only. |
| Interactive default creation is hard to test | Direct TTY reads can be brittle. | Introduce a small injectable confirmation helper owned by CLI; unit-test noninteractive and confirmation branches without opening an editor. |

## Recommended Plan Split

1. CLI profile runner and default-profile creation for `lattice run --agent`.
2. Neutral helper extraction and `lattice code` command/module/test removal.
3. CLI command surface cleanup for pipeline/orchestrator/plugin/legacy registry UX.
4. TUI selected-profile startup and single runtime session state.
5. TUI plugin command/status/menu removal and agent label/status UX.
6. Single-agent guard/tests and final focused verification.

## RESEARCH COMPLETE
