---
phase: 11
plan: 06
subsystem: workspace-topology
tags: [deletion, loc-budget, boundary-check, release-verification]
dependency_graph:
  requires: [11-01, 11-02, 11-03, 11-04]
  provides: [clean-8-crate-workspace, loc-budget-gate, release-boundary-pass]
  affects: [root-Cargo.toml, all-crate-manifests, CLI-source, docs]
tech-stack:
  added: []
  patterns: [strict-loc-budget, boundary-check-script, docs-drift-remediation]
key-files:
  created:
    - crates/lattice-llm/src/logging.rs
  modified:
    - Cargo.toml
    - crates/lattice-llm/Cargo.toml
    - crates/lattice-llm/src/lib.rs
    - crates/lattice-cli/Cargo.toml
    - crates/lattice-cli/src/main.rs
    - crates/lattice-cli/src/lib.rs
    - crates/lattice-cli/src/commands/run.rs
    - crates/lattice-cli/src/commands/session.rs
    - crates/lattice-cli/src/commands/doctor.rs
    - crates/lattice-cli/src/config.rs
    - crates/lattice-cli/src/credentials.rs
    - crates/lattice-tools/Cargo.toml
    - crates/lattice-tools/src/lib.rs
    - crates/lattice-tools/src/executor.rs
    - crates/lattice-tools/src/tool_definitions.rs
    - crates/lattice-profile/src/lib.rs
    - README.md
    - AGENTS.md
    - CLAUDE.md
  deleted:
    - crates/lattice-core/ (entire directory, 9121 lines)
    - crates/lattice-runtime/ (entire directory, 2412 lines)
    - crates/lattice-prompt/ (entire directory, 3528 lines)
    - crates/lattice-sandbox/ (entire directory, 2243 lines)
    - crates/lattice-policy/ (entire directory, 2284 lines)
    - crates/lattice-cli/src/profile_runtime.rs
    - crates/lattice-cli/src/runtime.rs
    - crates/lattice-cli/src/runtime_turn.rs
    - crates/lattice-cli/src/security.rs
    - crates/lattice-cli/src/session.rs
    - crates/lattice-cli/src/session_seed.rs
    - crates/lattice-cli/src/model_inventory.rs
    - crates/lattice-cli/src/test_support.rs
    - crates/lattice-cli/src/display.rs
    - crates/lattice-cli/tests/profile_runtime_eval.rs
    - crates/lattice-tools/src/capabilities.rs
    - crates/lattice-tools/src/tool_registry.rs
    - crates/lattice-tools/tests/exposure.rs
    - crates/lattice-profile/src/lowering.rs
    - crates/lattice-profile/src/spec.rs
decisions:
  - Moved init_logging from deleted lattice-core to lattice-llm (natural home for infra utilities)
  - Inlined critical CLI helpers from deleted bridge files into run.rs rather than creating new modules
  - Deleted capabilities/tool_registry modules entirely (unused after agent-loop refactor)
  - Deleted lowering/spec modules from lattice-profile (unused after profile contract extraction)
  - Removed blob-store feature gate and bus:fetch tool definition (dead code after bus removal)
  - Accepted .planning/codebase/ docs drift per D-05 (planning artifacts may reference old crates)
metrics:
  duration: ~45min
  completed: 2026-05-17
  tasks_completed: 3
  files_modified: 20
  lines_deleted: ~20000
  lines_added: ~150
  final_loc: 19922
---

# Phase 11 Plan 06: Delete Old Crates, Prune Imports, Enforce LOC Budget Summary

Removed the five old crate identities (lattice-core, lattice-runtime, lattice-prompt, lattice-sandbox, lattice-policy) from the active workspace, fixed all transitive imports, and proved the strict 20k LOC budget passes with a clean 8-crate workspace.

## Completed Tasks

### Task 1: Delete old architecture crate directories and update root workspace metadata

- Removed 5 crate directories from filesystem (19,588 lines deleted)
- Updated root Cargo.toml: 8 workspace members, pruned 13 unused workspace dependencies
- Moved `init_logging` from lattice-core to lattice-llm to preserve CLI logging
- Commit: 97a23ad

### Task 2: Prune final crate manifests, fix source imports, regenerate Cargo.lock

- Updated lattice-cli/Cargo.toml: removed 4 old crate dependencies
- Fixed all import paths in CLI source (main.rs, run.rs, doctor.rs)
- Inlined helper functions from deleted bridge files into run.rs
- Fixed session dir reference after deleting session.rs module
- Fixed cli_cutover test assertion (5 -> 6 commands including clap help)
- Added boundary test in run.rs checking production code has no old crate imports
- Deleted CLI bridge files (profile_runtime.rs, runtime.rs, runtime_turn.rs, security.rs, session.rs, session_seed.rs, model_inventory.rs, test_support.rs)
- Deleted profile_runtime_eval.rs test
- Commit: 28326a2

### Task 3: Enforce strict LOC and active-source boundary

- Deleted unused modules: capabilities.rs, tool_registry.rs, lowering.rs, spec.rs, display.rs
- Removed dead code: UiConfig, SecurityConfig structs, to_hashmap/diagnostics methods, RegistryToolAccess trait, BlobFetcher trait, blob_store/registry fields, blob-store cfg gate
- Deleted exposure.rs test (tested deleted capabilities module)
- Fixed docs drift: updated README.md, AGENTS.md, CLAUDE.md to 8-crate architecture, removed references to deleted commands and --trace flag
- Gated test-only not_found() helper with #[cfg(test)]
- Removed unused Arc import in executor tests
- Final LOC: 19,922 (budget: 20,000) -- PASS
- All 492 workspace tests pass
- Release boundary check: 6/7 groups PASS; docs drift only in .planning/codebase/ (allowed per D-05)
- Commit: c295200

## Verification Results

| Check | Result |
|-------|--------|
| `cargo check --workspace` | PASS (0 errors, 0 warnings) |
| `cargo test --workspace` | PASS (492 tests) |
| `source-loc-budget.sh --max 20000` | PASS (19,922 lines) |
| `cargo metadata` workspace members | 8 (correct) |
| `release-boundary-check.sh` | 6/7 PASS (docs drift in .planning/ only, allowed) |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical Functionality] Moved init_logging to lattice-llm**
- **Found during:** Task 1
- **Issue:** CLI depended on lattice_core::init_logging which was deleted with the crate
- **Fix:** Moved init_logging function to lattice-llm/src/logging.rs with tracing-subscriber dependency
- **Files modified:** crates/lattice-llm/src/logging.rs (new), crates/lattice-llm/src/lib.rs, crates/lattice-llm/Cargo.toml, crates/lattice-cli/src/main.rs
- **Commit:** 97a23ad

**2. [Rule 3 - Blocking Issue] Inlined CLI helpers from deleted bridge files**
- **Found during:** Task 2
- **Issue:** Deleting profile_runtime.rs and session.rs removed functions still called from run.rs and doctor.rs
- **Fix:** Inlined default_session_dir(), profile_source_from_profile_arg(), missing_default_message(), create_default_profile(), safe_agents_dir(), validate_agents_dir() into run.rs; made commands::session::default_session_dir() pub(crate)
- **Files modified:** crates/lattice-cli/src/commands/run.rs, crates/lattice-cli/src/commands/session.rs, crates/lattice-cli/src/commands/doctor.rs
- **Commit:** 28326a2

**3. [Rule 1 - Bug] Fixed boundary test self-reference**
- **Found during:** Task 2
- **Issue:** run_source_has_no_old_crate_imports test's forbidden array contains old crate names, causing the test to always fail against its own source
- **Fix:** Split source at #[cfg(test)] marker and only check production code portion
- **Files modified:** crates/lattice-cli/src/commands/run.rs
- **Commit:** 28326a2

**4. [Rule 1 - Bug] Fixed cli_cutover command count assertion**
- **Found during:** Task 2
- **Issue:** Test expected 5 commands but clap adds built-in "help" making 6
- **Fix:** Changed assertion from 5 to 6
- **Files modified:** crates/lattice-cli/tests/cli_cutover.rs
- **Commit:** 28326a2

**5. [Rule 2 - Missing Critical Functionality] Removed dead blob-store feature gate**
- **Found during:** Task 3
- **Issue:** blob-store feature removed from Cargo.toml but cfg gate still in tool_definitions.rs, causing compiler warning
- **Fix:** Removed the entire #[cfg(feature = "blob-store")] bus:fetch tool definition block
- **Files modified:** crates/lattice-tools/src/tool_definitions.rs
- **Commit:** c295200

**6. [Rule 1 - Bug] Removed unused test import**
- **Found during:** Task 3
- **Issue:** std::sync::Arc imported but unused in executor test module
- **Fix:** Removed the unused import
- **Files modified:** crates/lattice-tools/src/executor.rs
- **Commit:** c295200

**7. [Rule 2 - Missing Critical Functionality] Deleted orphaned exposure test**
- **Found during:** Task 3
- **Issue:** crates/lattice-tools/tests/exposure.rs referenced deleted capabilities.rs module and imported removed types
- **Fix:** Deleted the entire test file and empty tests/ directory
- **Files modified:** crates/lattice-tools/tests/exposure.rs (deleted)
- **Commit:** c295200

## Known Stubs

None. All data paths are wired; no placeholder text or empty hardcoded values remain.

## Threat Flags

None. No new security surface introduced.

## Self-Check: PASSED

- SUMMARY.md: FOUND
- Commit 97a23ad (Task 1): FOUND
- Commit 28326a2 (Task 2): FOUND
- Commit c295200 (Task 3): FOUND
- Old crate directories: ALL GONE
- Workspace members: 8 (correct)
