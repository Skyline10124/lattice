---
phase: 09-agent-loop-tools-and-api-facade
plan: 02
subsystem: tools
tags: [tools, enabled-tools, project-root, containment, llm-definitions, lattice-tools]

# Dependency graph
requires:
  - phase: 07-functional-llm-and-direct-profiles
    provides: lattice-llm ToolDefinition type for LLM schema conversion
  - phase: 01-profile-contract-extraction
    provides: AgentProfile.tools.enabled Vec<String> as the v1.1 authorization input
provides:
  - Explicit enabled-tool resolver that filters inventory by exact profile names
  - LLM-compatible ToolDefinition adapter for ChatRequest.tools
  - Project-root executor path with traversal and symlink-escape containment
  - No-bundle, no-approval v1.1 tool boundary
affects: [09-03, 09-04, agent-loop, api-facade]

# Tech tracking
tech-stack:
  added: [lattice-llm dependency in lattice-tools]
  patterns: [explicit-enabled-name resolution, project-root path containment, for_project_root constructor]

key-files:
  created:
    - crates/lattice-tools/src/enabled_tools.rs
  modified:
    - crates/lattice-tools/Cargo.toml
    - crates/lattice-tools/src/executor.rs
    - crates/lattice-tools/src/lib.rs

key-decisions:
  - "enabled_tools module uses exact name matching against default_tool_definitions, no bundle expansion (D-13, D-14)"
  - "for_project_root constructor canonicalizes root and applies explicit allowlist, no SandboxConfig dependency for v1.1 boundary (D-19, D-20, D-21, D-22)"
  - "reject_parent_traversal is a fast pre-check before filesystem access; ensure_under_project_root validates after canonicalization"

patterns-established:
  - "Explicit enabled-name resolver: resolve_enabled_tools filters inventory by exact names, fails fast on unknown"
  - "Project-root constructor: for_project_root combines canonicalized root + authorized_tools without approval/policy machinery"
  - "Path containment: reject_parent_traversal + ensure_under_project_root applied to all file/search/edit tool arms"

requirements-completed: ["TOOL-01", "TOOL-02", "TOOL-03", "TOOL-04"]

# Metrics
duration: 6min
completed: 2026-05-17
---

# Phase 9 Plan 02: Tools Boundary Summary

**Explicit enabled-name tool resolver and project-root path containment for v1.1 main path, replacing bundle expansion and sandbox approval**

## Performance

- **Duration:** 6 min
- **Started:** 2026-05-17T21:19:36Z
- **Completed:** 2026-05-17T21:25:33Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments

- Created `enabled_tools` module with `resolve_enabled_tools` that filters `default_tool_definitions()` by exact profile `tools.enabled` names, returning `EnabledTools` (BTreeSet + Vec<lattice_llm::ToolDefinition>) or `EnabledToolError` with unknown/available lists
- Added `for_project_root` constructor on `DefaultToolExecutor` that canonicalizes the project root, applies explicit tool allowlist, and sets up path containment without SandboxConfig approval/policy dependency
- Added `reject_parent_traversal` and `ensure_under_project_root` private helpers enforced on all file/search/edit tool arms (read_file, read_files, write_file, grep, list_directory, find_files, workspace_context)
- Added project-root containment to recursive grep and find_files via context structs, rejecting symlink escapes during traversal

## Task Commits

Each task was committed atomically:

1. **Task 1: Add explicit enabled-tool resolver and LLM schema adapter** - `2179a71` (test)
2. **Task 2: Add project-root executor path for v1.1 file tools** - `154a47e` (feat)

## Files Created/Modified

- `crates/lattice-tools/src/enabled_tools.rs` - v1.1 explicit enabled-name resolver and LLM ToolDefinition adapter
- `crates/lattice-tools/Cargo.toml` - Added lattice-llm workspace dependency
- `crates/lattice-tools/src/lib.rs` - Added enabled_tools module declaration and exports
- `crates/lattice-tools/src/executor.rs` - Added for_project_root constructor, path containment helpers, and project-root checks on all file/search/edit arms

## Decisions Made

- `enabled_tools` module is the final public name (not `v11` or `simple`) per D-03/D-04
- `resolve_enabled_tools` does not call `resolve_capabilities` or expand `ToolBundle` per D-14
- `for_project_root` does not set `policy_authority` or `approved_rules` -- the v1.1 main path has no dynamic approval flow
- Existing file-size, output, command, and HTTP limits are preserved per D-25/D-26
- `reject_parent_traversal` checks both `/` and `\` separated paths for cross-platform safety

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Known Stubs

None - all functionality is fully wired.

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| T-09-01 mitigated | executor.rs | Path traversal and symlink escape checks added to all file/search/edit tool arms |
| T-09-02 mitigated | enabled_tools.rs | Unknown enabled names fail before turn execution; bash/web_search require explicit names |

## Next Phase Readiness

- `resolve_enabled_tools` and `for_project_root` are ready for `AgentLoop` to consume in plans 09-03 and 09-04
- `lattice-api` can use `resolve_enabled_tools` to derive tool definitions from profile and pass to agent loop
- No blockers for downstream agent loop or API facade integration

## Self-Check: PASSED

- All 4 created/modified files verified present
- All 2 task commits verified in git log
- 64 tests pass (53 existing + 6 enabled_tools + 5 project_root)

---
*Phase: 09-agent-loop-tools-and-api-facade*
*Completed: 2026-05-17*
