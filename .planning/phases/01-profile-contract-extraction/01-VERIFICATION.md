---
phase: 01-profile-contract-extraction
verified: 2026-05-14T13:45:00Z
status: passed
score: 15/15 must-haves verified
overrides_applied: 0
re_verification:
  previous_status: gaps_found
  previous_score: 15/15 (one truth failed verification due to test compilation error)
  gaps_closed:
    - "Unit tests cover profile parsing/defaulting/lowering and reject removed fields or concepts — fixed by adding `use std::path::Path;` to discovery.rs test module (line 90)"
  gaps_remaining: []
  regressions: []
---

# Phase 1: Profile Contract Extraction — Re-Verification Report

**Phase Goal:** Extract profile contract — strict v1 schema, discovery, lowering, remove preset API
**Verified:** 2026-05-14
**Status:** passed (all gaps closed)
**Re-verification:** Yes — after gap closure

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | Strict v1 `agent.toml` schema exists with only allowed sections | VERIFIED | `crates/lattice-profile/src/schema.rs` — `#[serde(deny_unknown_fields)]` on `AgentProfile` and all section structs. Only `schema_version = 1` accepted. Allowed sections: `[agent]`, `[prompt]`, `[tools]`, `[security]`, `[context]`, `[behavior]`. |
| 2 | Forbidden legacy sections/fields are rejected (D-22) | VERIFIED | schema.rs tests reject all 8 forbidden top-level sections (`[capabilities]`, `[handoff]`, `[orchestrator]`, `[plugins]`, `[pipeline]`, `[subagents]`, `[micro_agents]`, `[memory]`) and 8 forbidden fields (`agent.preset`, `agent.type`, `agent.skippable`, `agent.tags`, `behavior.behavior_type`, `behavior.confidence_threshold`, `behavior.max_retries`, `behavior.enforce_sandbox`). Loader pre-scans TOML for forbidden sections before strict deserialization. |
| 3 | Profile errors are typed and actionable | VERIFIED | `crates/lattice-profile/src/error.rs` — `ProfileError` enum with 14 variants carrying `path`, `section`, `field`, `value`, `reason`, and `expected` fields per D-23. |
| 4 | `prompt.system_path` resolves relative to package dir, cannot escape | VERIFIED | `crates/lattice-profile/src/load.rs` — `load_prompt_file()` rejects absolute paths, `..` traversal, symlink escapes (canonicalize check), directories, and missing files. Tests cover all rejection cases. |
| 5 | Discovery supports explicit path, named, and default resolution (D-02, D-05, D-06) | VERIFIED | `crates/lattice-profile/src/discovery.rs` — `ProfileSource` (Path/Name/Default) and `DiscoveryPaths`. Project-local wins over global. Default uses name "default" with same priority. |
| 6 | Missing default returns typed error with template, no files written (D-07) | VERIFIED | `resolve_agent_profile()` returns `ProfileError::MissingDefault{dir, template}` without creating files or directories. Test `missing_default_creates_no_files` proves no side effects. |
| 7 | `default_agent_template` generates valid strict v1 TOML (D-08) | VERIFIED | `crates/lattice-profile/src/defaults.rs` — template includes comments about editing role/prompt/tools/security. Tests verify it parses as valid `AgentProfile`, contains only allowed sections, and has no forbidden legacy concepts. |
| 8 | `GenericAgentSpec` is pure data with no upper-layer semantics (PROF-02) | VERIFIED | `crates/lattice-profile/src/spec.rs` — `GenericAgentSpec` has exactly 8 fields (profile_name, model, description, system_prompt, tools, security, context, behavior). No cwd, project_root, handoff, orchestrator, preset, or coding fields. Source guard test verifies no forbidden symbols in production code. |
| 9 | `lower_profile_to_spec` is pure data copy, no live assembly (PROF-02) | VERIFIED | `crates/lattice-profile/src/lowering.rs` — copies already-loaded data only. Validates non-empty name/model/prompt. Does not resolve providers, credentials, tool bundles, or construct live Agent/Plugin/Session. Source guard test verifies no forbidden imports. |
| 10 | `ToolPreset` removed from `lattice-tools`; bundles+enabled only (PROF-03) | VERIFIED | `crates/lattice-tools/src/capabilities.rs` — No `ToolPreset` enum. `CapabilitySelection` has only `bundles: Vec<ToolBundle>` and `enabled: Vec<String>`. `resolve_tool_names` produces deterministic `BTreeSet` union. Coding-capable tool set expressed as explicit bundle list `[workspace, filesystem, editing, shell, planning, web]`. |
| 11 | `AgentPreset` removed from `lattice-orchestrator` exports (PROF-03) | VERIFIED | `crates/lattice-orchestrator/src/agent_spec.rs` — No `AgentPreset` enum, no `preset` field in `AgentTurnSpec`. Test `profile_lowering_no_longer_derives_preset_from_capabilities` proves `lower_profile_to_turn_spec` ignores `capabilities.presets`. `rg -n "AgentPreset" crates/lattice-orchestrator/src` returns no matches. |
| 12 | CLI `new agent` uses `lattice_profile::default_agent_template` | VERIFIED | `crates/lattice-cli/src/commands/new_agent.rs` calls `lattice_profile::default_agent_template(&name, &default_model)`. Tests verify v1 sections present, legacy fields absent. |
| 13 | `lattice-profile` has no forbidden upper-layer dependencies (D-18) | VERIFIED | `rg -n "lattice-(runtime|orchestrator|agent|session|plugin)|lattice-cli" crates/lattice-profile/Cargo.toml` returns no matches. Only depends on `serde`, `serde_json`, `toml`, `thiserror` (deps), `tempfile` (dev-deps). |
| 14 | Workspace compiles with all Phase 1 changes | VERIFIED | `cargo check --workspace` passes (2 warnings in lattice-runtime only, no errors). |
| 15 | Unit tests cover profile parsing/defaulting/lowering and reject removed fields or concepts (ROADMAP SC #4) | VERIFIED | `cargo test -p lattice-profile` — **55 passed** (2 suites). Was previously failing due to missing `use std::path::Path;` import in `discovery.rs` test module. **FIXED:** import added at line 90. All tests now compile and pass. |

### Previously Failed Truth (Gap Closed)

| # | Truth | Previous Status | Current Status | Evidence |
|---|-------|----------------|----------------|----------|
| F1 | Unit tests cover profile parsing/defaulting/lowering and reject removed fields or concepts (ROADMAP SC #4) | FAILED (3x E0425 compile errors) | VERIFIED | `cargo test -p lattice-profile` — 55 passed, 0 failed. The missing `use std::path::Path;` import on `discovery.rs` line 90 is now present. |

**Score:** 15/15 truths verified (all gaps closed)

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
| ----------- | ---------- | ----------- | ------ | -------- |
| PROF-01 | 01-01, 01-02, 01-05 | Developer can define a generic agent profile that describes model, prompt, tool capabilities, behavior, working directory, and security intent without plugin, handoff, orchestrator, or subagent fields. | SATISFIED | Strict v1 schema in `schema.rs` accepts only allowed sections/fields. Forbidden sections rejected by `deny_unknown_fields` + pre-scan. `default_agent_template()` generates compliant TOML. CLI `new_agent` uses it. |
| PROF-02 | 01-02, 01-04, 01-05 | Runtime can lower a profile or CLI default into a pure generic turn/build spec without constructing a live Agent or reading CLI/TUI state. | SATISFIED | `lower_profile_to_spec()` in `lowering.rs` produces `GenericAgentSpec` with pure data copy only. `lower_profile_to_turn_spec()` in `agent_spec.rs` (transitional) no longer derives `AgentPreset`. Both validate non-empty name/model/prompt without live assembly. |
| PROF-03 | 01-03, 01-04, 01-05 | Coding-capable behavior is represented by profile prompt and tool capabilities, not by AgentPreset::Coding, coding_agent.rs, or a dedicated agent subtype. | SATISFIED | `ToolPreset` enum deleted from `lattice-tools`. `AgentPreset` enum deleted from `lattice-orchestrator` exports. Coding-capable tool access uses explicit bundles. Source guard tests prevent regression. |

### Required Artifacts

| Artifact | Expected | Status | Details |
| -------- | -------- | ------ | ------- |
| `crates/lattice-profile/Cargo.toml` | Narrow crate manifest, no forbidden deps | VERIFIED | Only serde, serde_json, toml, thiserror deps; tempfile dev-dep. No upper-layer crate deps. |
| `crates/lattice-profile/src/schema.rs` | Strict `schema_version = 1` profile schema | VERIFIED | `deny_unknown_fields` on all structs. Allowed sections: agent, prompt, tools, security, context, behavior. |
| `crates/lattice-profile/src/error.rs` | Typed actionable profile errors | VERIFIED | 14 `ProfileError` variants with path/section/field/value/reason/expected. |
| `crates/lattice-profile/src/load.rs` | Read-only TOML loading, package-local path resolution | VERIFIED | `load_agent_profile()` with forbidden-section pre-scan, schema_version check, mutual exclusivity, path containment. Read-only. |
| `crates/lattice-profile/src/discovery.rs` | Explicit/name/default agent.toml resolution | VERIFIED | Production code and tests both work. `use std::path::Path;` import added at line 90. 55 tests pass. |
| `crates/lattice-profile/src/defaults.rs` | Auto-generated default agent template | VERIFIED | `default_agent_template(name, model)` returns valid v1 TOML string. No filesystem writes. |
| `crates/lattice-profile/src/spec.rs` | Pure profile-derived generic agent spec | VERIFIED | `GenericAgentSpec` — 8 fields, no runtime/orchestrator/plugin semantics. |
| `crates/lattice-profile/src/lowering.rs` | `LoadedAgentProfile` -> `GenericAgentSpec` conversion | VERIFIED | Pure data copy. Validates non-empty name/model/prompt. No live assembly. |
| `crates/lattice-tools/src/capabilities.rs` | Bundle+enabled capability API (no ToolPreset) | VERIFIED | `CapabilitySelection { bundles, enabled }` only. No `ToolPreset` enum. |
| `crates/lattice-orchestrator/src/agent_spec.rs` | Compile-safe transitional spec without AgentPreset | VERIFIED | No `AgentPreset` enum. `lower_profile_to_turn_spec` ignores `capabilities.presets`. 7/7 tests pass. |
| `crates/lattice-cli/src/commands/new_agent.rs` | CLI template bridge to lattice-profile defaults | VERIFIED | Calls `lattice_profile::default_agent_template()`. 4/4 tests pass. |

### Key Link Verification

| From | To | Via | Status |
| ---- | -- | --- | ------ |
| Root `Cargo.toml` | `crates/lattice-profile/Cargo.toml` | Workspace member + `[workspace.dependencies]` | WIRED |
| `crates/lattice-cli/Cargo.toml` | `crates/lattice-profile` | `lattice-profile = { workspace = true }` | WIRED |
| `discovery.rs` | `load.rs` | `resolve_agent_profile()` calls `load_agent_profile()` | WIRED |
| `lowering.rs` | `spec.rs` | `lower_profile_to_spec()` constructs `GenericAgentSpec` | WIRED |
| `new_agent.rs` | `defaults.rs` | `lattice_profile::default_agent_template()` | WIRED |
| `lattice-runtime/preset.rs` | `lattice-tools/capabilities.rs` | `CapabilitySelection { bundles, enabled }` — no presets | WIRED |

### Data-Flow Trace (Level 4)

| Artifact | Data Variable | Source | Produces Real Data | Status |
| -------- | ------------- | ------ | ------------------ | ------ |
| `load.rs:load_agent_profile` | `system_prompt: String` | `std::fs::read_to_string` for inline, `load_prompt_file()` for system_path | Yes — reads real files | FLOWING |
| `lowering.rs:lower_profile_to_spec` | `GenericAgentSpec` fields | Copies from `LoadedAgentProfile` fields | Yes — data pass-through | FLOWING |
| `new_agent.rs:run` | `template: String` | `lattice_profile::default_agent_template()` | Yes — generates real TOML | FLOWING |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
| -------- | ------- | ------ | ------ |
| `lattice-profile` lib compiles | `cargo check -p lattice-profile` | 0 errors, 0 warnings | PASS |
| `lattice-profile` tests run | `cargo test -p lattice-profile` | **55 passed** (2 suites) | PASS |
| `lattice-tools` capabilities tests | `cargo test -p lattice-tools capabilities` | 14 passed, 0 failed | PASS |
| `lattice-tools` exposure tests | `cargo test -p lattice-tools --test exposure` | 8 passed, 0 failed | PASS |
| `lattice-orchestrator` agent_spec tests | `cargo test -p lattice-orchestrator agent_spec` | 7 passed, 0 failed | PASS |
| `lattice-cli` new_agent tests | `cargo test -p lattice-cli new_agent` | 4 passed, 0 failed | PASS |
| Workspace-wide check | `cargo check --workspace` | 0 errors, 2 warnings | PASS |
| No forbidden crate deps | `rg "lattice-(runtime\|orchestrator\|agent\|session\|plugin)\|lattice-cli" crates/lattice-profile/Cargo.toml` | No matches | PASS |
| No `AgentPreset` in orchestrator src | `rg "AgentPreset" crates/lattice-orchestrator/src` | No matches | PASS |
| No `ToolPreset` in lattice-tools src | `rg "ToolPreset\|UnknownPreset" crates/lattice-tools/src` | No matches | PASS |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
| ---- | ---- | ------- | -------- | ------ |
| `crates/lattice-profile/src/discovery.rs` | 89 | `use std::io::Write;` (unused import in test) | INFO | Minor code quality. Does not produce compiler warnings in current toolchain. |
| `crates/lattice-profile/src/load.rs` | 183 | `use std::io::Write;` (unused import in test) | INFO | Same as above. |
| `crates/lattice-profile/src/lowering.rs` | 106 | `use std::io::Write;` (unused import in test) | INFO | Same as above. |

_Note: These unused imports do not produce compiler warnings (`cargo check -p lattice-profile` emits 0 warnings). They are cosmetic code quality items, not blockers._

### Human Verification Required

_(None — all verification items are programmatically checkable)_

### Gaps Summary

**No gaps remain.** The single gap from the previous verification — missing `use std::path::Path;` import in `crates/lattice-profile/src/discovery.rs` test module (causing 3x E0425 compile errors in `cargo test -p lattice-profile`) — has been fixed. The import is now present at line 90 of `discovery.rs`.

All 55 tests in `lattice-profile` now compile and pass. All other crate tests pass (lattice-tools: 22, lattice-orchestrator: 7, lattice-cli: 4). Workspace-wide `cargo check` passes with 0 errors.

All 4 ROADMAP success criteria for Phase 1 are met:
1. Generic profile/spec contract exists in `lattice-profile` with no plugin/handoff/orchestrator fields.
2. Existing runtime/CLI default profile data can be represented without `AgentPreset::Coding`.
3. Profile lowering is pure data conversion and does not construct live agents, read CLI/TUI state, or access session stores.
4. Unit tests cover profile parsing/defaulting/lowering and reject removed fields or concepts — verified by 55 passing tests.

---

_Verified: 2026-05-14T13:45:00Z_
_Verifier: Claude (gsd-verifier)_
