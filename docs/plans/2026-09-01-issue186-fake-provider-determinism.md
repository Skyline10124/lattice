# Issue #186 deterministic fake provider plan

## Scope and references

This plan implements GitHub #186 in `lattice-system-harness`. It follows the
external-binary boundary from GitHub #41, the Direct-Local test architecture in
GitHub #40 and ADR 0003, the layer and `LATTICE_BIN` rules in
`docs/TESTING.md`, and the bounded process and fixture rules in
`crates/lattice-system-harness/AGENTS.md`. The fake provider remains a localhost
test fixture reached through `OPENAI_COMPATIBLE_*`. It does not add a product
API, a second provider model, or a general HTTP server.

GitHub #185's `CommandOutcome` model remains unchanged. GitHub #192 continues
to own combined assertion, secret-leak, evidence, and cleanup failures. This
issue does not change artifact ownership or failure precedence. GitHub #40
stays open for maintainer closeout.

## Current fake-provider tests and caller inventory

`crates/lattice-system-harness/src/fake_llm.rs` owns eight local tests:

- `render_plain_text_script_matches_openai_shape`
- `render_tool_call_script_emits_start_args_and_tool_calls_finish`
- `render_disconnect_script_omits_terminal_chunk_and_done`
- `render_illegal_response_script_emits_non_json_data`
- `parse_body_surfaces_model_roles_and_tool_names`
- `parse_body_records_raw_text_when_not_json`
- `server_captures_request_and_returns_plain_text_sse`
- `gated_response_waits_for_release_and_shutdown_joins_handlers`

The shared integration callers are `tests/support/mod.rs` functions
`wire_fake_llm`, `wire_logged_fake_llm`, and `wire_gated_fake_llm`. The public
harness caller `src/tui_mock.rs::TuiMockScenario::start_server` uses ordinary
and gated servers; `tui_mock::tests::every_scenario_starts_a_local_server`
checks that construction path. `src/lib.rs` re-exports `CapturedRequest`,
`FakeLlmServer`, `ResponseGate`, `ServeError`, and `SseScript`.

The exact external test callers, grouped by target, are:

- `direct_failures`: `direct_failure_http_error_no_retry_commits_error`,
  `direct_failure_disconnect_before_output_commits_error`,
  `direct_failure_disconnect_after_partial_token_commits_error`,
  `direct_failure_malformed_sse_commits_error`,
  `direct_failure_sigint_while_stalled_commits_cancelled`,
  `direct_failure_profile_deadline_while_stalled_commits_error`,
  `direct_failure_mutation_survives_provider_failure`, and
  `direct_failure_session_lock_serializes_same_session_turns`.
- `direct_local_turn`: `direct_local_plain_text_turn_commits_and_exit_zero`,
  `direct_local_session_reopen_after_external_change`, and
  `direct_local_mutate_create_denied_by_write_policy`.
- `direct_mutation`: `direct_mutation_create_atomic_and_existing_target`,
  `direct_mutation_edit_hash_and_anchor`,
  `direct_mutation_stale_edit_and_external_visibility`,
  `direct_mutation_no_rollback_after_provider_failure`,
  `direct_mutation_denials_have_no_side_effect`,
  `direct_mutation_concurrent_same_target_create_has_one_winner`, and
  `direct_mutation_tool_activity_before_commit`.
- `direct_observation`: `direct_observation_visibility_and_bounds`.
- `direct_operator`: `direct_operator_fetch_terminal_contract`,
  `direct_operator_component_terminal_contract`,
  `direct_operator_mixed_query_tail_replay_and_status`,
  `direct_operator_status_bounds_twenty_newest_invocations`,
  `direct_operator_unknown_typed_session_is_empty`, and
  `direct_operator_unknown_typed_turn_fails`.
- `direct_rewind`: `direct_rewind_reverses_later_mutations_and_preserves_external_changes`
  and `direct_rewind_conflict_moves_nothing`.
- `direct_secrets`: `direct_secrets_sentinels_never_leak_into_public_sinks` and
  `direct_secrets_failure_bundle_redacts_sentinels`.
- `direct_session_isolation`: `direct_session_branch_listing_isolation`,
  `direct_session_checkout_isolation`, `direct_session_branch_create_isolation`,
  `direct_session_reset_isolation`, and `direct_session_diff_isolation`.
- `direct_tools`: `direct_tools_fetch_web_localhost_success_and_cap`,
  `direct_tools_fetch_disabled_no_dispatch`,
  `direct_tools_fetch_host_policy_denial`,
  `direct_tools_fetch_capability_denial_with_public_https`,
  `direct_tools_todo_write_success`,
  `direct_tools_todo_write_validation_rejection`,
  `direct_tools_todo_write_result_survives_session_reopen`, and
  `direct_tools_catalog_exposure_matches_enabled`.
- `direct_turn_contract`: `direct_turn_completed_reopen_contract`.
- `direct_workspace_history`: `direct_workspace_history_records_installed_mutation`
  and `direct_workspace_history_records_nothing_for_a_failed_mutation`.
- `pty_tui_smoke`: `pty_tui_smoke_runs_turn_and_quits_cleanly`.
- Feature-gated `direct_crash_barriers`:
  `turn_started_sigkill_reopens_without_replaying_the_provider`,
  `next_model_attempt_sigkill_recovers_the_byte_exact_cumulative_transcript`,
  `post_mutation_step_closed_sigkill_reconciles_with_current_history_checkpoint`,
  `post_mutation_turn_closing_sigkill_preserves_the_current_history_checkpoint`,
  `model_and_binding_windows_recover_without_provider_replay`,
  `result_terminal_and_cursor_windows_recover_without_redispatch`, and
  `every_multi_tool_cursor_recovers_without_redispatch`. The latter matrix
  cases share `run_barrier_case`.
- Feature-gated `direct_rewind_crash_barriers`:
  `rewind_action_to_audit_windows_survive_sigkill`, through `run_case`.
- Feature-gated `release_operator`:
  `release_operator_component_matrix`, through `release_fetch_case`,
  `release_component_case`, and `release_mixed_case`.

No caller receives a new product semantic. The only external caller edits will
replace timing guesses with the fake provider's bounded lifecycle handshakes.

## Accepted HTTP subset

The fixture accepts one request per TCP connection with these exact rules:

- Request line bytes end in `\r\n` and contain exactly three single-space
  separated fields. Method is `POST`, target is `/chat/completions` with no
  query or fragment, and version is `HTTP/1.1`.
- Headers use `\r\n`, ASCII token names, a colon, and ASCII values without
  control bytes or obsolete folding. Names are case-insensitive. `Host` is
  present exactly once and non-empty. `Content-Type` is present exactly once
  and equals `application/json` case-insensitively. Other syntactically valid
  request headers are ignored.
- Exactly one decimal `Content-Length` is required. Empty, signed, non-decimal,
  duplicate, or conflicting values are rejected. Any `Transfer-Encoding` is
  rejected before body allocation.
- `Connection` is not a request policy input. The fixture reads one bounded
  request, sends one response with `Connection: close`, and closes the socket.
  It does not inspect future bytes or claim to detect keep-alive or pipelining.
- Request headers, including the final `\r\n\r\n`, are capped at 16,384 bytes.
  The reader checks the cap before growing the accumulator. The declared body
  is capped at 1,048,576 bytes. The fixture parses and checks the decimal length
  before allocating the body and rejects an oversize declaration without
  reading the body.
- Socket reads and writes retain the fixed two-second deadline. An incomplete
  request therefore cannot hold a handler forever.
- The body must be UTF-8 before JSON capture. Invalid UTF-8 is rejected and is
  never converted with `from_utf8_lossy`. Valid UTF-8 that is not JSON keeps
  the existing `__raw_body` capture behavior because scenario diagnostics own
  that contract.

Protocol rejections use fixed ASCII bodies that never quote rejected bytes.
Each response is byte-exact `HTTP/1.1`, uses only `\r\n` in the HTTP head,
includes `Content-Type`, byte-counted `Content-Length`, and
`Connection: close`, then sends the stated body once. Scripted `HttpError`
responses use the same response writer, so a non-ASCII JSON body proves that
`Content-Length` counts bytes rather than characters. Unsupported method,
route, version, framing, length, UTF-8, and size cases have stable status/body
pairs asserted as raw bytes.

This is the whole HTTP contract. There is no chunk decoder, keep-alive loop,
pipeline queue, route table, HTTP/2 support, or generic request framework.

## One capture and Sequence transaction

Replace the separate capture and script locks with one shared state lock. Once
a request passes HTTP validation and body decoding, one critical section:

1. chooses and advances the request's `Sequence` script,
2. appends that request to capture order, and
3. publishes the request-arrived condition.

The handler carries its chosen script after releasing the lock. A gate may
stall delivery, but cannot reorder script ownership. The order visible from
`captured_requests()` is therefore the same order used to assign Sequence
entries, even when handlers resume concurrently. Out-of-range requests retain
the current last-script repeat behavior, and an empty sequence still fails
loudly through `IllegalResponse`.

## Bounded lifecycle and failure signals

Use `Mutex`, `Condvar`, `TcpStream`, and thread handles from `std`; add no
dependency.

- `wait_for_requests(count, timeout)` remains the request-arrived handshake.
  It returns early if a server or handler failure makes the requested count
  impossible.
- `ResponseGate::wait_until_stalled(count, timeout)` proves handlers reached
  the intentional stall. `ResponseGate::release(timeout)` wakes them and waits
  for a bounded acknowledgement that current stallers left the gate.
- Explicit `FakeLlmServer::shutdown(timeout)` sets shutdown, cancels a gate,
  wakes blocking `accept` with one loopback connection, waits for serve-loop
  and handler completion through a condition variable, and joins only after
  completion is published. `Drop` uses one fixed bounded fallback and never an
  unbounded join.
- Serve-loop errors, handler I/O errors, and caught thread panics retain only
  the first fixture failure. A request wait returns it when the requested capture
  becomes impossible; explicit shutdown returns it after bounded teardown. There
  is no independent health query or failure subscription API.
- Fake-provider external synchronization does not depend on fixed sleeps.
  Direct SIGINT, session-lock, and mutation-concurrency callers wait for request
  or stall readiness before interrupt or release. The deadline owner regression
  uses local bounded timing only to place process wait after the spawn deadline.
- `RunningCommand` measures its per-command timeout from process spawn. Provider
  request, stall, and release waits cap themselves to the same remaining budget;
  the final process wait cannot start a fresh timeout.

## Vertical-slice TDD and red tests

Each slice starts by running its named test against the preceding code, records
the expected failure, then adds only the shared-root implementation needed for
that slice.

1. Extend `server_captures_request_and_returns_plain_text_sse` to assert the
   valid `POST /chat/completions HTTP/1.1` path with raw UTF-8 conversion, not
   lossy conversion.
2. Add `request_contract_accepts_only_post_chat_completions_http_1_1`. Its raw
   byte table checks exact method, route, malformed request-line, and version
   rejection responses.
3. Add `request_contract_requires_single_bounded_content_length`. It checks
   missing, malformed, duplicate, conflicting, and oversize declarations, and
   proves oversize rejection without sending a body.
4. Add `request_contract_rejects_unsupported_framing_and_invalid_utf8`. It
   checks chunked transfer, bare-LF framing, invalid header bytes, and invalid
   body UTF-8 through exact raw responses. Connection intent and future bytes
   are intentionally outside the fixture contract.
5. Add `request_contract_rejects_oversize_headers_before_terminator`. It sends
   a header beyond 16,384 bytes without a terminator and receives a bounded,
   exact rejection.
6. Add `http_error_responses_have_byte_exact_crlf_and_content_length`. It checks
   both a protocol rejection and a scripted non-ASCII JSON `HttpError` body.
7. Add `concurrent_sequence_keeps_capture_and_response_assignment_paired`.
   Many uniquely named requests enter a shared gate in known capture order.
   Each has a unique Sequence response. After one release, every client's
   response must match the Sequence position of its captured request. Repeat
   this test after green to catch order flakes.
8. Strengthen the existing
   `gated_response_waits_for_release_and_shutdown_joins_handlers`. It first
   observes request arrival and stall readiness, proves no response with a
   nonblocking channel check, uses bounded release acknowledgement, and uses
   explicit bounded shutdown to prove listener release and gate cancellation.
9. Add `shutdown_returns_first_fixture_failure`. A logged server is given an
   unwritable log target, then bounded shutdown returns the retained handler
   failure after joining teardown.
10. Add `timeout_includes_time_before_wait`. A child is held past its absolute
    spawn deadline before `wait` begins; work released after that deadline must
    not run. This pins the shared command budget without a wall-clock threshold.

After the local owner tests are green, replace the 400 ms synchronization sleep
in `direct_failure_sigint_while_stalled_commits_cancelled` with
`wait_until_stalled`. Apply the same replacement to the request/release waits in
`direct_failure_session_lock_serializes_same_session_turns` and
`direct_mutation_concurrent_same_target_create_has_one_winner`. Product outcome
and workspace assertions remain byte-for-byte in place.
Those handshakes use `RunningCommand::remaining_timeout`, so they consume the
same command budget established at spawn.

## Replacement-first and assertion ownership

No existing test is deleted or moved. The eight current `fake_llm` identities
remain. The valid-server and gated-lifecycle tests gain stricter assertions,
and the seven new fake-provider tests own protocol rejection, bounds, atomic
Sequence assignment, and thread-failure observability. The driver regression
owns the spawn-to-wait deadline.

The local tests are the sole owners of fake-provider HTTP and concurrency
semantics. `direct_failures` continues to own product provider failure, SIGINT,
deadline, and Session execution-lock behavior. `direct_mutation` continues to
own the Local mutation race. Their replacement-first evidence is:

- keep every existing product event, exit, cleanup, and persistence assertion;
- first make request, stall, release, and shutdown handshakes pass locally;
- then replace only `thread::sleep` or ignored wait results in external tests;
- run the exact external test before and after replacement.

This is an assertion superset. Existing product checks remain, while the setup
now proves the request reached the intended stall instead of guessing from
elapsed time. No #192 artifact assertion is copied into this module.

## Verification

Run from `/home/astrin/Dev/lattice-worktrees/issue-186`:

```bash
cargo test -p lattice-system-harness --lib fake_llm::tests -- --nocapture
for run in $(seq 1 50); do
  cargo test -p lattice-system-harness \
    fake_llm::tests::concurrent_sequence_keeps_capture_and_response_assignment_paired \
    -- --exact
 done
cargo test -p lattice-system-harness --all-features --no-run
cargo build -p lattice-cli --bin lattice
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --test direct_failures
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness
cargo xtask harness-boundary
cargo fmt --all --check
cargo clippy -p lattice-system-harness --all-targets -- -D warnings
```

The focused test command must report the named tests, not a zero-test filter.
The Direct-Local commands must use the absolute binary built in this worktree.
After the fix, search harness Rust sources for `from_utf8_lossy` in fake-provider
protocol evidence, fake-provider synchronization sleeps, swallowed serve-thread
errors, and separate capture/script locks.

Append one GitHub #186 entry to `FIX.md` only after the implementation and tests
pass. Change `FINDINGS.md` only if a real #186 residual remains. Do not restore
the retired triage files.
