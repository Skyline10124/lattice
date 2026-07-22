# Issue #24 Slice 7 — Public session graph isolation

## Goal

Replace five internal CLI graph-operation tests with stronger two-session
external-binary scenarios using only public commands.

## Setup

Create sessions A and B through `session new`, seed each with at least two real
completed Turns, and capture public commit IDs from JSONL/session log. Give both
sessions the same branch names so accidental global ref resolution is visible.

## Scenarios

1. `planned_direct_branch_listing_isolation`: create `feature` in both sessions
   at different commits; listing A reports only A's pointer and changes neither.
2. `planned_direct_checkout_isolation`: checkout A `feature`; A HEAD changes,
   B remains on `main` with the same commit.
3. `planned_direct_branch_create_isolation`: create A `work --from <A0>`; A has
   the exact pointer, B has no `work`.
4. `planned_direct_reset_isolation`: reset A's current branch to A0; B's branch
   and current commit are unchanged.
5. `planned_direct_diff_isolation`: identical `main`/`feature` names resolve
   within the addressed session; A/B feature commit IDs differ and A's diff
   reports only A's transcript changes.

## Assertion helper

```rust
let before_b = cli_json(["session", "status", b])?;
let changed_a = cli_json(["session", "checkout", a, "feature"])?;
let after_a = cli_json(["session", "status", a])?;
let after_b = cli_json(["session", "status", b])?;
assert_eq!(after_b, before_b);
assert_eq!(after_a["head"]["Branch"]["name"], "feature");
```

Every command is step/total bounded and every stdout/stderr is sentinel-scanned.
Do not inspect `sessions.git` or session sidecars directly.

## Deletion gates

Run each named external scenario independently. Only then delete the exactly
mapped `session.rs` internal test and update both triage ledgers. Parser/render
unit tests remain.

## Files

- `crates/lattice-system-harness/tests/direct_session_isolation.rs` (new)
- generic JSON CLI helper in harness only after the second caller proves reuse
- `crates/lattice-cli/src/commands/session.rs`
- `docs/{TEST-TRIAGE.md,test-triage-decisions.json}`

## Verification

```bash
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness \
    --test direct_session_isolation -- --test-threads=1
cargo +1.96.0 test -p lattice-cli session
```
