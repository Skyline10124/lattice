---
phase: 03
slug: cli-tui-single-agent-ux
status: executed
nyquist_compliant: true
wave_0_complete: true
created: 2026-05-15
---

# Phase 03 - Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Rust built-in `cargo test` with inline crate tests and crate integration tests |
| **Config file** | Root `Cargo.toml`, crate `Cargo.toml` files, and CLI/TUI source guards |
| **Quick run command** | `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` |
| **Full suite command** | `cargo test -p lattice-cli run_agent -- --nocapture && cargo test -p lattice-cli tui_agent -- --nocapture && cargo check -p lattice-cli --bin lattice` |
| **Estimated runtime** | ~180-360 seconds for targeted CLI/TUI checks |

---

## Sampling Rate

- **After every task commit:** Run the narrow test or guard for the touched CLI/TUI subsystem.
- **After every plan wave:** Run `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` plus affected inline tests.
- **Before `$gsd-verify-work`:** Run `cargo check -p lattice-cli --bin lattice`, `cargo fmt --all --check`, and `git diff --check`.
- **Optional closure evidence:** Run `cargo test -p lattice-cli` if targeted tests are green and runtime is stable.
- **Max feedback latency:** 360 seconds for targeted CLI/TUI feedback.

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 03-01-01 | 01 | 1 | CLI-01 | T-03-01 / T-03-02 | `lattice run` resolves `ProfileSource::{Default, Name, Path}` through `lattice-profile` and does not generate a temporary `"run"` profile. | unit/integration | `cargo test -p lattice-cli profile_source -- --nocapture` | Existing | green |
| 03-01-02 | 01 | 1 | CLI-01 | T-03-03 | Missing default creation prompts only in interactive mode, writes project-local `.lattice/agents/default/agent.toml` after confirmation, and noninteractive mode writes nothing. | unit | `cargo test -p lattice-cli missing_default_creation -- --nocapture` | Existing | green |
| 03-01-03 | 01 | 1 | CLI-01 / CLI-05 | T-03-04 | Restored sessions keep locked profile snapshots and surface drift warnings without rebinding. | integration | `cargo test -p lattice-cli profile_drift -- --nocapture` | Existing | green |
| 03-02-01 | 02 | 2 | CLI-03 | T-03-05 | `coding_agent.rs`, `Commands::Code`, `code_agent`, and coding-preset naming are absent from active CLI code and tests. | guard + compile | `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` | Existing | green |
| 03-03-01 | 03 | 3 | CLI-02 | T-03-06 | CLI command tree no longer exposes pipeline, orchestrator status, plugin loading, plugin execution, or legacy agent registry validation/listing UX. | guard + parse tests | `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` | Existing | green |
| 03-04-01 | 04 | 4 | CLI-01 / CLI-05 | T-03-01 / T-03-04 | TUI starts from selected/default profile, uses one runtime session cache, and continues model/provider/stream/tool/approval/session behavior. | unit/integration | `cargo test -p lattice-cli tui_runtime_uses_selected_profile_source -- --nocapture` | Existing | green |
| 03-05-01 | 05 | 5 | CLI-04 | T-03-07 | TUI active source has no `/plugins`, `/plugin`, `PluginRegistry`, `plugin_sessions`, `plugin_label`, prepared plugin prompts, or plugin menus. | guard + unit | `cargo test -p lattice-cli --test frontend_runtime_guards -- --nocapture` | Existing | green |
| 03-06-01 | 06 | 6 | CLI-01 / CLI-02 / CLI-03 / CLI-04 / CLI-05 | T-03-01..T-03-07 | Single-agent UX guard covers active CLI/TUI surfaces while excluding Phase 04 workspace crates. | guard + build | `cargo check -p lattice-cli --bin lattice` | Existing | green |

*Status values: pending, green, red, flaky.*

---

## Wave 0 Requirements

- [x] `crates/lattice-cli/tests/frontend_runtime_guards.rs` is migrated into a single-agent UX guard scoped to active `lattice-cli` files.
- [x] Tests can create strict v1 profile packages with project-local and global discovery paths.
- [x] CLI default creation logic has a test seam for interactive confirmation and noninteractive failure.
- [x] TUI view model has an `agent_label` or equivalent field testable without running the terminal.
- [x] Existing TUI tests for streaming, queueing, approvals, permissions, search, transcript, and tool output remain present or are renamed without behavior loss.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Real interactive missing-default prompt | CLI-01 | Unit tests should cover the decision seam, but actual terminal detection and stdin prompt are environment-sensitive. | Run `lattice run "hello"` in a clean temp project with no default profile; confirm it asks before writing, writes `.lattice/agents/default/agent.toml`, prints an edit hint, and continues. |
| TUI first-screen agent label | CLI-04 / CLI-05 | Most TUI rendering is unit-testable, but visual placement is easier to confirm manually. | Launch `lattice tui --agent default` with a default profile and confirm statusline/welcome show `agent:default` or equivalent, not plugin count. |

---

## Threat Model

| Threat ID | STRIDE | Surface | Mitigation | Verification |
|-----------|--------|---------|------------|--------------|
| T-03-01 | Spoofing / confusion | CLI `--agent` source selection | Use `lattice-profile::ProfileSource` and `DiscoveryPaths`; explicit path wins, named lookup checks project-local before global, omitted source means `Default`. | `cargo test -p lattice-cli run_agent -- --nocapture` |
| T-03-02 | Tampering | Temporary generated `"run"` profiles | Remove generated-profile path from the main `run` UX so prompt/tools/security semantics come from a real profile. | Source guard and run-agent tests |
| T-03-03 | Tampering | Missing default profile creation | Prompt before writing in interactive mode; fail without writing in noninteractive mode. | `cargo test -p lattice-cli missing_default -- --nocapture` |
| T-03-04 | Tampering / repudiation | Restored session with changed profile file | Runtime locked snapshot remains authoritative; CLI/TUI surface `profile_drift_warning` without rebind/fork. | `cargo test -p lattice-cli profile_drift -- --nocapture` |
| T-03-05 | Elevation of privilege / confusion | Specialized `code` command and coding module | Remove `Commands::Code` and `coding_agent.rs`; coding behavior is ordinary profile prompt/tools. | Single-agent UX guard |
| T-03-06 | Elevation of privilege / confusion | Pipeline/orchestrator/plugin CLI command surface | Delete active CLI flags/subcommands/dispatch for pipeline, orchestrator status, plugin loading, and plugin execution. | CLI command surface tests and guard |
| T-03-07 | Elevation of privilege / confusion | TUI plugin sessions and commands | Remove `/plugins`, `/plugin`, plugin registry/session/menu/statusline/prepared prompt flows from active TUI path. | Frontend guard and TUI unit tests |

---

## Boundary Grep

Run after Phase 03 cleanup:

```sh
rg -n "Commands::Code|coding_agent|code_agent|AgentPreset::Coding|pipeline|orchestrator_status|plugins_dir|/plugins|/plugin|PluginRegistry|plugin_sessions|plugin_label|RuntimePluginOverlay" crates/lattice-cli/src crates/lattice-cli/tests
```

Expected result: no matches in active CLI/TUI implementation. Guard deny-lists may contain the strings as test needles. Historical `.planning` and Phase 04 workspace crates are intentionally out of scope.

---

## Validation Sign-Off

- [x] All tasks have automated verification commands or Wave 0 dependencies.
- [x] Sampling continuity: no 3 consecutive tasks without automated verify.
- [x] Wave 0 covers missing research references.
- [x] No watch-mode flags.
- [x] Feedback latency target documented.
- [x] `nyquist_compliant: true` set in frontmatter.

**Approval:** executed
