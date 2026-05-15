---
phase: 03-cli-tui-single-agent-ux
verified: 2026-05-15T08:30:09Z
status: passed
score: 5/5 requirements verified
gaps_found: 2
gaps_closed: 2
gaps_remaining: []
regressions: []
---

# Phase 03: CLI/TUI Single-Agent UX - Verification Report

**Phase Goal:** Make the user-facing CLI and TUI reflect one generic agent path.
**Verified:** 2026-05-15
**Status:** passed

## Goal Achievement

| # | Success criterion | Status | Evidence |
|---|-------------------|--------|----------|
| 1 | CLI `run` and TUI prompt submission use the same generic runtime spec/profile path. | VERIFIED | `lattice run --agent` is wired through `ProfileSource`, `ProfileRuntimeSessionRequest`, `prepare_profile_runtime_session`, and `submit_session_turn`. TUI `AgentBuildRequest` uses selected `ProfileSource` plus one `RuntimeAgentSession`; guard requires `ProfileSource`, `prepare_profile_runtime_session`, `RuntimeAgentSession`, and `submit_session_turn`. |
| 2 | `code` is removed or becomes ordinary profile behavior with no separate `coding_agent.rs`. | VERIFIED | `crates/lattice-cli/src/coding_agent.rs` is deleted. `Commands::Code`, `coding_agent`, `code_agent`, and `AgentPreset::Coding` are absent from active CLI/TUI source and tests outside the guard deny-list. `profile_runtime_eval` proves `run --agent code` is ordinary profile-backed behavior. |
| 3 | CLI command tree no longer exposes pipeline, orchestrator status, plugin loading/execution, or legacy registry UX. | VERIFIED | `Commands::Orchestrator`, `run_pipeline`, pipeline flags, plugin/tool directory flags, and `orchestrator_status.rs` are removed from active CLI code. Agent inventory/list/validate use strict v1 profile loading only. |
| 4 | TUI removes plugin commands, labels, handles, overlays, and menus. | VERIFIED | `/plugins`, `/plugin`, `PluginRegistry`, `plugin_sessions`, `plugin_label`, `PreparedSubmission`, `RunPluginContext`, `RuntimePluginOverlay`, and `MenuKind::Plugin` are absent from active TUI code. Statusline/welcome/composer render `agent:<name|path>` and optional `snapshot:<hash8>`. |
| 5 | TUI still supports core single-agent interactions. | VERIFIED | Full `lattice-cli` test suite covers model/provider display, prompt submission, runtime streaming, stale event filtering, tool details, approvals/permissions, queueing, search, transcript, session restore, and statusline rendering. |

## Requirements Coverage

| Requirement | Status | Evidence |
|-------------|--------|----------|
| CLI-01 | SATISFIED | `cargo test -p lattice-cli profile_source -- --nocapture`, `cargo test -p lattice-cli profile_runtime_model_override -- --nocapture`, `cargo test -p lattice-cli tui_runtime_uses_selected_profile_source -- --nocapture`, and full `cargo test -p lattice-cli` passed. |
| CLI-02 | SATISFIED | Active source grep for pipeline/orchestrator/plugin command surfaces returned no matches outside guard tests; `agent_inventory` tests passed. Broad filters `cli_command_surface` and `validate_rejects_legacy_orchestrator_profile` currently match zero tests, so the source guard and v1 inventory tests are the authoritative coverage. |
| CLI-03 | SATISFIED | `coding_agent.rs` is deleted, `code_agent` terminology is absent from active tests, and `cargo test -p lattice-cli --test profile_runtime_eval -- --nocapture` passed. |
| CLI-04 | SATISFIED | `slash_registry_has_no_plugin_commands`, `slash_plugin_is_not_handled`, `agent_statusline`, `welcome_does_not_render_plugin_count`, and `frontend_runtime_guards` passed. |
| CLI-05 | SATISFIED | Full `cargo test -p lattice-cli` passed, including TUI queue/search/transcript/tool/approval/session/runtime event tests. |

## Verification Commands

| Command | Result |
|---------|--------|
| `cargo test -p lattice-cli profile_source -- --nocapture` | PASS: 4 matched tests passed. |
| `cargo test -p lattice-cli missing_default_creation -- --nocapture` | PASS: 2 matched tests passed. |
| `cargo test -p lattice-cli profile_runtime_model_override -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli profile_drift -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli run_completion_json_includes_profile_warning -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli runtime_completion -- --nocapture` | PASS: 3 matched tests passed. |
| `cargo test -p lattice-cli load_latest_manifest_metadata -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli --test profile_runtime_eval -- --nocapture` | PASS: 3 integration tests passed. |
| `cargo test -p lattice-cli agent_inventory -- --nocapture` | PASS: 3 matched tests passed. |
| `cargo test -p lattice-cli tui_runtime_uses_selected_profile_source -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli view_model_exposes_agent_label -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli tui_completion_sets_snapshot_label -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli slash_registry_has_no_plugin_commands -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli slash_plugin_is_not_handled -- --nocapture` | PASS after gap fix: 1 matched test passed. |
| `cargo test -p lattice-cli agent_statusline -- --nocapture` | PASS: 2 matched tests passed. |
| `cargo test -p lattice-cli welcome_does_not_render_plugin_count -- --nocapture` | PASS: 1 matched test passed. |
| `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` | PASS: 6 guard tests passed. |
| `cargo test -p lattice-cli` | PASS: lib 198, bin 202, guard 6, profile runtime eval 3, doctests 0. |
| `cargo check -p lattice-cli --bin lattice` | PASS. |
| `cargo fmt --all --check` | PASS. |
| `git diff --check` | PASS. |
| Active boundary grep over `crates/lattice-cli/src` and tests excluding `frontend_runtime_guards.rs` | PASS: no matches. |

## Filters With No Matching Tests

| Filter | Result | Replacement coverage |
|--------|--------|----------------------|
| `cargo test -p lattice-cli cli_command_surface -- --nocapture` | 0 matched tests. | `frontend_runtime_guards` deny-list plus active boundary grep cover removed command surfaces. |
| `cargo test -p lattice-cli validate_rejects_legacy_orchestrator_profile -- --nocapture` | 0 matched tests. | `agent_inventory_loads_only_v1_profiles` plus source grep over validate/list/inventory cover strict v1-only behavior. |
| `cargo test -p lattice-cli tui_agent -- --nocapture` | 0 matched tests. | Targeted TUI runtime/status/plugin-removal tests plus full `cargo test -p lattice-cli` cover the active behavior. |

## Gaps Found And Closed

| Gap | Impact | Fix | Verification |
|-----|--------|-----|--------------|
| Removed `/plugin` was absent from slash suggestions but still needed safe unknown-command handling. | Risked either retaining plugin compatibility behavior or accidentally forwarding unknown slash commands to the model. | Kept `/plugin` out of the command registry/suggestions, while unknown slash commands are consumed with `Unknown command: <name>. Type /help for shortcuts.` and are not submitted as prompts. | `cargo test -p lattice-cli slash_plugin_is_not_handled -- --nocapture` and full `cargo test -p lattice-cli` passed. |
| `runtime_turn_event_stale_runtime_event_is_ignored` used the same runtime `TurnId` for expected and event IDs, so it did not actually verify stale-event filtering. | Test was a false negative and did not prove stale runtime events were ignored. | Updated the test to send a different `TurnId` for the stale event. | `cargo test -p lattice-cli runtime_turn_event_stale_runtime_event_is_ignored -- --nocapture` and full `cargo test -p lattice-cli` passed. |

## Residual Risk

- Manual interactive missing-default prompt behavior was not exercised through a real terminal. Unit coverage verifies the write/no-overwrite helpers and noninteractive message path; terminal prompting remains a manual-only check from `03-VALIDATION.md`.
- TUI first-screen visual placement was not manually inspected in a terminal. Rendering tests verify agent/snapshot labels and absence of plugin count.
- Phase 04 workspace crate removal is intentionally untouched; plugin/orchestrator crates still existing in the workspace is not a Phase 03 failure.

## Sign-Off

Phase 03 passes verification. Active CLI/TUI surfaces now present one generic profile-backed agent path, removed code/plugin/orchestrator/pipeline UX does not appear in active source, and the full `lattice-cli` test suite passes after verification fixes.
