---
phase: 15-generic-agent-runtime-naming-profile-assembly
verified: 2026-05-13T04:21:34Z
status: passed
score: "20/20 must-haves verified"
overrides_applied: 0
---

# Phase 15: Generic Agent Runtime Naming & Profile Assembly Verification Report

**Phase Goal:** Replace code-agent-specific internal construction with generic agent specs, profile assembly, and compatibility boundaries.
**Verified:** 2026-05-13T04:21:34Z
**Status:** passed
**Re-verification:** No - initial verification

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Runtime exposes product-neutral agent spec/factory concepts. | VERIFIED | `AgentSpec`, `BuiltAgent`, and `Runtime::build_agent` are public runtime concepts in `crates/lattice-runtime/src/lib.rs:246-356` and `crates/lattice-runtime/src/lib.rs:655-723`; old `CodeAgentSpec`/`build_code_agent` names are absent from crate source. |
| 2 | `lattice code` remains backed by the coding preset and preserves v1.2 behavior. | VERIFIED | CLI builds `AgentSpec::new(...).preset(AgentPreset::Coding)` and calls `runtime.build_agent` in `crates/lattice-cli/src/coding_agent.rs:177-203`; deterministic evals use the same path in `crates/lattice-cli/tests/code_agent_eval.rs:141-162`; `cargo test --workspace` passed including the v1.2 eval harness. |
| 3 | Code-agent naming remains only where it describes the user-facing coding preset or an explicit compatibility shim. | VERIFIED | Legacy API symbols `CodeAgentSpec`, `build_code_agent`, and `ToolPreset::CodeAgent` have zero crate-source references. Remaining `code-agent` source matches are compatibility aliases/tests in `capabilities.rs`, product/eval helper vocabulary in `code_agent_eval.rs`, historical Phase 14 path assertions, or fixture model IDs; no active construction API uses old names. |
| 4 | Non-coding profiles do not inherit coding capabilities by default. | VERIFIED | `ToolPreset::Minimal` resolves no bundles and `ToolPreset::Reader` resolves only Workspace/Filesystem in `crates/lattice-tools/src/capabilities.rs:97-109`; tests assert excluded coding tools in `capabilities.rs:242-287`, `exposure.rs:58-85`, `lib.rs:1302-1319`, `assembly.rs:490-520`, and `code_agent_eval.rs:587-593`. |
| 5 | ToolPreset::Coding is the canonical coding preset name. | VERIFIED | `ToolPreset::Coding` is the enum variant in `crates/lattice-tools/src/capabilities.rs:27-34`; serde serializes it as `"coding"` in `capabilities.rs:220-228`. |
| 6 | The string `"code-agent"` still resolves to `ToolPreset::Coding` via FromStr backward compatibility. | VERIFIED | `FromStr` maps `"coding" | "code-agent"` to `Self::Coding` in `capabilities.rs:165-174`; tests cover both strings in `capabilities.rs:214-218`. |
| 7 | `ToolPreset::CodeAgent` variant no longer exists. | VERIFIED | `rg -n 'ToolPreset::CodeAgent' crates/ --glob '!target/**'` returned zero results; enum definition contains only `Minimal`, `Reader`, and `Coding`. |
| 8 | AgentPreset::Coding exists as a public enum variant in lattice-runtime. | VERIFIED | `pub enum AgentPreset { Coding }` is defined in `crates/lattice-runtime/src/preset.rs:9-12` and re-exported in `lib.rs:38-39`. |
| 9 | Runtime::build_agent accepts an AgentSpec with optional preset and dispatches to apply_coding_preset. | VERIFIED | `AgentSpec` has `preset: Option<AgentPreset>` and builder methods in `lib.rs:246-326`; `Runtime::build_agent` matches `AgentPreset::Coding` and calls `crate::preset::apply_coding_preset` in `lib.rs:655-673`. |
| 10 | coding_system_prompt() is a public function exported from lattice-runtime. | VERIFIED | `pub fn coding_system_prompt` exists in `preset.rs:14-43` and is exported from `lib.rs:38-39`; CLI wrapper delegates to it in `coding_agent.rs:263-265`. |
| 11 | CodeAgentSpec struct and Runtime::build_code_agent method do not exist. | VERIFIED | `rg -n 'CodeAgentSpec|build_code_agent' crates/ --glob '!target/**'` returned zero results; `crates/lattice-runtime/src/code_agent.rs` is absent. |
| 12 | pub mod code_agent does not exist in lib.rs. | VERIFIED | `crates/lattice-runtime/src/lib.rs:38-39` declares `pub mod preset` and exports preset symbols; `rg -n 'pub mod code_agent' crates/` returned zero results. |
| 13 | All runtime unit tests pass. | VERIFIED | `cargo test --workspace` passed; runtime output included 25 passing runtime tests, including preset tests and `coding_preset_requires_runtime_security`. |
| 14 | Orchestrator assembly tests use `"coding"` preset string instead of `"code-agent"`. | VERIFIED | Test profile fixture uses `presets: vec!["coding".into()]` in `crates/lattice-orchestrator/src/assembly.rs:436-454`; `rg -n 'code-agent|code_agent|CodeAgent' crates/lattice-orchestrator/src/assembly.rs` returned zero results. |
| 15 | All orchestrator tests pass with updated names. | VERIFIED | `cargo test --workspace` passed; source has `coding_profile_exposes_editing_shell_planning_and_web` in `assembly.rs:554-567`. |
| 16 | Profile assembly still resolves coding tools through ToolPreset::Coding. | VERIFIED | `assembly.rs` parses capability preset strings through `ToolPreset::from_str` via `resolve_profile_tools`, and the fixture's `"coding"` preset exposes editing/shell/planning/web tools in `assembly.rs:554-567`. |
| 17 | `lattice code` command builds agent through `AgentSpec::new().preset(AgentPreset::Coding)` plus `runtime.build_agent()`. | VERIFIED | `crates/lattice-cli/src/coding_agent.rs:192-203` constructs `AgentSpec` with project root, cwd, messages, security, task-state seed, `.preset(AgentPreset::Coding)`, then calls `runtime.build_agent(spec)`. |
| 18 | CodeAgentSpec import is removed from CLI coding_agent.rs. | VERIFIED | CLI import is `use lattice_runtime::{AgentPreset, AgentSpec, Runtime};` in `coding_agent.rs:9`; `rg -n 'CodeAgentSpec' crates/lattice-cli/src/coding_agent.rs` returned zero results. |
| 19 | Deterministic eval tests use AgentSpec + coding preset and preserve all v1.2 behavioral anchors. | VERIFIED | Eval harness imports `AgentPreset, AgentSpec, Runtime` in `code_agent_eval.rs:18-21`; fixture builds through `.preset(AgentPreset::Coding)` and `runtime.build_agent` in `code_agent_eval.rs:149-162`; `cargo test --workspace` passed all eval tests. |
| 20 | coding_system_prompt in CLI wraps lattice_runtime::coding_system_prompt. | VERIFIED | `crates/lattice-cli/src/coding_agent.rs:263-265` returns `lattice_runtime::coding_system_prompt(workdir, workdir)`. |

**Score:** 20/20 truths verified

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `crates/lattice-tools/src/capabilities.rs` | ToolPreset enum with Coding variant and compatibility parsing | VERIFIED | Exists; GSD artifact helper passed; lines `27-34`, `97-109`, and `165-174` implement the required behavior. |
| `crates/lattice-tools/tests/exposure.rs` | Coding exposure and non-coding isolation tests | VERIFIED | Exists; GSD artifact helper passed; tests at lines `45-85` cover coding and reader isolation. |
| `crates/lattice-runtime/src/preset.rs` | AgentPreset, apply_coding_preset, coding_system_prompt, validation, tests | VERIFIED | Exists; GSD artifact helper passed; lines `9-117` implement preset assembly and lines `170-304` test it. |
| `crates/lattice-runtime/src/lib.rs` | Generic AgentSpec/build_agent exports and dispatch | VERIFIED | `AgentSpec` and `Runtime::build_agent` are wired in lines `246-356` and `655-723`; old `code_agent` module export is absent. |
| `crates/lattice-cli/src/coding_agent.rs` | CLI coding agent construction using generic AgentSpec plus AgentPreset::Coding | VERIFIED | Lines `177-203` route the CLI build path through `Runtime::build_agent`. |
| `crates/lattice-cli/tests/code_agent_eval.rs` | Migrated eval tests preserving v1.2 anchors | VERIFIED | Lines `1-24`, `141-162`, and `558-593` show the generic preset fixture and profile-isolation assertions. |
| `crates/lattice-orchestrator/src/assembly.rs` | Updated profile assembly test fixtures and names | VERIFIED | Lines `436-454` use `"coding"`; lines `490-567` prove reader isolation and coding exposure. |

### Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `ToolPreset::Coding` | Workspace, Filesystem, Editing, Shell, Planning, Web bundles | `ToolPreset::bundles()` match arm | VERIFIED | `capabilities.rs:97-109` returns exactly those bundles for Coding. |
| `FromStr for ToolPreset` | `"coding"` and `"code-agent"` -> `ToolPreset::Coding` | Match arm with alias | VERIFIED | `capabilities.rs:165-174`; serde alias and deserialization tests at `capabilities.rs:32` and `220-238`. |
| `Runtime::build_agent` | `preset::apply_coding_preset` | Match on `spec.preset` | VERIFIED | `lib.rs:662-671` dispatches `AgentPreset::Coding` to `apply_coding_preset`. |
| Runtime exports | `preset::coding_system_prompt` | `pub use preset::{coding_system_prompt, AgentPreset}` | VERIFIED | `lib.rs:38-39`. |
| `AgentSpec.preset` field | `AgentPreset::Coding` | Builder method plus `coding_preset()` helper | VERIFIED | `lib.rs:258-326`. |
| Orchestrator `make_profile()` | `ToolPreset::from_str("coding")` capability path | `CapabilitiesConfig { presets: vec!["coding".into()] }` | VERIFIED | Fixture at `assembly.rs:436-454`; resolver imports `ToolPreset` and parses capability strings. |
| CLI `build_coding_agent` | `Runtime::build_agent(AgentSpec ... AgentPreset::Coding)` | AgentSpec builder chain | VERIFIED | `coding_agent.rs:192-203`. |
| CLI eval fixture | Generic runtime preset path | `make_code_agent_at` builds `AgentSpec` then calls `runtime.build_agent` | VERIFIED | `code_agent_eval.rs:149-162`. |

### Data-Flow Trace (Level 4)

| Artifact | Data Variable | Source | Produces Real Data | Status |
|----------|---------------|--------|--------------------|--------|
| `crates/lattice-runtime/src/preset.rs` | `capabilities.tool_names` / `capabilities.definitions` | `resolve_capabilities(CapabilitySelection { presets: vec![ToolPreset::Coding] })` | Yes - used to authorize `DefaultToolExecutor` and expose agent tools in `preset.rs:63-80`. | VERIFIED |
| `crates/lattice-runtime/src/lib.rs` | `spec.preset` | `AgentSpec::preset` / `AgentSpec::coding_preset` builders | Yes - `Runtime::build_agent` dispatches on the typed preset and returns coding tool definitions in `lib.rs:662-671`. | VERIFIED |
| `crates/lattice-cli/src/coding_agent.rs` | CLI `CodingAgentBuildOptions` | User/session options converted into `AgentSpec` | Yes - workdir, messages, security, task-state seed, and coding preset flow into `runtime.build_agent` in `coding_agent.rs:177-203`. | VERIFIED |
| `crates/lattice-orchestrator/src/assembly.rs` | Profile capability strings | `CapabilitiesConfig.presets = ["coding"]` | Yes - profile assembly resolves names through capability parsing and tests prove coding tools are exposed while reader tools are isolated. | VERIFIED |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| Workspace compiles | `cargo build --workspace` | Finished successfully | PASS |
| Workspace tests pass | `cargo test --workspace` | Passed, including runtime preset tests, tools exposure tests, orchestrator assembly tests, and CLI evals | PASS |
| Warnings denied | `cargo clippy --workspace -- -D warnings` | Finished successfully | PASS |
| Formatting clean | `cargo fmt --all --check` | Exit 0 | PASS |
| Old API names absent | `rg -n 'CodeAgentSpec|build_code_agent|ToolPreset::CodeAgent' crates/ --glob '!target/**'` | Zero results | PASS |
| Runtime code-agent module absent | `find crates/lattice-runtime/src -maxdepth 1 -type f -printf '%f\n' \| sort` | Only `lib.rs` and `preset.rs` | PASS |

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|-------------|-------------|--------|----------|
| GEN-01 | 15-02, 15-04 | Runtime exposes a generic agent specification and factory API that is not named around `code_agent`. | SATISFIED | `AgentSpec` and `Runtime::build_agent` are the public construction path; old `CodeAgentSpec` and `build_code_agent` are absent. |
| GEN-02 | 15-04 | Existing `lattice code` command remains available as a coding preset backed by the generic runtime assembly path. | SATISFIED | `coding_agent.rs:177-203` builds `AgentSpec` with `AgentPreset::Coding`; deterministic evals passed in `cargo test --workspace`. |
| GEN-03 | 15-01, 15-02, 15-03 | Agent profile, capability, prompt-provider, security, and task-state assembly use product-neutral runtime concepts. | SATISFIED | `ToolPreset::Coding`, `AgentPreset::Coding`, `AgentSpec`, `RuntimeSecurity`, provider wiring, and task-state seed are typed/generic concepts in `capabilities.rs`, `preset.rs`, and `lib.rs`. |
| GEN-04 | 15-01, 15-03, 15-04 | Existing code-agent tests and callers have a clear migration path through compatibility aliases or focused updates. | SATISFIED | `code-agent` alias is accepted by FromStr and serde; CLI and eval callers migrated to `AgentSpec`; old API names have zero crate-source references. |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| None | - | - | - | Stub scan found no blocking TODO/FIXME/placeholders, empty user-visible implementations, or console-only handlers. Matches reviewed were legitimate format strings, an empty manifest event match arm, and a retired-tool error message. |

### Human Verification Required

None. This phase is code/API behavior, compatibility parsing, and regression coverage; all success criteria were verified from source plus automated cargo gates.

### Gaps Summary

No gaps found. All roadmap success criteria, plan must-haves, required artifacts, key links, requirements GEN-01 through GEN-04, compatibility aliases, review-fix behavior, and automated gates are verified.

---

_Verified: 2026-05-13T04:21:34Z_
_Verifier: the agent (gsd-verifier)_
