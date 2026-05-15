# Phase 03 - Pattern Map

## Purpose

Map Phase 03 changes to existing source patterns so execution can reuse local conventions instead of inventing new CLI/TUI architecture.

## Profile Runtime Path

| Target | Existing Analog | Pattern To Reuse |
|--------|-----------------|------------------|
| `lattice run --agent` profile resolution | `crates/lattice-profile/src/discovery.rs` | `ProfileSource::{Default, Name, Path}` plus `DiscoveryPaths { project_root, global_agents_dir }` is the source of truth. Do not reimplement lookup in command files. |
| CLI to runtime session prep | `crates/lattice-cli/src/profile_runtime.rs` | Central helper owns `resolve_agent_profile`, `lower_profile_to_spec`, source fingerprinting, runtime security construction, and `Runtime::prepare_agent_session`. Extend this helper for model overrides and default creation rather than duplicating in `run.rs` or TUI. |
| Runtime turn submission | `crates/lattice-cli/src/commands/run.rs` and `crates/lattice-cli/src/frontend/tui/agent_runner.rs` | Drain `SubmittedTurn.events`, await `SubmittedTurn.completion`, then gate completion status. Preserve this shape when replacing generated profiles with real profiles. |
| Session snapshot drift | `crates/lattice-runtime/src/turn.rs` and `crates/lattice-session/src/turn_tx.rs` | Runtime already returns `TurnCompletion.profile_drift_warning`; CLI/TUI should surface it, not mutate snapshots or add rebind behavior. |

## CLI Command Surface

| Target | Existing Analog | Pattern To Reuse |
|--------|-----------------|------------------|
| Command enum edits | `crates/lattice-cli/src/main.rs` | Keep clap-derived enum definitions and dispatch in one file. Remove variants/fields and their match arms together. |
| Prompt loading | `load_run_prompt` in `main.rs` | Reuse the existing `PROMPT` or `--file` mutual exclusion helper for `run`; do not add a second prompt parser. |
| New generic agent creation | `crates/lattice-cli/src/commands/new_agent.rs` | This is already v1-profile based and can remain after copy updates. It should not point users at removed `lattice validate`. |
| Model/provider inventory | `coding_agent::authenticated_models` and `authenticated_providers` | Move these helpers into a neutral module so TUI `/model` and `/provider` remain without depending on `coding_agent.rs`. |

## TUI Runtime And State

| Target | Existing Analog | Pattern To Reuse |
|--------|-----------------|------------------|
| Runtime session cache | `frontend/tui/app.rs` `normal_session: Option<RuntimeAgentSession>` | Keep one neutral runtime session cache; remove plugin session maps instead of adding profile maps in Phase 03. |
| Stream identity | `frontend/tui/agent_runner.rs` | Continue filtering runtime events by `session_id` and `TurnId`. Do not reintroduce live handles. |
| Completion handling | `App::apply_runtime_completion` | This already updates model/provider, approvals, token count, drift warnings, and status. Preserve this path and add snapshot/agent label updates around it. |
| Slash commands | `frontend/tui/commands.rs` plus `state.rs::SLASH_COMMANDS` | Command execution and suggestions are separate; remove `/plugins` and `/plugin` from both. |
| Statusline projection | `frontend/tui/view_model.rs` and `widgets/statusline.rs` | Add `agent_label` and optional `snapshot_label` to `StatuslineView`; widgets should read the view model instead of peeking into `App`. |

## Tests And Guards

| Target | Existing Analog | Pattern To Reuse |
|--------|-----------------|------------------|
| Source guard | `crates/lattice-cli/tests/frontend_runtime_guards.rs` | Existing helper strips comments before scanning active source. Extend this guard for single-agent UX forbidden strings. |
| Runtime fixture | `crates/lattice-cli/src/test_support.rs::GenericRuntimeTurnFixture` | Use this profile-backed fixture for `run --agent` and renamed generic profile eval tests. |
| TUI render tests | `frontend/tui/ui.rs`, `view_model.rs`, and `app.rs` inline tests | Rewrite plugin statusline tests into agent/snapshot label tests. Keep queue, search, permissions, tool, and completion tests intact. |

## Files To Treat As Active Phase 03 Scope

- `crates/lattice-cli/src/main.rs`
- `crates/lattice-cli/src/lib.rs`
- `crates/lattice-cli/src/profile_runtime.rs`
- `crates/lattice-cli/src/commands/run.rs`
- `crates/lattice-cli/src/commands/debug.rs`
- `crates/lattice-cli/src/commands/mod.rs`
- `crates/lattice-cli/src/commands/new_agent.rs`
- `crates/lattice-cli/src/commands/orchestrator_status.rs`
- `crates/lattice-cli/src/commands/agent_inventory.rs`
- `crates/lattice-cli/src/commands/list_agents.rs`
- `crates/lattice-cli/src/commands/validate.rs`
- `crates/lattice-cli/src/coding_agent.rs`
- `crates/lattice-cli/src/plugins.rs`
- `crates/lattice-cli/src/test_support.rs`
- `crates/lattice-cli/tests/code_agent_eval.rs`
- `crates/lattice-cli/tests/frontend_runtime_guards.rs`
- `crates/lattice-cli/src/frontend/tui/mod.rs`
- `crates/lattice-cli/src/frontend/tui/agent_runner.rs`
- `crates/lattice-cli/src/frontend/tui/app.rs`
- `crates/lattice-cli/src/frontend/tui/commands.rs`
- `crates/lattice-cli/src/frontend/tui/interaction.rs`
- `crates/lattice-cli/src/frontend/tui/plugin_mode.rs`
- `crates/lattice-cli/src/frontend/tui/state.rs`
- `crates/lattice-cli/src/frontend/tui/ui.rs`
- `crates/lattice-cli/src/frontend/tui/view_model.rs`
- `crates/lattice-cli/src/frontend/tui/widgets/composer.rs`
- `crates/lattice-cli/src/frontend/tui/widgets/statusline.rs`
- `crates/lattice-cli/src/frontend/tui/widgets/welcome.rs`

## Non-Goals For Pattern Mapping

- Do not scan or fail Phase 03 plans because `crates/lattice-plugin`, `crates/lattice-plugins`, or `crates/lattice-orchestrator` still exist. Phase 04 owns workspace layer removal.
- Do not redesign ratatui layout. Replace plugin labels/commands with agent labels/commands inside the existing layout.
- Do not introduce multi-profile TUI state, `/agent`, profile switching, rebind, or fork UX.
