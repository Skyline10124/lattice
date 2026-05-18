---
phase: 08-session-graph-core
plan: 01
subsystem: session
tags: [graph, commit, sha256, sqlite, session-store]

requires:
  - phase: 07-functional-llm-and-direct-profiles
    provides: KISS direction and direct profile consumption
provides:
  - v1.1 graph types: GraphMessage, GraphToolCall, GraphSession, GraphRef, GraphHead
  - Turn commit types: TurnCommitStatus, TurnCommitInput, TurnCommit
  - Status/log/diff/reset types: TurnLogEntry, SessionStatus, SessionGraphSummary, GraphDiff, ResetResult
  - Narrow SessionGraphStore trait (create/open/status/list/log/commit_turn/checkout/branch/diff/reset)
  - Canonical commit ID helpers excluding storage metadata
  - Deterministic commit-message derivation from first user message
affects: [08-02-sqlite-persistence, 08-03-pointer-operations, 08-04-api-wiring]

tech-stack:
  added: []
  patterns: [canonical-hash-over-logical-fields, narrow-store-trait, commit-pointer-refs]

key-files:
  created:
    - crates/lattice-session/src/graph.rs - v1.1 graph contract, types, store trait, canonical helpers
  modified:
    - crates/lattice-session/src/lib.rs - public re-exports for v1.1 graph types

key-decisions:
  - "Commit message max length set to 72 characters (matches git convention)"
  - "Commit payload schema version is 1 (COMMIT_PAYLOAD_SCHEMA_VERSION)"
  - "GraphHead uses tagged enum (Branch/Detached) rather than separate fields"
  - "GraphRefKind is Branch-only in v1.1 (no tags per D-05)"
  - "derive_commit_message uses split_whitespace join for normalization"

patterns-established:
  - "Canonical hash pattern: SHA-256 over kind-tag + JSON of logical fields only"
  - "Narrow store trait pattern: only v1.1 operations, no legacy leakage"
  - "Commit-pointer refs: branches and HEAD point to commit IDs, not session IDs"

requirements-completed: [SESS-01, SESS-02, SESS-03, SESS-04, SESS-05]

duration: 4min
completed: 2026-05-16
---

# Phase 8 Plan 01: Session Graph Contract Summary

**v1.1 graph types, narrow SessionGraphStore trait, canonical SHA-256 commit IDs, and deterministic commit-message derivation**

## Performance

- **Duration:** 4 min
- **Started:** 2026-05-16T16:52:55Z
- **Completed:** 2026-05-16T16:56:59Z
- **Tasks:** 2
- **Files modified:** 2

## Accomplishments
- Created the v1.1 session graph contract with 15 public types covering messages, sessions, refs, HEAD, commits, log, status, diff, and reset
- Defined a narrow `SessionGraphStore` trait with only 10 methods (create/open/status/list/log/commit_turn/checkout/branch/diff/reset) that explicitly excludes legacy operations
- Implemented canonical SHA-256 commit ID helpers that hash only logical fields (schema version, session ID, parents, messages, profile, model, provider, status, commit_message, timestamp) and exclude all storage metadata
- Implemented deterministic `derive_commit_message` that normalizes whitespace, truncates to 72 chars, and falls back to "Turn commit"

## Task Commits

Each task was committed atomically:

1. **Task 1: Add v1.1 graph structs and narrow store trait** - `47dcc3f` (test)
2. **Task 2: Add canonical commit IDs and commit-message derivation** - `762119d` (feat)

## Files Created/Modified
- `crates/lattice-session/src/graph.rs` - v1.1 graph contract: 15 public types, SessionGraphStore trait, derive_commit_message, canonical_message_id, canonical_turn_commit_id, 18 contract tests
- `crates/lattice-session/src/lib.rs` - Added `pub mod graph` and public re-exports for all v1.1 graph types

## Decisions Made
- Commit message max length: 72 characters (aligns with git convention for subject lines)
- Schema version constant: `COMMIT_PAYLOAD_SCHEMA_VERSION = 1` for forward-compatible hash changes
- GraphHead uses a tagged enum (`Branch`/`Detached`) for type-safe HEAD state
- GraphRefKind is Branch-only in v1.1 (tags explicitly excluded per D-05)
- Commit message normalization uses `split_whitespace().collect::<Vec<_>>().join(" ")` for consistent whitespace handling

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- Plan 08-02 can implement `SqliteSessionGraphStore` behind the `SessionGraphStore` trait
- Plan 08-03 can implement pointer operations (checkout, branch, diff, reset) using the graph types
- Plan 08-04 can wire `lattice-api` session API against the narrow store contract
- All graph types are serializable with serde for SQLite JSON column storage

---
*Phase: 08-session-graph-core*
*Completed: 2026-05-16*
