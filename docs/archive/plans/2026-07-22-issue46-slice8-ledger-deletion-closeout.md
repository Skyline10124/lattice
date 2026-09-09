# Issue #46 Slice 8 — Replacement deletion and ledger closeout

## Goal

Finish the #45→#46 handoff only after every honest replacement is green, remove
superseded internal product-behavior tests, and leave local invariants intact.

## Preconditions

- Slices 0–7 pass independently in the ordinary lane without KVM, MSB daemon,
  OCI image, guest tooling, or workspace override.
- Each remaining `planned_direct_*` entry has an exact external test/assertion
  ID and an assertion-level superset checklist.
- The seven approved API-only entries are already reclassified to `keep` and
  their tests still exist.
- No test is deleted based on a family-level scenario or an approximation.

## Mechanical closeout

For every mapped row:

1. run its external replacement by exact test name;
2. compare old assertions against the replacement checklist;
3. delete only the mapped internal test body and orphaned fixture imports;
4. run the owning crate test file;
5. update machine ledger reason/replacement state without changing identity
   fields;
6. update `docs/TEST-TRIAGE.md` counts/status in the same commit.

Use symbol-based deletion because #45's historical line numbers have drifted.
Keep the ledger's fixed 1449-test identity snapshot; new harness tests are
replacement evidence, not additions to that historical enumeration.

## Required cleanup searches

Active code/test/docs (excluding historical ADR/FIX records where appropriate)
must contain no retired harness surface:

```text
require_msb_env
LATTICE_HARNESS_MSB
/dev/kvm
KVM lane
OCI image capability probe
Upper cleanup assertion
sandbox-only fixture
workspace override
```

Do not rewrite historical records. Do not modify `.gitignore`.

## Records

- Append an Issue #46 entry to `FIX.md` only after fixes/scenarios land,
  including verification and changed files.
- Remove/close the #46 replacement finding from `FINDINGS.md`; retain explicit
  residual findings for any approved local-only invariant or unsupported public
  behavior.
- Update affected crate `AGENTS.md` recent issues/next actions.
- Comment on GitHub #46 (formerly Codeberg #24) with scenario/deletion counts and gate results; do not
  close the issue unless the maintainer workflow requests it.

## Final verification

```bash
cargo +1.96.0 fmt --all --check
cargo +1.96.0 clippy --workspace --all-targets -- -D warnings
cargo +1.96.0 xtask harness-boundary
cargo +1.96.0 xtask lancet-boundary
LATTICE_BIN="$(pwd)/target/debug/lattice" cargo +1.96.0 test -p lattice-system-harness
LATTICE_BIN="$(pwd)/target/debug/lattice" cargo +1.96.0 test --workspace
cargo +1.96.0 xtask ci
```

Run the two-axis `/review` against the pre-issue-24 fixed point, apply findings,
re-run relevant gates, then commit all reviewed work to the current branch.
