# Issue #185 command-outcome follow-up plan

## Scope and references

This follow-up repairs three residuals confirmed after PR #209 merged. It follows
root `AGENTS.md`, `docs/TESTING.md`, the harness rules in
`crates/lattice-system-harness/AGENTS.md`, and the original
`docs/plans/2026-09-01-issue185-command-outcome.md`. It does not restore the old
termination model or add another outcome type.

The separate fast failure-bundle omission remains owned by GitHub #192. That
work will add one metadata-only outcome summary to the existing bundle path; it
is not duplicated here.

## Changes

1. Make stdout and stderr capture completeness explicit. A drain stopped before
   EOF returns its bytes with `complete = false`. `clean_exit_code` rejects an
   otherwise-successful outcome when either stream is incomplete. Keep one
   capture type shared by stdout and stderr; do not add descendant tracking,
   pidfds, a subreaper, or a larger drain grace period.
2. Change `SessionSendError::NewSessionFailed` to retain the complete
   `CommandOutcome`, whose `Debug` implementation is already metadata-only.
   Callers can distinguish product exit, signal, harness intervention, cleanup
   error, and surviving PIDs without copying summary fields.
3. Treat cleanup `SIGKILL` `ESRCH` as benign only after one immediate
   process-group inspection confirms the group is empty. Keep every other
   signal error. Do not apply the exception to requested `SIGINT`.

## TDD seams

- `driver::tests`: an escaped pipe writer forces a stopped drain and proves that
  captured bytes remain available while exact exit is rejected.
- `session_send::tests`: a synthetic signaled/contaminated outcome survives
  inside `NewSessionFailed`.
- `driver::tests`: a pure cleanup helper receives `ESRCH` plus either an empty or
  non-empty recheck and records only the non-benign case.

## Verification

```bash
cargo test -p lattice-system-harness --lib driver::tests -- --nocapture
cargo test -p lattice-system-harness --lib session_send::tests -- --nocapture
cargo test -p lattice-system-harness --all-features --no-run
cargo fmt --all --check
cargo clippy -p lattice-system-harness --all-targets -- -D warnings
cargo xtask harness-boundary
LATTICE_BIN="$PWD/target/debug/lattice" cargo test -p lattice-system-harness
```

Append the applied repair to `FIX.md`. Keep the deferred fast-artifact gap in
`FINDINGS.md` under #192. Leave the worktree unstaged, uncommitted, and unpushed
for direct review.
