# Issue #24 Slice 1 — Completed Turn contract and ledger correction

## Goal

Expand the existing plain/reopen scenarios into the public completed-Turn
contract, and correct #23 mappings that cannot honestly be elevated through the
binary seam.

## Approved ledger correction

The user approved retaining and reclassifying API-only invariants. Change these
seven machine/prose ledger entries from `elevate/direct_local_binary` to
`keep/local_invariant`; do not delete their tests:

1. `streaming_backward_compat_fake_llm_chat_only_still_works`
2. `stream_turn_live_with_agent_receiver_drop_does_not_cancel`
3. `stream_turn_live_with_agent_slow_observer_still_commits`
4. `consistency_error_after_partial_token`
5. `consistency_tool_call_path`
6. `consistency_completed_disposition_parity`
7. `consistency_error_disposition_parity`

The reason must state that trait fallback, API observer behavior, and
`run_turn`/collector parity have no faithful external binary trigger.

## External assertions

Use separate named tests/assertion blocks for the remaining exact IDs, even when
they share one world:

- message order from prior transcript + current user input;
- first event `turn_started`, routed provider/model, branch population;
- token/model activity before `turn_committing` and `turn_committed`;
- matching started/committed session IDs, nonempty commit ID, completed status;
- no failed/cancelled/commit-failed events on success;
- second request contains exactly the prior user/assistant pair before the new
  user message;
- session log/status report both commit IDs, count two, HEAD `main`;
- first-turn content remains unchanged after the second delta commit;
- missing session exits 2 with no JSONL Turn events.

## Code example

```rust
assert_event_subsequence(
    &turn,
    &["turn_started", "model_started", "token_delta",
      "model_finished", "turn_committing", "turn_committed"],
)?;
assert_eq!(turn.started_session_id(), turn.committed_session_id());
assert_eq!(status["commit_count"], 2);
assert_eq!(second_capture.message_contents(), [
    "first prompt", "first answer", "second prompt",
]);
```

Do not assert a full event golden. Unknown additive events are allowed between
required events.

## Deletion gates

After the external tests pass, delete only the mapped internal tests whose
observable assertions are strict subsets. Keep commit-object representation
assertions if public session/captured-provider evidence cannot prove them.
Update `docs/TEST-TRIAGE.md` and the machine ledger atomically with each final
judgment.

## Files

- `crates/lattice-system-harness/tests/direct_turn_contract.rs` (new)
- `crates/lattice-system-harness/tests/direct_local_turn.rs` (split/remove duplication)
- `crates/lattice-agent/tests/{agent_loop,streaming_delta}.rs`
- `crates/lattice-api/src/turn/live_tests.rs`
- `crates/lattice-api/tests/consistency.rs`
- `crates/lattice-api/tests/usage_turn.rs`
- `crates/lattice-cli/src/commands/run_tests.rs`
- `crates/lattice-cli/tests/release_smoke.rs`
- `docs/{TEST-TRIAGE.md,test-triage-decisions.json}`

## Verification

```bash
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_turn_contract
cargo +1.96.0 test -p lattice-agent
cargo +1.96.0 test -p lattice-api
cargo +1.96.0 test -p lattice-cli
```
