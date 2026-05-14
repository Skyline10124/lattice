---
phase: 01-profile-contract-extraction
plan: "03"
subsystem: tools
tags: [lattice-tools, capability-selection, tool-bundles, preset-removal]

requires:
  - phase: 01-profile-contract-extraction
    provides: "decision coverage D-26 through D-32, tool bundle vocabulary"
provides:
  - "Bundle + explicit enabled tool name capability API in lattice-tools"
  - "Deterministic BTreeSet union of bundle-expanded and explicit tool names (D-29)"
  - "Source guard tests preventing legacy preset reintroduction (T-01-10)"
affects: ["lattice-runtime", "lattice-orchestrator", "lattice-cli"]

tech-stack:
  added: []
  patterns:
    - "CapabilitySelection { bundles, enabled } replaces preset-based tool selection"
    - "BTreeSet<String> deterministic deduplication for tool name union"

key-files:
  created: []
  modified:
    - crates/lattice-tools/src/capabilities.rs
    - crates/lattice-tools/src/lib.rs
    - crates/lattice-tools/tests/exposure.rs

key-decisions:
  - "Delete ToolPreset enum entirely rather than deprecating (D-30)"
  - "Union bundle tools + explicit enabled names via BTreeSet for determinism (D-29, T-01-11)"
  - "Source guard tests use include_str! to check entire source tree for preset regressions"

patterns-established:
  - "preset_absence_guard: denylist-based source scan test prevents API regression"
  - "resolve_tool_names public API for tool-name-only queries without definition collection"

requirements-completed:
  - PROF-03

duration: 21min
completed: 2026-05-14
---

# Phase 01 Plan 03: Remove Preset Capability API, Keep Bundle+Enabled Tool Selection

**Deleted ToolPreset/CapabilitySelection.presets from lattice-tools and replaced with bundles + explicit enabled tool name union using deterministic BTreeSet deduplication.**

## Performance

- **Duration:** 21min (20m 58s)
- **Started:** 2026-05-14T11:12:26Z
- **Completed:** 2026-05-14T11:33:24Z
- **Tasks:** 2
- **Files modified:** 3

## Accomplishments

- Removed `ToolPreset` enum (Minimal, Reader, Coding) and `CapabilityError::UnknownPreset` entirely
- Removed `CapabilitySelection.presets` field and all preset resolution branches
- `CapabilitySelection` now uses `bundles: Vec<ToolBundle>` + `enabled: Vec<String>`, both defaulting to empty
- `resolve_capabilities` and `resolve_tool_names` produce deterministic `BTreeSet<String>` union of bundle-expanded tools plus explicit enabled names (D-29)
- Coding-capable tool set expressed as explicit bundle list `[workspace, filesystem, editing, shell, planning, web]` rather than a preset
- Bundle vocabulary preserved: core, workspace, filesystem, editing, shell, planning, web (D-31)
- Source guard test (`preset_absence_guard`) scans all lattice-tools source for legacy preset strings and fails on regression (T-01-10)
- `resolve_tool_names` made public for callers needing tool-name-only resolution

## Task Commits

Each task was committed atomically:

1. **Task 1 RED: Add failing tests for bundle+enabled API** - `2e5eb29` (test)
2. **Task 1 GREEN: Replace preset API with bundle+enabled tool selection** - `528187f` (feat)
3. **Task 2: Rewrite exposure tests for bundle+enabled API with source guard** - `b36e8a4` (test)

## Files Created/Modified

- `crates/lattice-tools/src/capabilities.rs` - Removed ToolPreset enum, presets field, preset resolution; added enabled field, unified resolve_tool_names; updated all inline tests (14 tests, all passing)
- `crates/lattice-tools/src/lib.rs` - Removed ToolPreset from exports, added resolve_tool_names
- `crates/lattice-tools/tests/exposure.rs` - Rewrote all preset-based tests as bundle+enabled tests; added preset_absence_guard source scan (8 tests, all passing)

## Decisions Made

- Followed plan as specified -- no new decisions beyond those already captured in D-26 through D-32
- Implementation detail: `resolve_tool_names` was made public (previously private helper) to serve as a lightweight API for callers that only need tool names, not full definitions

## Deviations from Plan

None - plan executed exactly as written. The TDD RED/GREEN cycle produced two commits for Task 1 and a single test commit for Task 2.

## Issues Encountered

- Claude Code harness Edit/Write tools did not persist changes to disk despite reporting success; had to use Bash heredoc with `>|` (noclobber bypass) for all file writes. No code impact -- purely a tooling workaround.

## Threat Flags

None - changes remove attack surface (deleted ToolPreset and preset resolution paths, replaced with explicit bundle+enabled selection).

## Known Stubs

None - all data flows are wired. `CapabilitySelection.enabled: Vec<String>` holds explicit tool names that are unioned with bundle-expanded tools.

## Next Phase Readiness

- `lattice-tools` now exposes only bundle+enabled tool selection API
- Ready for Plan 04 (bridge compilation) which must update `lattice-runtime`, `lattice-orchestrator`, and `lattice-cli` call sites that still reference `ToolPreset`

---
*Phase: 01-profile-contract-extraction*
*Completed: 2026-05-14*
