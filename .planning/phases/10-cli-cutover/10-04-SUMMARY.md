---
phase: 10-cli-cutover
plan: 04
subsystem: cli
tags: [model-router, diagnostics, credential-safety, alias-table]

requires:
  - phase: 07-functional-llm-and-direct-profiles
    provides: "ModelRouter with fixed alias table and CredentialStatus"
  - phase: 10-cli-cutover
    provides: "Plan 01 CLI surface cutover (clap boundary)"
provides:
  - "ModelRouter::aliases() helper for fixed v1.1 alias enumeration"
  - "models command using ModelRouter instead of old runtime catalog"
  - "resolve command using ModelRouter instead of old runtime resolver"
  - "doctor command using ModelRouter instead of old Catalog/Runtime diagnostics"
  - "cli_diagnostics test suite covering credential safety, alias metadata, and no-leak guarantees"
affects: [10-05-verification]

tech-stack:
  added: []
  patterns: ["ModelRouter-owned alias enumeration", "credential-safe diagnostic rendering"]

key-files:
  created: []
  modified:
    - "crates/lattice-llm/src/router.rs"
    - "crates/lattice-cli/src/commands/models.rs"
    - "crates/lattice-cli/src/commands/resolve.rs"
    - "crates/lattice-cli/src/commands/doctor.rs"
    - "crates/lattice-cli/src/main.rs"

key-decisions:
  - "ModelRouter::aliases() returns all known aliases (including openai-compatible) as Vec<ResolvedModel>"
  - "models --auth text lists alias names one per line; --json emits structured array with all metadata"
  - "resolve default text shows provider/provider_model only; verbose adds alias/auth_env/credential_status/base_url"
  - "doctor covers config loading, credential env var names/status, session store availability, alias table count"
  - "Removed provider_override and trace terminology from resolve command per D-05/D-28"

patterns-established:
  - "ModelRouter as single source of truth for alias/auth data in CLI diagnostics"
  - "cli_diagnostics test filter for targeted diagnostic test execution"

requirements-completed: [CLI-03]

duration: 9min
completed: 2026-05-17
---

# Phase 10 Plan 04: Credential-Safe Diagnostics Summary

**ModelRouter-backed models/resolve/doctor with credential-safe rendering and no old catalog/runtime dependencies**

## Performance

- **Duration:** 9 min
- **Started:** 2026-05-17T09:52:50Z
- **Completed:** 2026-05-17T10:02:20Z
- **Tasks:** 2
- **Files modified:** 5

## Accomplishments

- Added `ModelRouter::aliases()` returning the fixed v1.1 alias list with full `ResolvedModel` metadata
- Rewrote `models`, `resolve`, and `doctor` CLI commands to use `lattice_llm::ModelRouter` exclusively
- Removed all references to old `lattice_core::Catalog`, `Runtime::list_models()`, `Runtime::list_authenticated_models()`, and `Runtime::inspect_model()` from supported command code
- Added 38 `cli_diagnostics` tests covering credential safety, alias enumeration, metadata rendering, and no-leak guarantees
- All 438 lattice-cli tests and 128 lattice-llm tests pass

## Task Commits

1. **Task 10-04-01: Add credential-safe diagnostics tests** - `70ad807` (test)
   - Added `ModelRouter::aliases()` helper and `router_cli_diagnostics` tests
   - Added `cli_diagnostics` tests to models.rs, resolve.rs, doctor.rs
   - Updated main.rs dispatch to pass json/verbose flags

2. **Task 10-04-02: Implement ModelRouter-backed models, resolve, and doctor** - `70ad807` (test)
   - Implementation was completed alongside tests in the same commit
   - All three commands now use `ModelRouter` exclusively
   - Old catalog/runtime diagnostic APIs removed from supported command code

## Files Created/Modified

- `crates/lattice-llm/src/router.rs` - Added `aliases()` method returning fixed v1.1 alias list; added `router_cli_diagnostics` tests
- `crates/lattice-cli/src/commands/models.rs` - Rewrote to use `ModelRouter::aliases()` for listing; text lists alias names one per line; JSON emits structured array; removed old runtime/catalog/status_icon dependencies
- `crates/lattice-cli/src/commands/resolve.rs` - Rewrote to use `ModelRouter::resolve()`; default text shows provider/provider_model only; verbose adds all metadata; removed provider_override and trace terminology
- `crates/lattice-cli/src/commands/doctor.rs` - Rewrote to use `ModelRouter::aliases()` for credential status; reports config loading, credential env var names/status, session store availability, alias table count; removed old Catalog/Runtime references
- `crates/lattice-cli/src/main.rs` - Updated dispatch to pass `cli.json` to models and doctor, `cli.verbose` to resolve

## Decisions Made

- `ModelRouter::aliases()` returns all known aliases (resolving each one), so credential status is live-checked
- `models --auth` text output is a simple list of alias names, one per line, with no status icons per D-26
- `resolve` default text is deliberately short (provider + provider_model only) per D-28
- `doctor` does not reference old Catalog, Runtime, or inspect_model per D-30
- Provider override removed from `resolve` command per D-05/D-10/D-28

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## Known Stubs

None - all diagnostic commands are fully wired to ModelRouter with live credential status checks.

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| T-10-01 | models.rs, resolve.rs, doctor.rs | Credential presence rendered to users via env var names and present/missing status only; tests verify no secret leakage |
| T-10-06 | resolve.rs | Unknown aliases return explicit error with no provider/model fallback |
| T-10-10 | doctor.rs | Reports only v1.1 CLI/API health data; no old full catalog/runtime diagnostics |
| T-10-11 | models.rs, resolve.rs (JSON) | Structured JSON includes explicit alias/provider/provider_model/auth_env/auth_status/base_url fields |

## Self-Check

- [x] All 38 cli_diagnostics tests pass
- [x] All 17 router tests pass (including 3 new router_cli_diagnostics tests)
- [x] Full lattice-cli test suite (438 tests) passes
- [x] Full lattice-llm test suite (128 tests) passes
- [x] No old Catalog/list_models/list_authenticated_models/inspect_model references in supported command code
- [x] No credential values in diagnostic output (verified by test assertions)
- [x] Build succeeds with 0 errors

## Next Phase Readiness

- CLI diagnostics are fully cutover to ModelRouter
- Plan 10-05 (verification) can run full boundary audits
- No blockers for subsequent plans

---
*Phase: 10-cli-cutover*
*Plan: 04*
*Completed: 2026-05-17*
