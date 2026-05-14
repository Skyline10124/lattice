---
phase: 15-generic-agent-runtime-naming-profile-assembly
plan: 01
subsystem: tools
tags: [rust, lattice-tools, capabilities, presets, serde]

requires:
  - phase: 10-capability-bundles-profile-isolation
    provides: Capability bundle and preset isolation foundations
provides:
  - ToolPreset::Coding as the canonical coding capability preset
  - Backward-compatible ToolPreset::from_str("code-agent") alias
  - Updated lattice-tools exposure tests using coding terminology
affects: [phase-15-runtime-preset-assembly, lattice-runtime, lattice-orchestrator, lattice-cli]

tech-stack:
  added: []
  patterns:
    - Product-neutral capability preset names
    - Backward-compatible string aliases at configuration boundaries

key-files:
  created:
    - .planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-01-SUMMARY.md
  modified:
    - crates/lattice-tools/src/capabilities.rs
    - crates/lattice-tools/tests/exposure.rs

key-decisions:
  - "ToolPreset::Coding is the canonical coding capability preset name."
  - "The legacy string alias \"code-agent\" remains accepted by FromStr during migration."
  - "Serde output uses the canonical \"coding\" spelling through kebab-case enum serialization."

patterns-established:
  - "Compatibility aliases are parser-only; Rust enum variants use canonical product-neutral names."
  - "Reader profile isolation tests compare against Coding as the positive control for coding-only tools."

requirements-completed: [GEN-03, GEN-04]

duration: 3 min
completed: 2026-05-13
---

# Phase 15 Plan 01: Tool Preset Coding Rename Summary

**Canonical `ToolPreset::Coding` capability selection with a backward-compatible `code-agent` parser alias**

## Performance

- **Duration:** 3 min
- **Started:** 2026-05-13T03:19:11Z
- **Completed:** 2026-05-13T03:22:52Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments

- Replaced the `ToolPreset::CodeAgent` variant in `lattice-tools` with `ToolPreset::Coding`.
- Preserved existing profile compatibility by accepting both `"coding"` and `"code-agent"` in `FromStr`.
- Added serde roundtrip coverage proving the canonical serialized preset string is `"coding"`.
- Renamed exposure tests to coding terminology and kept reader profiles isolated from coding-only tools.

## Task Commits

Each task was committed atomically:

1. **Task 1 RED: Coding preset behavior tests** - `d7f52f5` (test)
2. **Task 1 GREEN: Rename ToolPreset to Coding** - `194bed1` (feat)
3. **Task 2: Rename exposure tests for Coding** - `4168233` (test)

## Files Created/Modified

- `crates/lattice-tools/src/capabilities.rs` - Defines `ToolPreset::Coding`, maps it to the existing coding bundle set, accepts both parser spellings, and tests serde/parser behavior.
- `crates/lattice-tools/tests/exposure.rs` - Uses Coding terminology for integration tests and verifies reader profiles do not expose coding-only tools.
- `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-01-SUMMARY.md` - Records the plan outcome.

## Decisions Made

- Used `ToolPreset::Coding` as the only Rust enum variant for the coding preset in `lattice-tools`.
- Kept `"code-agent"` as a `FromStr` alias only, so existing TOML/profile strings continue to parse while new serialization emits `"coding"`.
- Strengthened the reader isolation integration test with a Coding positive control so excluded tools are proven to be coding-preset tools.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Strengthened exposure acceptance coverage**
- **Found during:** Task 2 (Rename exposure.rs tests to use ToolPreset::Coding)
- **Issue:** The planned body-only rename left only one exact `Coding` token in `exposure.rs`, failing the plan's literal acceptance check requiring at least two.
- **Fix:** Added a Coding preset positive control in the reader isolation test before asserting reader profiles exclude coding-only tools.
- **Files modified:** `crates/lattice-tools/tests/exposure.rs`
- **Verification:** `cargo test -p lattice-tools --test exposure`; `grep -c 'Coding' crates/lattice-tools/tests/exposure.rs`
- **Committed in:** `4168233`

---

**Total deviations:** 1 auto-fixed (1 blocking verification issue)
**Impact on plan:** The deviation only strengthens test coverage and does not change production behavior.

## Issues Encountered

- `cargo test -p lattice-tools capabilities::tests` initially failed after Task 1 because Cargo compiled the still-unmigrated `exposure.rs` integration test. Task 2 completed that planned migration, and the exact command passed afterward.
- `rg -n 'ToolPreset::CodeAgent' crates/` still finds `crates/lattice-runtime/src/code_agent.rs`, which is outside this plan's ownership and is explicitly left for later Phase 15 runtime migration plans.

## Verification

- `cargo test -p lattice-tools capabilities::tests` - passed
- `cargo test -p lattice-tools --test exposure` - passed
- `cargo test -p lattice-tools` - passed
- `cargo clippy -p lattice-tools -- -D warnings` - passed
- `cargo fmt --all --check` - passed
- `rg -n 'CodeAgent' crates/lattice-tools` - zero results
- `rg -n 'ToolPreset::CodeAgent' crates/` - one downstream runtime reference remains for a later plan

## Known Stubs

None.

## Authentication Gates

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

Plan 15-02 can update runtime preset assembly against `ToolPreset::Coding`. The only known downstream reference to the removed variant is in `crates/lattice-runtime/src/code_agent.rs`, which is owned by the next runtime migration plan.

## Self-Check: PASSED

- Found modified files: `crates/lattice-tools/src/capabilities.rs`, `crates/lattice-tools/tests/exposure.rs`
- Found summary file: `.planning/phases/15-generic-agent-runtime-naming-profile-assembly/15-01-SUMMARY.md`
- Found task commits: `d7f52f5`, `194bed1`, `4168233`

---
*Phase: 15-generic-agent-runtime-naming-profile-assembly*
*Completed: 2026-05-13*
