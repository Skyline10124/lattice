# Issue #185 command outcome plan

## Scope and references

This plan implements GitHub #185 inside `lattice-system-harness`. It follows the
workspace rules in `AGENTS.md`, the test-layer and `LATTICE_BIN` contracts in
`docs/TESTING.md`, the harness process-group rules in
`crates/lattice-system-harness/AGENTS.md`, and ADR 0003's Direct-Local test
migration. The harness remains an external binary consumer and gains no product
crate dependency. GitHub #40 remains open for maintainer acceptance.

## Current interface and caller inventory

`src/driver.rs` currently returns `CommandOutcome { output: Output,
termination: Termination }`. `Termination` mixes four different facts:
`Exited(Option<i32>)`, timeout, requested interrupt, and forced kill. A signal is
therefore `Exited(None)`, an interrupt can hide the observed exit, and an
interrupt followed by timeout loses the timeout and forced kill. Group-signal,
direct-child kill, and wait errors are discarded. The outcome never reports a
surviving process-group member.

The complete consumer set is:

- `src/driver.rs` constructs outcomes in `RunningCommand::{wait,kill_and_wait}`
  and exposes them through `LatticeDriver::{run,help,bare_invoke}`.
- `src/lib.rs` re-exports both current types.
- `src/assert.rs` accepts only `std::process::Output`; its exact-code helper maps
  a signal to magic value `-1` and cannot see timeout, interrupt, forced
  cleanup, cleanup errors, or survivors.
- `src/artifacts.rs` records only the old `Termination` beside stdout/stderr.
- `src/cli.rs`, `tests/fast_audit_cli.rs`, and
  `tests/fast_config_cli.rs` discard `CommandOutcome` and return its `Output`.
  Their downstream assertion callers are `fast_cli_help`, `fast_audit_cli`,
  `fast_config_cli`, `fast_env_boundary`, `fast_lancet_cli`, and
  `fast_session_cli`.
- `src/session_send.rs` consumes the old enum while creating a session and
  projects `Output.status.code()` while parsing sends. `SendExecution` and
  `SessionSendDriver::run_command` expose `CommandOutcome`.
- `tests/support/mod.rs` checks `Termination::Exited`, records outcomes for
  artifacts, and has another CLI helper that discards the outcome. The helper
  feeds `direct_local_turn`, `direct_tools`, `direct_turn_contract`,
  `direct_operator`, `release_operator`, and `direct_rewind`.
- `direct_crash_barriers` and `direct_rewind_crash_barriers` assert
  `Termination::Killed`. `direct_rewind_crash_barriers` also reads raw status.
  `direct_secrets` reaches through `command.output`.
- `ScenarioEvidence::record_command`, reached through `finish_scenario`, records
  outcomes from `direct_failures`, `direct_local_turn`, `direct_mutation`,
  `direct_observation`, `direct_operator`, `direct_rewind`, `direct_secrets`,
  `direct_session_isolation`, `direct_tools`, `direct_turn_contract`,
  `direct_workspace_history`, and `release_operator`.

Plain `std::process::Output` values created for test-side `git` commands in
`direct_rewind` and `direct_workspace_history` are outside this interface and
stay unchanged.

## One replacement model

Keep `CommandOutcome` as the sole command result and replace, rather than layer
beside, `Termination`:

- `ProcessTermination` is `Exited(i32)` or `Signaled(i32)`. The value is absent
  only when a cleanup wait failed before the harness could observe either fact.
- `HarnessIntervention` is an ordered list containing any of
  `InterruptRequested`, `TimedOut(Duration)`, and `ForcedKill`. An empty list
  means no intervention. A timeout after interrupt records all three events.
- `ProcessGroupCleanup` records whether cleanup was forced, every typed
  group-signal/direct-child-kill/direct-child-wait error, and the final sorted
  surviving PID list. Success means no errors and no survivors. Forced cleanup
  remains a fact even when it succeeds.
- `CommandOutcome` owns stdout, stderr, the optional observed process
  termination, interventions, and cleanup. It exposes small exit-code,
  signal, and clean-exit queries. It does not infer a panic from a signal or
  nonzero exit because the product supplies no panic marker.

The raw `std::process::ExitStatus`, old `Termination`, and `Deref<Output>` do not
remain as competing authorities after migration.

## Compatibility cutover and consumer order

1. Replace outcome construction in `driver.rs` while keeping the
   `CommandOutcome` name and `LatticeDriver`/`RunningCommand` method signatures.
   During the local edit, stdout/stderr and exit accessors provide the narrow
   compatibility path needed by callers. No second public termination model is
   committed.
2. Move exit assertions and failure artifacts to `CommandOutcome`. Exact exit
   means a matching `Exited(code)`, no intervention, no forced cleanup, no
   cleanup error, and no survivor. Signals, wrong codes, and every contaminated
   outcome fail.
3. Migrate `src/cli.rs`, `src/session_send.rs`, and `tests/support/mod.rs` so
   they retain the complete outcome. Then migrate the fast CLI wrappers.
4. Migrate direct, release, secret, and crash-barrier consumers. Replace every
   raw status check on a harness command with the authoritative clean-exit
   query or assertion helper. Test-side Git outputs remain raw `Output`.
5. Remove the old enum/re-export, `Output` field, and deref compatibility. A
   final source search must find no `Termination`, `Exited(None)`, harness
   `.output`, or harness `.status` consumer.

## Deterministic cases and vertical-slice TDD

Each slice starts with its named test failing against the current model, then
adds only enough implementation to pass.

1. Add `exit_records_code_without_intervention_or_forced_cleanup`. A shell exits
   7. The outcome records `Exited(7)`, no intervention, successful unforced
   cleanup, and no survivors.
2. Add `signal_records_concrete_signal_without_magic_exit_code`. A shell sends
   itself `SIGTERM`. The outcome records `Signaled(SIGTERM)` and no exit code.
3. Extend `timeout_retains_partial_output_and_cleans_process_group` without
   renaming it. It keeps its current byte and empty-group assertions, then adds
   observed `SIGKILL`, timeout plus forced-kill interventions, forced successful
   cleanup, and an empty survivor list.
4. Extend `interrupt_reaches_and_reaps_the_process_group` without renaming it.
   It keeps the current group-reap assertion and adds the observed child exit,
   interrupt request, forced descendant cleanup, and clean survivor evidence.
5. Add `interrupt_timeout_preserves_both_interventions_and_forced_kill`. A shell
   and child ignore `SIGINT`; a bounded wait then records interrupt, timeout,
   forced kill, observed `SIGKILL`, and successful forced cleanup.
6. Add `kill_and_wait_records_forced_kill`. The explicit crash-barrier path
   records observed `SIGKILL`, `ForcedKill`, forced cleanup, and no survivors.
7. Add `cleanup_failures_are_retained_without_an_invented_process_status`. The
   test reaps its child with test-only `waitpid` before `kill_and_wait`, making
   group signal, direct kill, and wait fail deterministically. The outcome keeps
   all typed errors and leaves process observation absent.
8. Add `exact_exit_code_accepts_only_a_matching_uncontaminated_exit` and
   `exact_exit_code_rejects_signals_and_contamination` in `assert.rs`. The table
   covers wrong code, signal, interrupt, timeout, forced kill, forced cleanup,
   each cleanup error class, and a synthetic surviving PID. This is a pure
   assertion test and leaves no process behind.
9. Add `scenario_evidence_records_complete_command_outcome` in `artifacts.rs`.
   A synthetic contaminated outcome proves the persisted command artifact keeps
   the signal, every intervention, forced cleanup, cleanup errors, and surviving
   PIDs.

No existing test is deleted or moved. The four current driver test identities
remain. The two process tests named above gain stricter assertions; the
own-process-group and child-PGID guard tests stay unchanged.

## Assertion-superset proof

Before removing the old enum, run the existing timeout and interrupt tests with
replacement assertions in place. The timeout test still proves partial stdout,
partial stderr, and an empty process group. It additionally proves the concrete
signal, both timeout/kill facts, cleanup success, and no surviving PID. The
interrupt test still proves delivery and an empty process group. It additionally
proves the observed child result, interrupt fact, forced descendant cleanup,
and cleanup diagnostics. The new exit, signal, explicit kill, interrupt-timeout,
cleanup-failure, exact-exit, and artifact tests cover states the old tests could
not represent. Since no old test is deleted or moved, the replacement set is a
strict assertion superset.

## Verification

Run from the issue worktree root:

```bash
cargo test -p lattice-system-harness --lib driver::tests -- --nocapture
cargo test -p lattice-system-harness --lib assert::tests -- --nocapture
cargo test -p lattice-system-harness --lib artifacts::tests -- --nocapture
cargo test -p lattice-system-harness --all-features --no-run
cargo build -p lattice-cli --bin lattice
LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness
cargo xtask harness-boundary
cargo fmt --all --check
cargo clippy -p lattice-system-harness --all-targets -- -D warnings
```

After implementation, search all harness Rust sources for the retired model and
lossy projections. Append one #185 entry to `FIX.md` only after the code and
tests pass. Do not change `FINDINGS.md` unless implementation reveals a genuine
residual. Do not restore the retired test-triage files.
