# FIX — Changelog

Applied fixes across code review phases. Entries are append-only; never delete or overwrite.


## GitHub #192 PR #212 review: close unchecked artifact write paths — 2026-09-01

**Severity:** HIGH (unchecked destructive test-harness API / stale failure false diagnosis)

**Problem:** Review found that `artifacts::write_failure_artifacts` remained a public raw writer even
after the new finalizers stopped using it. External harness callers could construct arbitrary bundle
data, delete an arbitrary target directory, and write without name validation, a claim, redaction, or
`FailureReport`. `ScenarioEvidence::new` also accepted an arbitrary `PathBuf`; validating only its
basename did not prove that the target came from the configured failure base. Finally, no owner test
proved that a successful Fast rerun removes an older same-name failure bundle while preserving the
`.claims` namespace.

**Fix:** Deleted the zero-call public writer; made `FailureArtifacts` and `capture_from_output`
crate-private; and removed the dead `AssertError::ArtifactIo`, `EvidenceError::{Scan,
PostWriteSentinel}`, and `verify_persisted_bundle` surfaces. Public `ScenarioEvidence::new` now accepts
only a scenario name and resolves it through the same checked failure-base builder as `failure_dir`;
module tests retain one private `new_at_dir` seam for `TempDir` injection. The Fast finalizer has a
private target seam, and `fast_success_clears_stale_bundle_without_writing_new_evidence` proves that a
successful rerun removes the stale scenario bundle without deleting `.claims` or its persistent lock
file. Crate guidance now records the Direct recorder's construction-to-finish/drop claim lifetime.

**Verification:**

- `scenario_evidence_rejects_arbitrary_target_paths` failed before the constructor fix, then passed.
- Both new focused owner tests — PASS (2/2).
- `cargo test -p lattice-system-harness --lib -- --nocapture` — PASS (101/101).
- `cargo test -p lattice-system-harness --all-features --no-run` — PASS.
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — PASS.
- `cargo fmt --all --check`, `cargo xtask harness-boundary`, and the debug CLI build — PASS.
- `LATTICE_BIN="$PWD/target/debug/lattice" cargo test -p lattice-system-harness --test
  direct_secrets -- --nocapture` — PASS (2/2).
- The ordinary harness passed with only the pre-existing
  `direct_operator_status_bounds_twenty_newest_invocations` timeout owner explicitly skipped; every
  remaining Fast, Direct-Local, and PTY target passed.

**Changed files:** `crates/lattice-system-harness/src/{artifacts,assert}.rs`,
`crates/lattice-system-harness/tests/support/mod.rs`,
`crates/lattice-system-harness/AGENTS.md`,
`docs/plans/2026-09-01-issue192-failure-artifact-contract.md`, and `FIX.md`.

## GitHub #192: preserve complete, redacted failure reports (2026-09-01)

**Severity:** HIGH (primary assertion loss, incomplete secret evidence, concurrent artifact corruption)

**Problem:** Fast assertions and Direct scenario evidence used separate first-error-wins persistence
paths. A write, cleanup, scanner error, or post-write secret hit could replace the original
assertion. The scanner discarded leaks found before a later I/O error, fast `command.txt` omitted
the existing `CommandOutcomeSummary`, and same-name writers could clean and mix one another's
bundles. Artifact names were joined without a component check, and failure-only workspace snapshots
followed symlinked directories.

**Fix:** Added one `FailureReport` that displays sorted safe leak locations, the redacted primary
assertion, shared metadata-only command diagnostics, and every evidence error in a fixed order.
Fast and Direct paths now use the same command renderer and collecting writers. The scanner records
a leak before patching, continues across reachable siblings, and returns all scan errors to the new
finalizers while the public adapter keeps its first-error signature.

Failure targets now reject empty, absolute, multi-component, dot, NUL, and reserved `.claims`
names. Each writer takes a Linux nonblocking `flock` on the persistent
`<base>/.claims/<scenario>.lock` file before stale cleanup and holds it through scanning. Workspace
snapshots record symlinks as leaves with `symlink_metadata`. The nested compatibility bundle in
`direct_secrets` uses a distinct target because its Direct recorder already holds the scenario
claim. `M-GH185-01` was removed after fast `command.txt` gained termination, intervention, cleanup,
survivor, byte-count, and completeness facts.

**Verification:**

- Assertion and artifact unit suites: PASS (5/5 and 29/29).
- `cargo test -p lattice-system-harness --all-features --no-run`: PASS.
- `cargo fmt --all --check`: PASS.
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings`: PASS.
- `cargo xtask harness-boundary`: PASS.
- `cargo build -p lattice-cli --bin lattice`: PASS.
- Full harness with `LATTICE_BIN="$PWD/target/debug/lattice"`: PASS (100 library tests and every
  ordinary fast, Direct-Local, and PTY target).
- `env -u LATTICE_BIN cargo xtask ci`: PASS.

**Changed files:** `crates/lattice-system-harness/src/{artifacts,assert,lib}.rs`,
`crates/lattice-system-harness/tests/{support/mod.rs,direct_secrets.rs}`,
`crates/lattice-system-harness/AGENTS.md`, `docs/TESTING.md`,
`docs/plans/2026-09-01-issue192-failure-artifact-contract.md`, `FINDINGS.md`, and `FIX.md`.


## GitHub #185 PR #211 review: make drain completeness deterministic — 2026-09-01

**Severity:** HIGH（PR-introduced CI failure / output-completeness false red）

**Problem:** The first completeness regression launched `setsid sh` and relied on a detached process
remaining alive with an inherited stdout FD. GitHub Actions did not preserve that shell/process
behavior, so the PR's new `assert!(!outcome.stdout_complete)` failed. The implementation also inferred
completeness from `JoinHandle::is_finished()` at one scheduler instant instead of the drain reader's
actual EOF observation.

**Fix:** The drain thread now returns one private `DrainResult { bytes, reached_eof }`. `read == 0`
sets `reached_eof = true`; a stop-requested exit sets it false; a missing stream is complete.
`finish_drain` still uses the bounded thread wait for teardown, but derives completeness only from the
joined drain result. The regression now uses a local nonblocking `UnixStream::pair`: it writes
`prefix`, keeps the writer open so EOF cannot occur, finishes the drain, and closes the writer
explicitly. No shell, detached process, process-group behavior, or wall-clock performance threshold
remains.

**Verification:**

- Exact incomplete-drain regression — PASS（1/1; 20/20 repeated runs）。
- Driver tests — PASS（14/14）。
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — PASS。
- `cargo fmt --all --check`, all-features `--no-run`, and `cargo xtask harness-boundary` — PASS。
- The full ordinary harness with the absolute worktree binary — PASS（79 library tests and every
  ordinary fast, Direct-Local, and PTY target）。

**Changed files:** `crates/lattice-system-harness/src/driver.rs`,
`docs/plans/2026-09-01-issue185-command-outcome-followup.md`, and `FIX.md`.

## GitHub #185 PR #209 follow-up: preserve output completeness and cleanup facts — 2026-09-01

**Severity:** HIGH（exact-exit false green / command-outcome information loss）

**Problem:** PR #209's bounded drain stopped a pipe reader after 250 ms but returned only the bytes
collected so far. `CommandOutcome` could therefore report an uncontaminated exit 0 while stdout or
stderr completeness was unknown. `SessionSendError::NewSessionFailed` also reduced the complete
outcome back to `Option<i32>`. Finally, a descendant could exit between process-group inspection and
cleanup `SIGKILL`; the resulting benign `ESRCH` remained as a cleanup error even when an immediate
recheck proved the group empty.

**Fix:** `CommandOutcome` now records `stdout_complete` and `stderr_complete`. `finish_drain` returns
both bytes and whether EOF was observed before the bounded stop, the metadata-only summary includes
both flags, and exact-exit classification requires complete stdout and stderr. The regression uses an
escaped pipe writer to retain the already captured `prefix` bytes while proving that an interrupted
drain rejects clean exit.

`SessionSendError::NewSessionFailed` now owns a boxed complete `CommandOutcome`, preserving signal,
harness intervention, cleanup errors, survivors, bytes, and completeness without enlarging every
`Result` error slot or exposing raw output through diagnostics. Cleanup `SIGKILL` suppresses `ESRCH`
only when one immediate process-group recheck succeeds and returns empty; non-empty or failed
rechecks retain the error. Requested `SIGINT` behavior is unchanged.

**Verification:**

- The incomplete-drain regression failed before repair because `is_clean_exit(0)` returned true,
  then passed with `stdout_complete == false`.
- The `NewSessionFailed` type regression failed before repair because the variant exposed only
  `code`; it now retains the full outcome.
- The cleanup helper regression failed before repair because no ESRCH recheck existed.
- Driver tests — PASS（14/14）。
- Session-send tests — PASS（17/17）。
- Assertion tests — PASS（3/3）。
- Artifact tests — PASS（18/18）。
- `cargo fmt --all --check`, harness all-target Clippy with `-D warnings`, all-features `--no-run`,
  debug CLI build, and `cargo xtask harness-boundary` — PASS。
- The full ordinary harness with the absolute worktree binary — PASS（79 library tests and every
  ordinary fast, Direct-Local, and PTY target）。

**Changed files:** `crates/lattice-system-harness/src/{driver,session_send,assert,artifacts}.rs`,
`docs/plans/2026-09-01-issue185-command-outcome-followup.md`, `FIX.md`, and `FINDINGS.md`.

## GitHub #186 PR #210 final review wording cleanup — 2026-09-01

**Severity:** LOW（documentation and test-name precision）

**Problem:** The plan claimed that no lifecycle assertion used `thread::sleep`, although the deadline
owner regression uses a bounded sleep to place process wait after the spawn deadline. The fixture
failure test also said `first` despite constructing only one failure.

**Fix:** The plan now limits the no-sleep claim to fake-provider external synchronization and names
the deadline owner's local timing control. The test is now
`shutdown_returns_fixture_failure`; its behavior and coverage are unchanged.

**Verification:**

- `cargo test -p lattice-system-harness --lib fake_llm::tests::shutdown_returns_fixture_failure -- --exact` — PASS（1/1）。
- `cargo fmt --all --check` and `git diff --check` — PASS。

**Changed files:** `crates/lattice-system-harness/src/fake_llm.rs`,
`docs/plans/2026-09-01-issue186-fake-provider-determinism.md`, and `FIX.md`.

## GitHub #186 PR #210 scope reduction: keep bounded teardown, remove health polling — 2026-09-01

**Severity:** MEDIUM（test-fixture overreach）

**Problem:** The fake provider exposed independent `health()` and `wait_for_failure()` APIs and
stored every fixture failure in a vector. Their only consumer was a local test that queried the same
handler I/O error through failure wait, health, and shutdown. This turned a one-shot harness fixture
into a small service manager and created a false cross-ticket residual for #192/#196/#205.

**Fix:** Removed both polling APIs. `ServerState` now retains only `first_failure`, and every serve,
handler, spawn, accept, or join failure records only the first diagnostic. The required request wait
still fails early when a capture is impossible. Bounded `shutdown(timeout)` wakes accept, cancels the
gate, joins serve/handler threads, and returns the retained fixture failure after teardown. The local
failure test now asserts only that shutdown returns that error. The `M-GH186-01` residual was removed
from `FINDINGS.md`; #192/#196/#205 retain only their existing product/assertion scopes.

**Verification:**

- `cargo test -p lattice-system-harness --lib fake_llm::tests -- --nocapture` — PASS（15/15）。
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — PASS。
- `cargo fmt --all --check`, debug CLI build, and `cargo xtask harness-boundary` — PASS。
- The full ordinary harness with the absolute worktree binary — PASS（84 library tests and every
  ordinary fast, Direct-Local, and PTY target）。

**Changed files:** `crates/lattice-system-harness/src/fake_llm.rs`,
`docs/plans/2026-09-01-issue186-fake-provider-determinism.md`, `FINDINGS.md`, and `FIX.md`.

## GitHub #186 PR #210 review follow-up: use one command deadline and remove fake HTTP policy — 2026-09-01

**Severity:** HIGH（system-harness timeout false green / test-only protocol overreach）

**Problem:** `LatticeDriver::start` discarded `LatticeCommand.timeout`, while
`RunningCommand::wait` started a new timer only after provider request/stall handshakes. A nominal
30-second command could therefore spend up to 60 seconds waiting for a request and then receive a
fresh process-wait budget. The fake provider also tried to reject `Connection: keep-alive` and
pipelining with one immediate nonblocking `peek`; delayed extra bytes were accepted, so the claimed
contract was timing-dependent. Two owner tests additionally imposed unowned sub-second performance
thresholds.

**Fix:** `RunningCommand` now records process-spawn time and the `LatticeCommand` timeout supplied by
`LatticeDriver::start`. Intermediate provider request, stall, and release waits cap themselves to
`remaining_timeout`, and `wait_spawned` consumes the stored absolute deadline instead of creating a
new timeout. The regression `timeout_includes_time_before_wait` holds a child past its spawn deadline
and proves that work released afterward cannot run; it uses an observable child action rather than a
wall-clock threshold.

GitHub #186 now defines the smallest real fixture contract. The parser validates one bounded
`POST /chat/completions HTTP/1.1` request, rejects unsupported transfer framing and routes, sends one
`Connection: close` response, and closes the socket. It no longer interprets `Connection`, peeks for
future pipelined bytes, or tests either behavior. The unrelated `< 1s` assertions were deleted.
Server-health consumption by product-failure scenarios remains assigned to #192/#196/#205.

**Verification:**

- The new deadline regression failed before the fix because `after-deadline` reached stdout, then
  passed after the timer moved to process spawn.
- `cargo test -p lattice-system-harness --lib driver::tests::timeout_includes_time_before_wait --
  --exact --nocapture` — PASS（1/1）。
- `cargo test -p lattice-system-harness --lib fake_llm::tests -- --nocapture` — PASS（15/15）。
- `LATTICE_BIN="$PWD/target/debug/lattice" cargo test -p lattice-system-harness --test
  direct_failures` — PASS（8/8）。
- The exact concurrent mutation witness — PASS（1/1）。
- `cargo fmt --all --check`, harness all-target Clippy with `-D warnings`, all-features `--no-run`,
  and `cargo xtask harness-boundary` — PASS。
- The full ordinary harness with the absolute worktree binary — PASS（84 library tests and every
  ordinary fast, Direct-Local, and PTY target）。

**Changed files:** `crates/lattice-system-harness/src/{driver,fake_llm,session_send}.rs`,
`crates/lattice-system-harness/tests/{direct_failures,direct_mutation}.rs`,
`docs/plans/2026-09-01-issue186-fake-provider-determinism.md`, and `FIX.md`.

## GitHub #186 PR #210 review follow-up: remove port-reuse flake and close wire gaps — 2026-09-01

**Severity:** MEDIUM（system-harness flake / raw-wire regression gap）

**Problem:** PR #210's shutdown regression immediately rebound the server's former ephemeral TCP
address. Under the ordinary parallel harness, Linux returned `EADDRINUSE` even though bounded
shutdown had completed, so the test failed 1/83 from OS port-reuse state rather than fake-provider
state. The existing raw request-line matrix also omitted non-ASCII and control bytes, and no client
declared a longer `Content-Length`, sent a short body, half-closed its write side, and pinned the
incomplete-body rejection bytes.

**Fix:** Deleted the TCP rebind assertion. The existing lifecycle test now checks the server's
ABA-free terminal state directly: its listener and serve handle are removed, the serve loop has
published exit, the active handler count is zero, all handler handles were joined and drained, and
no thread failure was stored. No sleep or new lifecycle API was added.

The existing request-line table now sends non-ASCII and control separators as raw bytes and checks
the fixed ASCII rejection response byte-for-byte, without UTF-8 replacement. The existing bounded
`Content-Length` test now declares four bytes, writes two, half-closes `Shutdown::Write`, and checks
the complete CRLF response, exact `Content-Length: 24`, and bounded completion. No new test matrix
or helper abstraction was added.

**Verification:**

- Before repair, the ordinary full harness reproduced the review failure exactly: 82 passed and
  `gated_response_waits_for_release_and_shutdown_joins_handlers` failed with `EADDRINUSE`.
- `cargo test -p lattice-system-harness --lib fake_llm::tests -- --nocapture` — PASS（15/15）。
- Exact request-line, short-body, and shutdown owner tests — PASS（50/50 each）。
- Exact concurrent capture/Sequence regression — PASS（50/50）。
- `cargo test -p lattice-system-harness --all-features --no-run` — PASS。
- `cargo build -p lattice-cli --bin lattice` — PASS。
- `LATTICE_BIN="/home/astrin/Dev/lattice-worktrees/issue-186/target/debug/lattice" cargo test -p
  lattice-system-harness --test direct_failures` — PASS（8/8）。
- The full ordinary harness with the same absolute branch binary — PASS（2/2 complete runs under
  default parallel load; each included 83/83 library tests and every ordinary target）。
- `cargo xtask harness-boundary`, `cargo fmt --all --check`,
  `cargo clippy -p lattice-system-harness --all-targets -- -D warnings`, and
  `git diff --check origin/main...HEAD` — PASS。

`FINDINGS.md` remains unchanged because this follow-up leaves no #186-specific residual. GitHub #40
stays open for maintainer closeout.

**Changed files:** `crates/lattice-system-harness/src/fake_llm.rs` and `FIX.md`.

## GitHub #186: make fake-provider HTTP and Sequence assignment deterministic — 2026-09-01

**Severity:** MEDIUM（system-harness false green / provider race / teardown hang）

**Problem:** The shared localhost fake provider accepted unchecked methods, targets, versions, and
framing; treated missing or malformed `Content-Length` as an empty body; allocated any declared
body size; and converted invalid request bytes with lossy UTF-8. HTTP error literals mixed
indentation with line endings and counted characters rather than sent bytes. Capture publication
and `Sequence` script assignment used separate locks on opposite sides of a response gate, so
concurrent handlers could swap the scripted response associated with capture order. Teardown
polled with sleeps, swallowed thread failures, and exposed no bounded stall, release, or shutdown
acknowledgement.

**Fix:** Kept the fixture deliberately narrow and dependency-free. Its byte parser now accepts one
`POST /chat/completions HTTP/1.1` request per connection with exact `Host`,
`Content-Type: application/json`, and single decimal `Content-Length` rules. It caps headers at
16,384 bytes and the body at 1,048,576 bytes before unbounded growth or body allocation, rejects
unsupported transfer encoding, persistence, pipelining, routes, malformed CRLF, and invalid UTF-8,
and emits fixed byte-exact rejection responses. Scripted HTTP errors use the same CRLF response
writer and byte-counted length. One shared lock now assigns the next `Sequence` entry and appends
the corresponding capture as one transaction.

`wait_for_requests`, `wait_until_stalled`, bounded release acknowledgement, explicit bounded
shutdown, health, and failure waits now use condition variables. Shutdown wakes blocking `accept`
with a loopback connection, cancels stalled handlers, waits only to the caller's deadline, and
retains serve/handler errors for assertions. The exact product tests
`direct_failure_sigint_while_stalled_commits_cancelled`,
`direct_failure_session_lock_serializes_same_session_turns`, and
`direct_mutation_concurrent_same_target_create_has_one_winner` replaced only synchronization
sleeps or ignored waits with these handshakes. Their existing exit, #185 outcome, event,
cleanup, persistence, and workspace assertions remain unchanged, so the replacement is an
assertion superset. #192 still owns combined assertion, leak, evidence, and cleanup failures.

**Verification:**

- `cargo test -p lattice-system-harness --lib fake_llm::tests -- --nocapture` — PASS（15 named
  tests, including protocol, oversize, exact error bytes, lifecycle, thread failure, and
  capture/Sequence pairing）。
- Exact concurrent capture/Sequence regression — PASS（50/50 consecutive runs）。
- `cargo test -p lattice-system-harness --all-features --no-run` — PASS（all targets compiled）。
- `cargo build -p lattice-cli --bin lattice` — PASS。
- `LATTICE_BIN="/home/astrin/Dev/lattice-worktrees/issue-186/target/debug/lattice" cargo test -p
  lattice-system-harness --test direct_failures` — PASS（8/8）。
- The full harness with the same absolute branch binary — PASS（83 library tests and every
  ordinary fast, Direct-Local, and PTY target）。
- `cargo xtask harness-boundary`, `cargo fmt --all --check`, and
  `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — PASS。

`FINDINGS.md` is unchanged because no #186-specific residual remains. GitHub #40 stays open for
maintainer closeout.

**Changed files:** `docs/plans/2026-09-01-issue186-fake-provider-determinism.md`,
`crates/lattice-system-harness/src/fake_llm.rs`,
`crates/lattice-system-harness/tests/{direct_failures.rs,direct_mutation.rs}`, and `FIX.md`.

## GitHub #185 PR #209 review: secure diagnostics and bounded cleanup — 2026-09-01

**Severity:** HIGH（secret disclosure / system-harness hang / cleanup false green）

**Problem:** Independent review found three defects in the first #185 implementation. Failure
artifacts and assertion/support errors formatted all of `CommandOutcome` with derived `Debug`, so
stdout/stderr secrets became reversible decimal byte arrays that the plaintext sentinel scan could
not find. `RunningCommand::finish` joined drain threads before inspecting the process group, which
let an exited direct child hang the harness while a descendant retained a pipe. Linux `/proc`
inspection also converted root, directory-entry, stat-read, and stat-parse failures into an empty
member set, allowing incomplete cleanup evidence to report success.

**Fix:** Added one metadata-only `CommandOutcomeSummary` containing termination, intervention,
cleanup, and stdout/stderr byte counts. Assertion, artifact, and scenario-support diagnostics use
that summary, and `CommandOutcome`'s defensive `Debug` delegates to it, so raw output bytes cannot
enter a diagnostic through a missed `Debug` caller. This does not add #192's combined-failure
system. Drain readers now use nonblocking Linux pipes with a bounded stop-and-join path. `finish`
inspects and reconciles the process group before draining; a remaining descendant or incomplete
inspection records `ForcedKill`, attempts `SIGKILL`, waits for bounded final survivor evidence, and
then returns the bytes collected so far without detached work. Process inspection now returns typed
`ReadProc`, `ReadProcEntry`, `ReadStat(pid)`, or `ParseStat(pid)` cleanup errors. Those errors remain
in `ProcessGroupCleanup`, make `succeeded()` false, and force the cleanup decision closed.

**Verification:**

- The original review probes reproduced before repair: the secret was recoverable from
  `0-command.txt`, and `timeout 1s ... -- orphan` exited 124. After repair, the orphan probe returns
  in 26 ms with forced cleanup; the adversarial secret decoder finds no byte array to parse.
- `cargo test -p lattice-system-harness --lib driver::tests -- --nocapture` — PASS（12 passed）。
- Descendant-inherits-pipe regression under a two-second external bound — PASS（20/20 runs）。
- `cargo test -p lattice-system-harness --lib assert::tests -- --nocapture` — PASS（3 passed）。
- `cargo test -p lattice-system-harness --lib artifacts::tests -- --nocapture` — PASS（18 passed）。
- `cargo test -p lattice-system-harness --all-features --no-run` — PASS。
- `cargo build -p lattice-cli --bin lattice` — PASS。
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness` — PASS
  （76 library tests and every ordinary fast, Direct-Local, and PTY target）。
- `cargo xtask harness-boundary`, `cargo fmt --all --check`, and
  `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — PASS。

The unrelated #200 operator timeout test was not deleted, weakened, or retimed. `FINDINGS.md` is
unchanged because these repairs leave no #185-specific code residual; the required PR check remains
a publication gate and is rerun after push.

**Changed files:** `crates/lattice-system-harness/src/{driver,assert,artifacts,lib}.rs`,
`crates/lattice-system-harness/tests/{support/mod.rs,direct_failures.rs,direct_session_isolation.rs,pty_tui_smoke.rs}`,
and `FIX.md`.

## GitHub #185: preserve command termination and cleanup facts — 2026-09-01

**Severity:** MEDIUM（system-harness false green / process cleanup diagnostics）

**Problem:** `CommandOutcome` used one `Termination` enum to mix the direct child's exit,
harness intervention, and cleanup. Signals collapsed to `Exited(None)`; an interrupt could hide
the observed exit; interrupt followed by timeout lost the timeout and forced kill; group-signal,
direct-child kill/wait errors, and surviving PIDs were discarded. Assertion helpers accepted only
`std::process::Output`, mapped signals to `-1`, and could not reject a matching exit code
contaminated by intervention or failed cleanup.

**Fix:** Replaced the old enum in place with one authoritative `CommandOutcome`. It now retains
stdout/stderr, `ProcessTermination::{Exited, Signaled}`, ordered `HarnessIntervention` facts, and
`ProcessGroupCleanup` with forced-cleanup state, typed operation errors, and sorted surviving PIDs.
Timeout after interrupt keeps interrupt, timeout, and forced-kill facts. Exact-exit helpers now
accept only a matching normal exit with no intervention, forced cleanup, cleanup error, or survivor.
All harness consumers retain the full outcome; failure evidence records the same model. No generic
panic inference was added because the product exposes no panic marker.

The existing timeout and interrupt test identities remain. Their prior byte-drain and empty-group
assertions remain and now also pin concrete termination, interventions, and cleanup. New exit,
signal, interrupt-timeout, explicit-kill, cleanup-failure, exact-exit, and artifact tests form a
strict assertion superset; no test was deleted or moved. `FINDINGS.md` was unchanged because this
fix leaves no issue-specific residual.

**Verification:**

- `cargo test -p lattice-system-harness --lib driver::tests -- --nocapture` — PASS（9 passed）;
  the same focused driver suite passed 20 consecutive runs.
- `cargo test -p lattice-system-harness --lib assert::tests -- --nocapture` — PASS（2 passed）。
- `cargo test -p lattice-system-harness --lib artifacts::tests -- --nocapture` — PASS（17 passed）。
- `cargo test -p lattice-system-harness --all-features --no-run` — PASS（all targets compiled）。
- `cargo build -p lattice-cli --bin lattice` — PASS。
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness` — PASS
  （71 library tests plus every ordinary fast, Direct-Local, and PTY target）。
- `cargo xtask harness-boundary` — PASS。
- `cargo fmt --all --check` — PASS。
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — PASS。
- Retired-interface search found no old `Termination`, `Exited(None)`, or lossy harness
  `.output`/`.status` consumer. The remaining `std::process::Output` belongs to test-side Git.

**Changed files:** `crates/lattice-system-harness/src/{driver,assert,artifacts,cli,lib,session_send}.rs`,
`crates/lattice-system-harness/tests/support/mod.rs`, the migrated `direct_*` consumers,
`fast_{audit,config,env_boundary,session}_cli.rs`, and `release_operator.rs`.

## GitHub #182 PR #184 review: keep compact TUI copy actionable — 2026-08-31

**Severity:** MEDIUM（status 可用性 / 视觉契约）

**Problem:** PR #184 修复了 Ctrl+C 提示的过期重绘，但 80 列右栏只有 15 个可用 cell，
硬编码的中文提示会把 `Ctrl+C` 和退出动作截掉；窄屏底栏完全不读取两秒提示状态。界面同时
混用 `你`、`换行` 与其他英文 UI 文案。用户正文还错误复用了 User / iris-7，而锁版颜色
契约要求只有 speaker/prompt/stream phase 使用 User，正文使用 Assistant / cream-2。权威 spec
在合并后仍把 #118 的旧布局描述成当前实现。

**Fix:** 将固定 UI 文案收口为 `user`、`newline`、`Ctrl+C to quit`。宽屏提示现在完整落在
15-cell status 内容区；窄屏在两秒窗口内用退出提示替换最低优先级的换行键提示，过期后
恢复 `Alt+Enter newline` / `S-Enter newline`。用户正文改用 Assistant 色，speaker `user`
保留 User 色。同步把 #182 spec 标记为已由 PR #184 落地，并更新 crate 契约文案；没有新增
timer、状态、布局、依赖或国际化抽象。

**Verification:**

- RED：`cargo test -p lattice-tui view::tests:: -- --nocapture` 按预期失败 2 项：80 列不含
  `Ctrl+C to quit`，speaker 仍为 `你`。
- GREEN：同一 focused view 测试 — PASS（12 passed）。
- `cargo test -p lattice-tui` — PASS（33 passed）。
- `cargo clippy -p lattice-tui --all-targets -- -D warnings` — PASS。
- `cargo fmt --all -- --check`、`git diff --check`、修改 Rust 文件 LSP diagnostics — PASS。

**Changed files:** `crates/lattice-tui/src/view.rs`、`crates/lattice-tui/AGENTS.md`、
`docs/specs/2026-08-31-issue182-tui-cassette-futurism-spec.md`、`FIX.md`。

## GitHub #179 acceptance audit: restore the all-feature clippy gate — 2026-08-31

**Severity:** MEDIUM（acceptance evidence / test-only lint）

**Problem:** 验收第 15 条要求 all-feature clippy，但 `cargo xtask ci` 的 workspace clippy 不带
`--all-features`，此前的 focused clippy 也只覆盖 `lattice-session`。在当前 branch 首次运行
`cargo clippy --workspace --all-targets --all-features -- -D warnings` 时，
`direct_crash_barriers::wait_for_ack` 的嵌套 `if let` 触发 `clippy::collapsible_if`。因此第 15 条
此前没有完整证据，且实际为失败。

**Fix:** 将两层 `if let` 合为一个 let-chain，不改变 ack 部分写入重试语义。第 12 条也逐项复核：
barrier/matrix 实现在 `6e5d2094`，`T-GH34-04` 的关闭记录在 `36177b28`，当前
`FINDINGS.md` 已关闭该项；完整 crash target 与普通 system harness 都在当前 source head 复跑。

**Verification:**

- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — 首次按预期 FAIL，
  修正后 PASS。
- 使用本工作树 feature binary 运行完整 `direct_crash_barriers` target — PASS（7 passed）。
- `env -u LATTICE_BIN cargo xtask ci` — PASS，包含 fmt、workspace clippy、两个 boundary gates、
  24 crate tests、完整普通 system harness、tool-codegen 与 Tool artifact verify。
- `cargo fmt --all -- --check`、修改文件 LSP diagnostics、`git diff --check` — PASS。

**Changed files:** `crates/lattice-system-harness/tests/direct_crash_barriers.rs`、`FIX.md`。

## GitHub #179 review: make conservative terminal recovery repeatable — 2026-08-30

**Severity:** HIGH（repeated-crash reconciliation correctness）

**Problem:** 第一次 reopen 遇到 `DispatchStarted` 时，会把当前 invocation 保守终结为
`Indeterminate(MayHaveOccurred) + Unavailable(DraftPublicationFailed, None, None)`。若进程在随后
发布 `AdvanceStepClosing` 前再次退出，第二次 reopen 会把自己刚写入的 terminal 交给
`validate_terminal_with_candidate` 或 `validate_terminal_without_candidate`；两者都拒绝这个 exact
保守形状并误报 `JournalCorruption`。

**Fix:** 两个 validator 现在只额外接受 exact
`Indeterminate + DraftPublicationFailed(None, None)`。有无仍然 pinned 的 `ResultDraft` 都可恢复，
其他 candidate/material mismatch 继续 fail closed。回归作为既有 StepClosing reducer table 的两行
加入，没有新增 test function、PendingStage、schema version 或 wire 字段。`NotExecutedReason` 文档同时
明确 ledger 边界：首个观察到 cancellation 的 invocation 记录 `CancelledBeforeDispatch`，后续仅因
batch 已停止而闭合的 bindings 记录结构性 `BeforeDispatch`；live 与 reopen 因而写出相同证据。
PendingStage 数量文档由错误的 thirteen 改回 twelve。

**Verification:**

- RED：`cargo test -p lattice-session step_closing_resumes_the_cursor_binding_from_its_terminal_fact -- --nocapture`
  以 `terminal material does not match the pinned draft candidate` 失败。
- GREEN：同一 focused reducer test — PASS。
- `cargo test -p lattice-session` — PASS（152 passed）。
- `cargo clippy -p lattice-session --all-targets -- -D warnings` — PASS。
- 使用本工作树 feature binary 运行完整 `direct_crash_barriers` target — PASS（7 passed）。
- `env -u LATTICE_BIN cargo xtask ci` — PASS（fmt、workspace clippy、boundary gates、
  tool-codegen、24 crate tests、完整 system harness 与 Tool artifact verify）。

**Changed files:** `crates/lattice-session/src/pending/{contract,facts,mod,reconcile}.rs`、
`FINDINGS.md`、`FIX.md`。

## GitHub #179: make live and reopen Tool batches use one durable authority — 2026-08-30

**Severity:** HIGH（durable Turn closure / crash recovery correctness）

**Problem:** live saga 与 reopen reducer 分别推导 Tool batch 的控制结果。live path 读取
`CepErrorCode` 和 lossy `ToolResultOutcome`，reopen 则读取 durable invocation facts；同一
terminal 可随 crash timing 得到不同 Turn outcome。另一个缺口在无 ResultDraft 的合法 terminal：
`ResultDraft::new` 对超过 64 KiB 的结果返回 `OutputTooLarge`，ledger 随后持久化
`TerminalMaterial::Unavailable(OutputTooLarge)`，但 `StepClosing.current_candidate` 保持 `None`。
reopen 把这个 terminal 当作 journal corruption。`dispatch_started` SIGKILL row 还因两层 barrier
使用不同环境变量而无法运行。

**Fix:** `lattice-session` 新增纯函数 `derive_tool_batch_control`，live 与 reopen 都把 exact
published terminal 投影成同一 `InvocationLifecycleFact` 后调用它；`CepError` 只保留诊断用途，
conversation closure 继续保持 lossy。`lancet-api::DispatchOutcome::NotExecuted` 现在携带 closed
`DispatchNotStartedCause`，不再由错误码决定 durable reason。PendingTurn 保持 schema v3 与既有
wire shape；`contract.rs` 仅扩展 no-candidate cursor 真值表，使 exact
`Succeeded + Unavailable(OutputTooLarge, Some(digest), Some(length))` 合法。reducer 从 pinned
InvocationStore facts 恢复 exact no-draft terminal，并继续拒绝缺少 matching draft 的
`Available` / `DraftPublicationFailed`、material mismatch 与 `Reserved + ResultDraft`。

Product SIGKILL matrix 复用现有 barrier，加入真实 `session.todo_write` 70 KiB 结果和真实
`mutate.create` pre-dispatch cancellation rows。cancellation 注入与 crash 位置使用两个独立环境
变量；两层 barrier 统一为 `LATTICE_TEST_TURN_BARRIER{,_ACK}`，ack 同时携带 exact typed
invocation record 与 pinned session snapshot。旧 standalone cancellation tests 和重复 helper 已删除。

**Verification:**

- `cargo test -p lattice-session` — PASS。
- `cargo test -p lancet-api`、`cargo test -p lattice-api` — PASS。
- `cargo check -p lattice-api --features test-crash-barriers` — PASS。
- 使用本工作树 `target/debug/lattice` 运行完整 `direct_crash_barriers` target — 7 passed，
  包含 `dispatch_started`、`OutputTooLarge` terminal→cursor/reopen、pre-dispatch cancellation、
  multi-Tool cursor 与 TurnClosing rows。
- `env -u LATTICE_BIN cargo xtask ci` — PASS（toolchain、fmt、workspace clippy、debug binary、
  boundary gates、tool-codegen、24 crate tests、system harness、Tool artifact verify）。
- `cargo clippy -p lattice-session --all-targets -- -D warnings` — PASS。

**Changed files:** `AGENTS.md`、`FINDINGS.md`、`FIX.md`、
`crates/lancet-api/src/{lib,runtime}.rs`、`crates/lattice-api/src/turn/{crash_barrier,invocation_facts,saga}.rs`、
`crates/lattice-api/tests/support/mod.rs`、
`crates/lattice-session/src/pending/{contract,contract_tests,mod,outcome,reconcile}.rs`、
`crates/lattice-session/src/git_graph/tests/pending_crash.rs`、
`crates/lattice-system-harness/tests/direct_crash_barriers.rs`。

## GitHub #34 PR #178 review: close pre-dispatch cancellation durably — 2026-08-29

**Severity:** HIGH（durable cancellation / crash recovery correctness）

**Problem:** runtime 已在 admission 前阻止 pre-dispatch cancellation 的 Tool 副作用，并把
invocation 终结为 `NotExecuted(CancelledBeforeDispatch)`；但 saga 闭合该 binding 后直接
`continue`，没有把取消传播到 `stop_batch`。单 ToolCall 因此持久化
`StepClosed::ContinueModel`。若进程在下一轮 live cancellation check 前退出，reopen 会按
契约闭合为 `Interrupted(WhileAwaitingModelContinuation)`，使同一 durable cancellation
因崩溃时机不同得到 `Cancelled` 或 `Interrupted` 两种 Turn outcome。

**Fix:** `DispatchOutcome::NotExecuted(CallCancelled)` 在完成当前 terminal、
`ClosedBinding`、cursor 和事件后把 `CancelledBeforeDispatch` 写入 `stop_batch`。后续
bindings 继续按既有顺序闭合为未执行，step 最终持久化 `StepClosed::CancelTurn`；普通
`BeforeDispatch` 错误不受影响。新增 test-only saga hook，在循环顶部检查后、runtime
admission 前置位真实 cancel flag，并由 external binary harness 在 durable `StepClosed`
处 SIGSTOP/SIGKILL。回归同时验证真实 `mutate.create` 未写文件、未移动
WorkspaceHistory、ledger reason、无下一次模型请求，以及 reopen 后
`TurnOutcome::Cancelled`。同步更正 `lancet-runtime/AGENTS.md`：durable reservation
校验属于 `lancet-api::LancetRuntime::dispatch`，`RuntimeExecution` 只负责
request/binding consistency、descriptor resolution、Cell admission 与 Component execution。

**Verification:**

- RED：构建 `cargo build -p lattice-cli --features test-crash-barriers` 后，以该 binary
  运行 focused `pre_dispatch_cancellation_step_closed_sigkill_reopens_cancelled`；snapshot
  实际为 `StepClosed::ContinueModel`，测试按预期失败。
- GREEN：同一 focused SIGKILL 测试 — PASS（1 passed）。
- `env -u LATTICE_BIN cargo xtask ci` — PASS（toolchain、fmt、clippy、binary build、
  boundaries、tool-codegen、24 crate tests、system harness 与 Tool artifact verify）。
- `cargo test -p lattice-api --all-features` — PASS。
- `cargo clippy -p lattice-api -p lattice-system-harness --all-targets --all-features
  -- -D warnings` — PASS。
- `cargo xtask lancet-boundary`、`cargo xtask harness-boundary` — PASS。
- 三个改动 Rust 文件的 LSP diagnostics、`cargo fmt --all --check`、
  `git diff --check` — PASS。
- 完整 `direct_crash_barriers` target：7 passed / 1 pre-existing failure；失败的
  `dispatch_started` case 使用两套不一致的 test-only env names，已记录为
  `FINDINGS.md` `T-GH34-04`，不属于本次 cancellation closure。

**Changed files:** `crates/lattice-api/src/turn/{saga,crash_barrier}.rs`、
`crates/lattice-system-harness/tests/direct_crash_barriers.rs`、三份 crate-local
`AGENTS.md`、`FINDINGS.md`、`FIX.md`。

## GitHub #34 PR #178 review: close cancellation across the durable handoff — 2026-08-29

**Severity:** HIGH（cancellation correctness / mutation safety）

**Problem:** saga 在进入 dispatch 前只检查一次 request `cancel_flag`。runtime 随后先发布
`DispatchStarted`，再创建初值为 false 的 call-scoped watch，并依赖每 10 ms 运行一次的
异步 monitor 转发 flag。取消若在 saga 检查后到达，monitor 不保证先于 Component 的
pre-cancel check 运行；真实 `mutate.create` 可先安装文件并写入 private history。

**Fix:** `LancetRuntime::dispatch` 在 exact request check 后、durable admission 前同步读取
flag；已取消则返回共享的 typed `CallCancelled`，保持 reservation 未 dispatch，
`lattice-api` 以 `NotExecuted(CancelledBeforeDispatch)` 闭合。admission 成功后先建立
call-scoped sender/receiver 和持续 monitor，再同步重读 flag；若此时已取消，立即调用
sender，使 Component handoff 前的 authoritative pre-cancel check 必然看到取消。该路径
已越过 `DispatchStarted`，因此仍返回 executed receipt，并由 saga 映射为 durable
`TerminalOutcome::Cancelled` / conversation `Cancelled`；monitor 只负责后续 in-flight
取消。`lancet-runtime::error_map::cancelled_error` 公开给薄门面复用，未复制错误码或
category authority。

**Verification:**

- RED：`cargo test -p lancet-api cancel` — 2 failed；pre-cancelled case 越过
  admission，admission-window case 实际返回已写入 22 bytes 的 `mutate.create` 结果。
- GREEN：同一命令 — 2 passed；两条均使用真实 `mutate.create`，分别断言
  `Reserved`/`DispatchStarted` 边界、零 workspace 文件、零 workspace history。
- `cargo test -p lattice-api call_cancelled_maps_to_cancelled_terminal_and_conversation`
  — PASS，锁定 durable/conversation `Cancelled` 映射及 `CancelledBeforeDispatch` reason。
- `cargo test -p lancet-runtime -p lancet-api -p lattice-api` — PASS。
- `cargo clippy -p lancet-runtime -p lancet-api -p lattice-api --all-targets
  --all-features -- -D warnings` — PASS。
- `cargo xtask lancet-boundary` — PASS。
- 三个改动 Rust 文件的 LSP diagnostics、`cargo fmt --all --check`、
  `git diff --check` — PASS。

**Changed files:** `crates/lancet-runtime/src/error_map.rs`、
`crates/lancet-api/src/runtime.rs`、`crates/lattice-api/src/turn/saga.rs`、
三份 crate-local `AGENTS.md`、`FINDINGS.md`、`FIX.md`。

## GitHub #34 PR #178 review: admission durability ambiguity fails into reconciliation — 2026-08-29

**Severity:** HIGH（durable invocation lifecycle / correctness）

**Problem:** `LancetRuntime::dispatch` 把 `InvocationStore::admit_reserved` 的所有错误和
blocking worker 失败都包装为 `DispatchOutcome::NotExecuted`。但 admission 会先发布
`DispatchStarted` ref、再执行 durability barrier；后置 fsync 失败时调用方拿到 `Err`，
authoritative ledger 已经越过 dispatch boundary。`lattice-api` 随后补写
`NotExecuted(BeforeDispatch)`，会与 durable fact 冲突，并把 may-have-executed 路径错误
降级为确定未执行。

**Fix:** admission 失败后按同一 typed `InvocationId` 重读 authoritative ledger。只有
事实仍为 `Reserved` 时才返回 `NotExecuted`；观察到候选缺失、
`DispatchStarted`/`Terminal` 或无法完成 authoritative read 时返回新的
`DispatchOutcome::ReconciliationRequired`。durability 类原始错误保持 `Transient`，
blocking worker 的未知状态保持 `Unknown`。`lattice-api` 在现有 `StepClosing` journal
位置停止 live saga，发出 typed `TurnEvent::RecoveryRequired`；`run_turn` collector、CLI
和 TUI 保留 typed recovery，而 reopen 继续使用既有 reducer 将裸
`DispatchStarted` 闭合为 `Indeterminate(MayHaveOccurred)`，不重发 Tool，也不伪造
`NotExecuted` terminal。

**Verification:**

- RED：`cargo test -p lancet-api published_dispatch_started_is_not_classified_not_executed`
  在旧实现下失败，观测到 `DispatchStarted` 时仍返回 `NotExecuted`。
- GREEN：`cargo test -p lancet-api` — 9 passed。
- `cargo test -p lattice-api reconciliation_requirement` — 2 focused tests passed。
- `cargo test -p lancet-api -p lattice-api -p lattice-cli -p lattice-tui` — PASS
  （lancet-api 9、lattice-api 90 + integration matrices、lattice-cli 125、lattice-tui 24）。
- `cargo clippy -p lancet-api -p lattice-api -p lattice-cli -p lattice-tui
  --all-targets --all-features -- -D warnings` — PASS。
- `cargo xtask lancet-boundary` — PASS。
- `cargo fmt --all --check`、`git diff --check`、关键文件 LSP diagnostics — PASS。

**Changed files:** `crates/lancet-api/src/runtime.rs`、
`crates/lattice-api/src/{session.rs,turn/{types.rs,types_tests.rs,collector.rs,saga.rs}}`、
`crates/lattice-api/tests/support/mod.rs`、`crates/lattice-cli/src/commands/run.rs`、
`crates/lattice-tui/src/app.rs`、相关 `AGENTS.md`、`FINDINGS.md`、`FIX.md`。

## GitHub #34 follow-up: 删除墓碑门禁与弱信号测试 — 2026-08-28

**Severity:** LOW（测试债务 / hard-cut 收口）

**Problem:** #34 首次提交后只读复核发现一条新增单测仅点名断言已删除 legacy
effect 字段不存在；同时 `cargo xtask lancet-boundary` 仍永久扫描 #139-B 已删除符号，
并由三条自测维护这份 tombstone needle 表。另有两条单测分别只断言 descriptor
identity 字符串长度为 64、两个独立 `watch` channel 互不共享状态，未证明其测试名
声称的 host pin 来源或 runtime 调用隔离。它们没有独立语义回归信号。

**Fix:** 删除 `capability_error_data_round_trips_without_legacy_effect_fields`、
`enabled_projection_uses_host_pinned_identity`、`cancellation_pairs_are_independent`，
删除 `ISSUE139B_LEGACY_NEEDLES`、`issue139b_legacy_residue_hits` 及其三条自测，并从
LANCET boundary runner 移除该永久 tombstone gate。删除只被该 gate 使用的
`xtask/src/scan_rs_tree.rs`；保留真实依赖图、production/test feature 边界、当前
catalog unknown-name、mid-traversal cancellation、typed event identity 与生命周期
排序回归。

**Verification:**
- `env -u LATTICE_BIN cargo xtask ci` — PASS（完整 workspace 与 system harness）。
- `cargo test -p lancet-protocol -p lancet-runtime -p xtask` — PASS（73 / 202 +
  9 / 12 / 14 / 11 / 102）。
- `cargo xtask lancet-boundary` — PASS，当前只检查仍存在的结构边界。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — PASS。
- `cargo fmt --all --check`、`git diff --check`、LSP diagnostics — PASS。

**Changed files:** `crates/lancet-protocol/src/error.rs`、
`crates/lancet-runtime/src/{catalog,control}.rs`、`xtask/src/{main,scan_rs_tree}.rs`、
`xtask/src/lancet_boundary{,/check}.rs`、`FIX.md`。

## GitHub #34: LANCET runtime convergence hard cut — 2026-08-28

**Severity:** HIGH（架构 / 安全边界 / 删除迁移残留）

**Problem:** 五个默认 Tool 已迁为 manifest-owned WASM Components，但生产 Turn 仍通过
`lancet-native` → `lancet-dispatch` / `lancet-cep` / `lancet-catalog` 的兼容链执行，
并保留 legacy effect policy/type、native registry、JSON-RPC client/server transport、
独立 catalog authority 与多套调用/错误/证据语义。这使 closed catalog、typed
`InvocationId`、durable reservation/admission、Cell evidence 与 cancellation 的唯一权威
仍不成立，也阻塞 ADR 0004 的最终收敛。

**Fix:**
- 删除 `lancet-native`、`lancet-dispatch`、`lancet-cep`、`lancet-catalog` 四个 crate，
  以及 `ImpactKind` / `EffectScopeKind` / `OverlayBehavior` /
  `FailureSemantics` / `BaseAuthorityRequirement`、`ResolvedPolicy`、legacy effect
  ledger/checker 与 `[lancet.effects]` 生产面。
- 在 `lancet-runtime` 建立唯一 closed Component catalog、provider Tool projection、
  reservation-bound `RuntimeExecution`、call-scoped cancellation、host-bound ordered
  Tool events 与 typed `CellEvidenceTriple` receipt；workspace observation、mutation、
  network fetch 统一复用同一 admitted descriptor、budget、Local Workspace 与 history。
- 将 `lancet-api` 收窄为 runtime/audit/rewind 门面；`lattice-api::TurnCoordinator`
  在 `DispatchStarted` 后直接调用该门面并以同一 typed `InvocationId` 发布 Tool
  events、terminal material 与 evidence。修复异步转发竞争，使
  `cep_call_started < cep_call_finished < ToolCallFinished` 对同一 invocation 恒成立。
- 严格 profile 只接受 Cell capability/workspace/network/budget 配置；更新 CLI status/
  doctor、rollout 顺序、README、LANCET/TESTING 文档与 crate-local `AGENTS.md`。
  `cargo xtask lancet-boundary` 现在检查实际剩余依赖边界、legacy hard-cut residue
  与 production/test feature reachability，不保留已删除名字的 tombstone deny-list。

**Verification:**
- `env -u LATTICE_BIN cargo xtask ci` — PASS（toolchain、fmt、clippy、binary build、
  harness/LANCET boundaries、tool-codegen、24-crate tests、全部 system harness、
  `cargo xtask tool-build --verify`）。
- `LATTICE_BIN=$PWD/target/debug/lattice cargo test --workspace` — PASS。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — PASS。
- `cargo build --workspace --exclude noop-echo --exclude fetch-web --exclude observe-workspace
  --exclude mutate-create --exclude mutate-edit --exclude session-todo_write` — PASS；
  `cargo build -p noop-echo --target wasm32-unknown-unknown` — PASS。Issue 原列出的
  `cargo build --workspace --exclude noop-echo` 会按预期 host-link 其余五个 wasm guest
  cdylib 并失败；该既有 Cargo 目标隔离限制已在 `FINDINGS.md` `ENV-WASM-01` 更新，
  真实 guest gate 由 `cargo xtask tool-build --verify` 覆盖。
- `cargo test -p lancet-api -p lancet-runtime -p lattice-api -p lattice-profile
  -p lattice-cli -p lattice-system-harness` — PASS。
- `cargo xtask lancet-boundary`、`cargo fmt --all --check`、`git diff --check` — PASS。

**Changed files:** 删除 `crates/{lancet-native,lancet-dispatch,lancet-cep,lancet-catalog}`；
重构 `crates/lancet-{protocol,authority,runtime,api}`、`crates/lattice-{api,profile,cli}`
及 system harness；更新 root manifests、`README.md`、`AGENTS.md`、`FINDINGS.md`、
`docs/{LANCET,TESTING}.md`、issue #28 design closeout 与 `xtask` boundary/CI/rollout。

## 测试清理：删除不符合 YAGNI/KISS 的重复与无效测试 — 2026-08-28

**Severity:** LOW（测试债务与维护成本；无生产行为变更）

**Problem:** 工作区中保留了多组严格被更强测试覆盖的测试，以及若干无法检测其
声称不变量的恒真/编译型测试。典型问题包括：同一 renderer 的 focused 与 sequence
测试重复、单值 profile 拒绝测试被表驱动矩阵覆盖、直接比较手工构造的 `Option`/enum
值、所谓负向 API/trait 测试不会因新增禁用方法而失败，以及 #171 的共享 runtime
测试只检查硬编码 metrics 和脱离生产依赖的 `Arc::ptr_eq`。这些测试增加运行与维护成本，
但不增加独立回归信号。

**Fix:** 删除 38 个经两轮只读审计与对抗复核确认的冗余/无效测试，并删除只服务于
被删测试的 `descriptor_with` helper。保留具有独立信号的
`direct_local_plain_text_turn_commits_and_exit_zero`、
`tail_does_not_panic_when_the_cut_lands_mid_codepoint` 和 noop committed `render.json`
比较。#32 已在本次清理前移除旧 native-catalog 的两个候选测试，因此本次未触碰它们。
除清理由测试删除产生的未使用 import 外，不改生产代码。净减少 785 行。

**Verification:**

- `env -u LATTICE_BIN cargo xtask ci` — toolchain、fmt、clippy、build、system-harness boundary、LANCET boundary、tool-codegen check、28 crate tests 与完整 system harness 全部通过。
- `git diff --check` — clean；38 个目标测试名全部消失，3 个明确保留的测试仍存在。

**Changed files:** 20 个受影响的 test/test-module 文件（含
`crates/lattice-api/src/turn/coordinator.rs` 的测试 import 清理）与 `FIX.md`。

## GitHub #85 / #86 / #87 / #89 / #102 / #168: 全面收口文档漂移、幽灵 API、编号与契约描述 — 2026-08-28

**Severity:** DOCUMENTATION / LOW

**Problem:** 仓库多处现行文档、crate-local AGENTS.md、Rust doc comments 与测试说明存在漂移与历史遗留失真：
1. **#85**: 议题跟踪规则缺少对 AGENTS.md 必须使用 GitHub 编号的显式约束。
2. **#86**: 根级与子模块错误提示存在不存在的命令指引（`lattice new agent default`）、过时的 Rust 版本描述及残留的 `git2`/`migrate` 提及。
3. **#87**: 已退役的 `BaseAuthorityPlan`、`BaseExecutionEnvelope`、`lancet-cep/src/authority.rs` 仍残留在活跃文档中。
4. **#89**: 四份 crate-local `AGENTS.md`（`lancet-protocol`、`lancet-authority`、`lancet-native`、`lattice-cli`）的模块地图指向已删除的文件（`envelope.rs`、`boundary.rs`、`boundary_credit.rs`、`release_smoke.rs`），且漏记了 `effect_types.rs`、`binding.rs` 与 `policy/visibility.rs`。
5. **#102**: `README.md` 与 `AGENTS.md` 自述性架构说明、`INFO.md` 两条指向已删除架构的笔记以及 `FINDINGS.md` 的历史 P22 路径存在偏差。
6. **#168**: `FinalizeBudget` 截止契约在生产计划文档中未明确“截止后不启动新 attempt 但允许在途 kernel durability call 安全返回”；Rewind 注释与测试中的 `status=error` / ledger 创建生命周期注释与当前生产实现不一致。

**Fix:**
- `docs/agents/issue-tracker.md`: 明确根与各 crate `AGENTS.md` 统一使用 GitHub 编号且不属于历史豁免面。
- `crates/lattice-profile`: 将 `MissingDefault` 错误提示及 `discovery.rs` 测试断言修正为引导创建 `agent.toml`。
- `docs/LANCET.md`、`docs/README.md`、`docs/TESTING.md`: 清理指向已删除文件的链接，移除对 `BaseAuthorityPlan` 等幽灵 API 的描述。
- `crates/lancet-protocol`、`crates/lancet-authority`、`crates/lancet-cep`、`crates/lattice-cli`: 全面同步 `AGENTS.md` 与 `src/lib.rs` 中的模块地图与测试验证指引。
- `crates/lancet-runtime/src/rewind_audit.rs`: 纠正 `target_history_oid` 描述为“目标历史 OID”而非“已移动至”。
- `crates/lattice-system-harness`: 修正 `direct_failures.rs` 中的 `outcome=failed` 注释与断言文案，修正 `fast_audit_cli.rs` 关于 project ledger 建立时机的描述。
- `docs/plans/2026-08-23-issue139b-production-hard-cut.md`: 补充 `FinalizeBudget` 截止与安全返回的权威契约说明。
- `INFO.md` / `AGENTS.md`: 将过时笔记归入 `Obsolete Notes` 并补充 `INFO.md` 维护规则；校正 `FINDINGS.md` P22 路径。

**Verification:**
- `cargo test -p lattice-profile -p lancet-protocol -p lancet-authority -p lancet-native -p lattice-cli` (PASS)
- `LATTICE_BIN=$PWD/target/debug/lattice cargo test -p lattice-system-harness --test direct_failures --test fast_audit_cli` (PASS)
- `cargo clippy --workspace --all-targets -- -D warnings` (PASS)
- `cargo fmt --all --check` (PASS)
- `cargo xtask harness-boundary && cargo xtask lancet-boundary && cargo xtask tool-codegen --check` (PASS)

**Changed files:** `AGENTS.md`, `FINDINGS.md`, `INFO.md`, `crates/lancet-authority/AGENTS.md`, `crates/lancet-cep/AGENTS.md`, `crates/lancet-protocol/AGENTS.md`, `crates/lancet-protocol/src/lib.rs`, `crates/lancet-runtime/src/rewind_audit.rs`, `crates/lattice-cli/AGENTS.md`, `crates/lattice-profile/src/discovery.rs`, `crates/lattice-profile/src/error.rs`, `crates/lattice-system-harness/tests/direct_failures.rs`, `crates/lattice-system-harness/tests/fast_audit_cli.rs`, `docs/LANCET.md`, `docs/README.md`, `docs/TESTING.md`, `docs/agents/issue-tracker.md`, `docs/plans/2026-08-23-issue139b-production-hard-cut.md`, `FIX.md`.
## CI: remove crate-count prose pin after docs deletion — 2026-08-26

**Severity:** LOW (CI red on `main`; test was not a real architecture gate)

**Problem:** `xtask/src/ci.rs::documented_crate_counts_match_the_crates_directory`
substring-checked that four files contained `"{n} crates"` against the
`crates/` directory count. After the 2026-08-26 docs deletion
(`docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `docs/GETTING-STARTED.md`),
GitHub CI (`cargo test --workspace`) panicked on the missing files before
asserting anything. The pin was added for #102 and is not a dependency-graph
or catalog-boundary check.

**Fix:** Delete the test and its #102 comment. Drop the matching pin claim and
deleted-doc list from the root `AGENTS.md` hygiene bullet. Leave remaining
xtask CI tests (wasm-tools pins, `ordered_test_crates`, tool-manifest
fingerprint) unchanged. Do not restore the deleted docs. Historical `FIX.md`
entries that mention the old test are left as-is.

**Verification:** `cargo test -p xtask` — 120 passed, 0 failed. Remaining
`ci::tests` still run: wasm-tools parse/pin, tool-manifest fingerprint,
`ordered_test_crates` (deterministic + dependency order). The deleted test
name is absent from the run.

**Changed files:** `xtask/src/ci.rs`, `AGENTS.md`, `FIX.md`.

## GitHub #98（部分）：符号链接拒绝不再回吐宿主路径；字面量 `..` 文件名不再拖垮整次搜索 — 2026-08-22

**Severity:** 两项均 LOW

本票三条互不相关，本轮只修第 1、2 项。**第 3 项（`net-guard` 在 async 栈上同步
`getaddrinfo`）未修**，#98 保持 open，残留已记入 `FINDINGS.md` 的 `L-GH98-03`。

**1. `SymlinkRejected` 回吐宿主绝对路径**

`LocalWorkspaceError::SymlinkRejected { component }` 持有的是从 canonical root
一路 push 出来的**绝对**路径，Display 为 `symlink rejected at component: {component}`。
观察侧不受影响——`host_workspace::observation_io_error` 早就把这一支折叠成固定
reason；泄漏点在 `host_mutate`，它把 `err.to_string()` 直接放进 guest 可见的
`MutateError::PathRejected { reason }`，于是 Tool（以及经对话上送 provider 的内容）
拿到项目在磁盘上的真实位置。修法：两个构造点都改成 root-relative——
`open_parent_dir_secure` 的 `cur` 本就"仅用于错误报告、从不触碰文件系统"，直接从
`PathBuf::new()` 起步；`resolve_secure` 的 `cur` 要用于 `symlink_metadata` 不能改，
故经新 helper `walk_component_for_error` 剥掉 root 前缀（strip 失败时回落到叶名而
不是 `cur`，保证任何分支都不会把绝对路径放进消息）。`host_workspace` 那一支保持
固定字符串不变——隐藏路径的存在本身就是要瞒住的东西；两处过时注释（称该 variant
"carries an ABSOLUTE host path"）已同步更正。

**2. `WorkspacePath::parse` 用 `raw.contains("..")` 子串拒绝**

同函数末尾的 `normalize_relative` 已经逐分量拒绝 `Component::ParentDir`，所以子串
检查对真实穿越是冗余的，对合法文件名却过严：`foo..bar.rs` / `..gitignore` 这类名字
会让 parse 失败。因为 grep/find 对走到的每个条目都 parse，一个这样的文件会让**整次**
搜索报错，而不是把它当成一条普通结果。修法：删掉子串检查，穿越判定完全交给分量检查。

**Verification:**

- 新增 `workspace::types::tests::literal_dot_dot_in_a_filename_is_not_traversal`
  （5 个合法名，并断言 normalization 未改写）与
  `parent_dir_components_are_still_rejected`（6 个真实穿越，含 `./../x`、
  `a/b/../../..`）。
- 新增 `workspace::local::tests::symlink_rejection_reports_a_relative_component_not_the_host_path`，
  同时覆盖两个构造点（`resolve_secure` 走查与 `open_parent_dir_secure` 的
  pinned-dirfd 走查），断言 component 恰为 `nested/link` / `swap`，且 Display 不含
  workspace root 与 `/tmp`。
- 反向验证：把两处构造点改回绝对 `cur` → 该测试 FAILED，
  `left: "/tmp/.tmp4523G6/nested/link", right: "nested/link"`；把子串检查加回 →
  `legal filename foo..bar.rs rejected`。两者恢复后全绿。
- `cargo test -p lancet-runtime` — 177 + 10 + 12 + 15 + 11 passed。
- `cargo clippy -p lancet-runtime --all-targets -- -D warnings` — clean。
- `cargo fmt --all --check` — clean。
- 注：本轮撞上 `FINDINGS.md` 的 `ENV-DISK-01`（/tmp tmpfs 有用户配额，多 worktree
  共享 cargo target 时链接阶段报 `Disk quota exceeded`）。处理方式是给本 worktree
  单独设 `CARGO_TARGET_DIR` 与 `TMPDIR` 指向 `/` 上的 `target/`，未删除共享缓存。

**Changed files:** `crates/lancet-runtime/src/workspace/local.rs`,
`crates/lancet-runtime/src/workspace/types.rs`,
`crates/lancet-runtime/src/host_workspace.rs`（两处过时注释）, `FINDINGS.md`, `FIX.md`.

## GitHub #100（部分）：tracing 改写 stderr、Anthropic 输出上限脱离 4096 — 2026-08-22

**Severity:** A = MEDIUM（破坏 `--json` 机器契约）、C = MEDIUM（用户可见截断）

本票三条互不相关，本轮只修 A 与 C。**B（`turn_id` 不唯一）未修**，#100 保持 open，
残留已记入 `FINDINGS.md` 的 `M-GH100-01`。

**A. tracing 日志写 stdout，污染 `--json` 的 JSONL 契约**

`crates/lattice-llm/src/logging.rs` 两个分支都没设 writer，而
`tracing_subscriber::fmt()` 的默认写者是 **stdout**；非 verbose 的默认级别又是
`error` 而不是关闭，所以只要有一条 error 级日志，`lattice --json` 的 stdout 里就
会混进非 JSON 行，下游 `jq` / 解析器直接失败。修法：两个分支都加
`.with_writer(io::stderr)`；`with_ansi` 从硬编码（verbose 恒 `true`、非 verbose
恒 `false`）改为 `ansi_enabled(stderr.is_terminal(), NO_COLOR)`，重定向的 stderr
不再收到转义序列。

**C. Anthropic 输出被硬顶在 4096 token**

`providers/anthropic.rs` 的 `request.max_tokens.unwrap_or(4096)` 是生产路径上的
唯一取值——`lattice-agent` 构造 `ChatRequest` 时 `max_tokens` 恒为 `None`。长回复
因此被无条件截断为 `finish_reason: "length"`，而 agent loop 对该终局没有续写处理。
修法：提为具名常量 `DEFAULT_MAX_OUTPUT_TOKENS = 32_768`。可以用平坦常量而不需要
能力表，是因为 `ModelRouter` 只把三个封闭别名（`opus`/`sonnet`/`haiku`，均为
Claude 4.x）映射到该 provider，其余一律 `UnknownModel`，任意模型串只能走
OpenAI-compatible。按 issue 要求未引入 ModelSpec 能力表。

**Verification:**

- 新增 `logging::tests::default_level_diagnostics_go_to_stderr_not_stdout`：
  因为 `init_logging` 装的是进程级 subscriber，测试用 `--exact` 过滤重新 exec
  测试二进制（沿用 #144 的自 re-exec 模式），在子进程里发一条 ERROR，断言哨兵
  出现在 stderr 且**不**出现在 stdout。
- 新增 `logging::tests::ansi_requires_a_terminal_and_respects_no_color` 覆盖四
  种输入组合。
- 新增 `providers::anthropic::tests::default_max_tokens_is_not_the_legacy_4096_cap`，
  并先断言 `req.max_tokens.is_none()`，确保它测的是生产走的那条默认路径；既有
  `build_request_includes_model_and_max_tokens` 改为对常量断言。
- 反向验证：移除非 verbose 分支的 `.with_writer(io::stderr)` → stderr 测试
  FAILED（哨兵泄漏到 stdout）；把常量改回 `4096` → 上限测试 FAILED
  （`default output ceiling regressed to the truncating 4096`）；两者恢复后 ok。
- `cargo test -p lattice-llm` — 138 + 23 passed。
- `cargo clippy -p lattice-llm --all-targets -- -D warnings` — clean。
- `cargo fmt --all --check` — clean。

**Changed files:** `crates/lattice-llm/src/logging.rs`,
`crates/lattice-llm/src/providers/anthropic.rs`, `crates/lattice-llm/Cargo.toml`
（`tracing` 加入 dev-dependencies，仅供该测试发日志）, `Cargo.lock`,
`crates/lattice-llm/AGENTS.md`, `FINDINGS.md`, `FIX.md`.

## GitHub #88：test-utils 不再从 lattice-cli 生产依赖段进入发布二进制 — 2026-08-22

**Severity:** LOW（发布面 / 坏范式）

**Problem:** `crates/lattice-cli/Cargo.toml` 的生产 `[dependencies]` 段写着
`lattice-session = { workspace = true, features = ["test-utils"] }`，直接抵消了
`lattice-session/src/lib.rs:26-29` 的 `#[cfg(any(test, feature = "test-utils"))]`
门禁——会创建 git 仓库的 `init_fixture_repo` / `init_empty_repo` 因此进入发布
二进制 `lattice` 的依赖图。该 crate 对这些 helper 的全部使用点都在 `#[cfg(test)]`
里，而 `#[cfg(test)]` 单测本来就能看到 `[dev-dependencies]`，生产段从来不是到达
它们的必要路径。issue 正文提到的 `lattice-agent` 那条此前已修，本次只剩这一条。

**Fix:**

- `crates/lattice-cli/Cargo.toml`：生产段 `lattice-session` 去掉 `features`，
  dev 段那条改为 `features = ["test-utils"]`。
- 新增门禁 `check::production_test_feature_hits`，接进 `cargo xtask
  lancet-boundary` 的分组列表（该命令已在 `cargo xtask ci` 第 6 步与
  `ci.yml` / `full-gate.yml` 两条 lane 里，无需新增 CI 接线）。它读
  `cargo metadata --no-deps`，对**任意** workspace 成员的 `Normal` kind 依赖，
  只要启用了以 `test-` 开头的 feature 就报错。按前缀而非枚举清单判定，所以将来
  新增的 `test-*` feature 自动被覆盖。

**Verification:**

- `cargo metadata` 全量扫描：工作区内 Normal kind 依赖启用 test 类 feature 的
  条目为 0（修复前 1 条）。
- `cargo tree -p lattice-cli -e features --no-dev-dependencies | rg test-utils`
  — 无输出（修复前命中 `lattice-session feature "test-utils"`）。
- 反向验证：把 `features = ["test-utils"]` 加回生产段后
  `cargo xtask lancet-boundary` 退出码 1 并报
  `lattice-cli: [dependencies] lattice-session enables test-only feature(s)
  test-utils`；移除后退出码 0。
- 新增单测 `production_test_feature_flagged_only_in_dependencies_section`
  （同一 crate 两个 section 都启用，只有生产段计为 1 条 finding）与
  `dev_only_test_feature_is_clean`。
- `cargo test -p xtask lancet_boundary` — 16 passed。
- `cargo test -p lattice-cli` — 132 + 4 passed；`cargo build -p lattice-cli` 通过。

**Changed files:** `crates/lattice-cli/Cargo.toml`,
`xtask/src/lancet_boundary.rs`, `xtask/src/lancet_boundary/check.rs`,
`crates/lattice-session/AGENTS.md`, `FIX.md`.

## GitHub #90：provider HTTP 客户端禁用重定向 — 2026-08-22

**Severity:** HIGH (`safety`)

**Problem:** `crates/lattice-llm/src/client.rs` 的 `http_client_builder()` 只设三个
超时，从不调用 `.redirect(..)`，而 reqwest 0.13 的默认策略是 `Policy::limited(10)`。
本 crate 的全部网络防护——`validate_base_url`、`host_is_dns_rebinding_domain`、
以及 `build_http_client_for_url` 里的 `resolve_to_addrs` 钉扎——都只绑定**首跳**，
所以一次 3xx 就能把请求送到未校验的主机：私网/回环/云 metadata 端点全部可达，
307/308 还会重放整个请求体（system prompt + 对话历史 + agent 刚从工作区读出的
文件内容）。跨 host 时 reqwest 只剥离 `Authorization`/`Cookie` 等五个头，
Anthropic 的 `x-api-key` 与 Gemini 的 `x-goog-api-key` 会被原样转发。
同仓库另外两个出网客户端（`lancet-native` 的 `executor/web.rs:191`、
`handlers/external.rs:29`）早已是 `Policy::none()`，LLM 通道是唯一漏项。

**Fix:** `http_client_builder()` 增加 `.redirect(reqwest::redirect::Policy::none())`，
与 `fetch.web` 两个客户端对齐，并在函数上记录这条约束的理由。未额外加
`https_only`：`net_guard::validate_base_url` 已对非 localhost 主机拒绝明文 HTTP，
关掉重定向后不存在可降级的第二跳。

**Verification:**

- 新增 `client::tests::provider_client_does_not_follow_redirects`：两台本地
  `TcpListener`，第一台回 `307` 并把 `Location` 指向第二台，断言调用方看到的是
  307 且第二台从未被连接。
- 反向验证：移除 `.redirect(..)` 后该测试 FAILED（`left: 200, right: 307`，即
  重定向目标真被访问到）；恢复后 ok。
- `cargo test -p lattice-llm` — 134 + 23 passed。
- `cargo clippy -p lattice-llm --all-targets -- -D warnings` — clean。
- `cargo fmt --all --check` — clean。

**Changed files:** `crates/lattice-llm/src/client.rs`,
`crates/lattice-llm/AGENTS.md`, `FINDINGS.md`, `FIX.md`.
## GitHub #139-A: non-production InvocationStore + collision-aware facts contract — 2026-08-17

#139-A implements the non-production InvocationStore on the `lattice-store`
kernel and closes L-GH144-01 (clean UUID collision unmodellable with synthetic
facts). Does not connect to the production Tool dispatch, JSON-RPC, SQLite, or
operator Adapter paths — those remain in #139-B.

| Component | Change |
|---|---|
| Facts contract | `InvocationFact` → `InvocationLifecycleFact`; new `InvocationRecordFact { owner: PlanKey, lifecycle }` with mandatory owner (D-A1). `InvocationFactsSnapshot.invocations` now `BTreeMap<InvocationId, InvocationRecordFact>`. `state_of` → `lifecycle_of`. |
| Collision detection | `validate_against_plan` returns `Result<PlanValidation>` with `AbsentPlan / Collision / ExactMatch`. Truth table D-A2 (7 rows) fully covered. |
| Reconcile | `binding_window_closure` handles `Collision` → `ReviseBindingCandidates`. `reconcile()` short-circuits on collision (no TurnClosing). `step_closing_closure` requires `ExactMatch`. |
| InvocationStore | Single append-only ledger (`refs/lattice/invocations/ledger`) on `StoreRepo`. `reserve_batch` (all-or-nothing, idempotent), `dispatch_started`, `publish_terminal`, `query_invocations`. CAS publication with fsync barriers. |
| Boundary tests | `a_different_durable_reservation_stays_fail_closed` replaced by `collision_at_binding_planned_outputs_revise_candidates` (D-A2 #2), `different_durable_reservation_is_corruption` (#7), `exact_durable_reservation_does_not_re_reserve` (#4). |
| Dependencies | `lancet-audit -> lattice-store`, `lancet-audit -> gix` (workspace). |

**Verification:**

- `cargo test -p lattice-session` — 155 passed.
- `cargo test -p lancet-audit` — 94 passed (11 new invocation_store tests).
- `cargo check -p lattice-api -p lattice-cli` — clean.

**Changed files:** `crates/lattice-session/src/pending/{facts,reconcile,mod}.rs`,
`crates/lattice-session/src/git_graph/tests/pending_crash.rs`,
`crates/lancet-audit/src/{invocation_store,lib}.rs`,
`crates/lancet-audit/Cargo.toml`,
`crates/lancet-audit/AGENTS.md`,
`docs/plans/2026-08-17-issue139a-decision-record.md`,
`FINDINGS.md`, `FIX.md`.

## GitHub #144 review round 2: journal contract depth + real process-crash gate — 2026-08-17

Independent review of the #144 branch raised eleven findings (F1–F11). Each was
re-checked against live issue #144, ADR 0005, and the source before any change;
seven produced code or test fixes, one was refuted, and three are recorded as
deferred/handoff items. Scope is unchanged: session-side only, synthetic
Invocation facts, no production InvocationStore, Tool dispatch, FinalizeBudget
approval, or JSON-RPC/numeric-ID/SQLite/operator/dispatch path.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| #144-R1 | MEDIUM | AC#8 asks for target migration tests proving the legacy `WorkspaceDisposition`/`TurnLifecycleEvidence`/terminal-string fields carry no authority; only a single `terminal_reason == None` assertion existed. | Added migration coverage over all four `TurnOutcome` variants asserting the persisted manifest's `disposition`, `turn/audit.json` lifecycle evidence, and `terminal_reason` are a total projection of the typed outcome, plus a shape guard that `TurnCommitCandidate` can never grow those fields. Production deletion stays in #139-B (ADR 0005 §15/§20). |
| #144-R2 | MEDIUM | AC#12 requires process-crash/kill points; the branch shipped only injected durability faults plus fresh-handle reopen. ADR 0005 §18 names SIGKILL/reopen and injected barriers as *two* mechanisms, and #143 already ships a real SIGKILL fixture. | Added `sigkill_leaves_only_the_old_tip_or_a_complete_new_one`: a self-re-exec child drives full Turn lifecycles in a temp state root and is SIGKILLed at three delays; a strict reopen must show a contiguous Turn-pin prefix, a branch head equal to the last pin, and any surviving `active-turn` exactly the next Turn with one deterministic closure. No power-loss claim. |
| #144-R3 | MEDIUM | `validate_transition` accepted any legal attempt key after `StepClosed(ContinueModel)`: no step contiguity, no zero first attempt, and an empty `ordered_tool_results` left the previous step ordinal underivable. | `StepClosedKind::ContinueModel` now carries `model_step_ordinal`. It must be `< MAX_MODEL_STEPS`, non-empty closures must all address that step, `StepClosing -> StepClosed(ContinueModel)` must name the closed plan's step, and the next `ModelAttemptStarted` must be exactly `ordinal + 1` attempt 0. The store transition API can no longer write a non-contiguous edge. |
| #144-R4 | LOW | `BindingPlan::validate` checks turn/step but not session, and a binding digest is self-consistent over its own tuple, so a cross-session address survived in the journal. | `PendingSnapshot::validate` — the layer that knows the expected `SessionId` — now cross-checks every planned/closing/closure binding address, the pinned `ResultDraft` address, and the pinned `TurnCommitCandidate` session, failing closed with `JournalCorruption`. |
| #144-R5 | LOW | Strict open parsed the `active-turn` object and compared only its session, and accepted any `turns/<TurnId>` pin carrying a manifest. | Strict open now runs the full `PendingSnapshot::validate()` on `active-turn` and verifies the pinned commit's manifest names this session and this TurnId. Journal damage is reported as typed `SessionError::JournalCorruption` before the handle is returned, never repaired. |
| #144-R6 | LOW | Nothing compared a snapshot's payload `predecessor` with the commit's git parent, so a forged chain with two different lineages read cleanly. | Every journal read validates lineage: a non-root snapshot's single git parent must equal its payload predecessor, and the `TurnStarted` root's parent must be exactly its frozen `parent_conversation_tip` (or absent). Covers the `Committed -> TurnClosing` link, which is read through the same path. |
| #144-R7 | MEDIUM | `advance_pending_turn` published by CAS and then ran the ref durability barrier; a barrier failure returned `Durability` on an already-published snapshot, and the identical retry failed as an illegal transition (`Conflict`) instead of confirming its own publication. | The exact retry now reconstructs the candidate commit from the tip's own recorded predecessor and confirms `active-turn` points at that exact OID before re-running the barrier and returning the published tip. A different candidate, a different pinned commit time, or a CAS race is never accepted as success. |
| #144-R9 | LOW | `build_candidate`'s comment claimed a snapshot without `recorded_at` fails closed; the code silently fell back to `root.accepted_at`. | `reconcile` now fails closed with `JournalCorruption` when the tip carries no journal committer time, and when that time is not a representable instant. No new clock is introduced. |
| #144-R10 | INFO | FIX.md's #144 entry reported `cargo test -p lattice-store` as 50 passed. | Correction, recorded here rather than by rewriting the original entry: the real total is **49** (44 unit + 5 integration + 0 doc), re-measured twice on this worktree. The `lattice-session` figure of 132 was correct at that time and is now 148 with this round's sixteen new tests. |

**Refuted (no change):** F8 — `finalize_turn` stamping `SessionState.updated_at`
with a fresh clock is compliant. ADR 0005 §6 freezes exactly the `Committed`
and `TurnCommit` candidates, both of which are rebuilt byte-identically from
the pinned candidate on every retry; the state object is not part of that
frozen identity, and a failed ref transaction leaves it unreachable from any
authoritative ref.

**Deferred (see FINDINGS.md):** F11 — ADR 0005 §9's clean-UUID-collision
revision path needs InvocationStore facts that record which plan owns a
recorded invocation. The synthetic facts vocabulary cannot express that, so a
different reservation stays typed corruption; a boundary test now pins that
semantics explicitly for the #139-A handoff.

**Verification (this worktree):**

- `cargo fmt --all --check` — clean.
- `git diff --check` — clean.
- `cargo test -p lattice-store` — 49 passed.
- `cargo test -p lattice-session` — 148 passed.
- `cargo test -p lattice-api` — 171 passed.
- `cargo clippy -p lattice-session -p lattice-store --all-targets` — clean.
- `cargo xtask lancet-boundary` — pass.
- `cargo build -p lattice-cli` + `LATTICE_BIN="$PWD/target/debug/lattice" cargo test --workspace` and `cargo xtask ci` — pass.
- Every new guard was verified red-before-green by neutralizing it and
  confirming exactly the new tests fail.

**Changed files:** `crates/lattice-session/src/pending/{contract,step,reconcile}.rs`,
`crates/lattice-session/src/git_graph/{mod,pending}.rs`,
`crates/lattice-session/src/git_graph/tests/{pending_journal,pending_crash}.rs`,
`crates/lattice-session/AGENTS.md`, `FINDINGS.md`, `FIX.md`.

## GitHub #144：durable PendingTurn journal 与确定性 reconciliation — 2026-08-16

Scope: feature implementation (ADR 0005 §9/§12/§14, ordered #141 → #142 →
#143 → #144 → #139). Session-side only and validated with synthetic
Invocation facts: no production InvocationStore, Tool dispatch,
FinalizeBudget approval, or production JSON-RPC/numeric-ID/SQLite/operator/
dispatch path was touched.

**What landed:**

- `lattice-session::pending` (new module): the versioned `PendingSnapshot`
  envelope and the twelve durable `PendingStage` states with pure
  `validate_transition` (retry only after a pre-stream retryable failure of
  the next ordinal; `Bound` requires the byte-equal exact plan; the
  `StepClosing` cursor advances one ordinal at a time, pin-then-close or a
  not-executed closure without a candidate; `Committed` is never publishable
  through `active-turn`). `TurnStarted` freezes parent tip/user message/
  TurnId/profile+policy/model/system/catalog/limits with a 1 MiB
  exact-boundary catalog snapshot, a deterministic credential-key denylist,
  and the code-defined `MAX_MODEL_STEPS = 32`. `BindingPlanned` carries the
  exact ordered bindings with per-binding digests (no `PlanId`, no batch
  digest, no provider metadata in binding tuples); there is no Tool-bearing
  `MessageComplete` gap. `ResultDraft` is material-only (65,536-byte canonical
  ToolContent; plus-one rejected, never truncated) with the
  `Durable | CleanAbsent | Uncertain` classification. `StepClosed` is the
  closed `FinishTurn | ContinueModel | FailTurn | CancelTurn | InterruptTurn`
  sum; `TurnOutcome`/`ClosureOrigin` are the typed outcome authority with a
  legacy-manifest projection for today's readers.
- `git_graph/pending.rs`: journal persistence over the `lattice-store`
  kernel. `refs/lattice/sessions/<id>/active-turn` points directly at the
  current snapshot commit; the chain root grafts onto the frozen conversation
  tip. Draft publication is a content-addressed blob with a durable barrier;
  reopen classification is a read-only lookup (`lattice-store` gained
  `blob_id`/`has_object` read primitives). `finalize_turn` is the one
  all-or-nothing kernel `ref_transaction` (branch/detached advance +
  `turns/<TurnId>` pin + `state` update + `active-turn` delete), building the
  `Committed` journal object and the conversation TurnCommit (git parents
  `[Committed] ++ side parents`, explicit TurnId in the manifest) from the
  pinned candidate only, so retries are byte-identical and idempotent.
  Transcript walks skip journal snapshots; strict open validates the new
  refs.
- `pending::reconcile`: `reconcile(PendingSnapshot, InvocationFactsSnapshot)`
  decides the one closure of every durable stage from durable facts only.
  `BindingPlanned`+absent closure-only reserves the exact candidates and
  marks them `NotExecuted(BeforeDispatch)`; `DispatchStarted` without a
  terminal is conservatively `Indeterminate(MayHaveOccurred)` with the
  remaining bindings `NotExecuted(PrecedingInvocationIndeterminate)`;
  `StepClosing` resumes at its exact lowest unclosed ordinal (available
  material must match the pinned candidate); `StepClosed(ContinueModel)`
  closes interrupted — reopen never continues model/Tool execution.
  Reconciled candidates pin the durable tip committer time (no new clock)
  and a conservatively `Partial` usage aggregate; the synthetic facts
  vocabulary lives in `lattice-session` (production InvocationStore maps
  into it, #139-A/#139-B).
- Dependency delta (ADR 0005 §19): `lattice-session -> lancet-protocol`.
  `SessionError::JournalCorruption` is the typed journal-damage error.
- Crash fixtures: stage matrix × fresh strict reopen × determinism; injected
  faults keep the previous tip on an advance crash, keep `StepClosing +
  active-turn` while the store is unwritable (the FinalizeBudget-exhaustion
  shape) with the next command's plan exactly one idempotent `FinalizeTurn`,
  and demonstrate post-publication ref-fsync failure with an idempotent
  retry; draft windows close from classified material; executing a plan
  end-to-end commits the recovered Turn with the typed journal chain
  reachable from its permanent pin.

**Verification:**

- `cargo test -p lattice-session` — 132 passed (journal, reducer, crash
  fixtures, and the pre-existing graph/store suites).
- `cargo test -p lattice-store` — 50 passed (incl. the new
  `blob_id`/`has_object` primitive test).
- `cargo clippy -p lattice-session -p lattice-store --all-targets` — clean.
- `cargo test --workspace` / `cargo xtask ci` — run before push (see
  branch).

**Changed files:** `crates/lattice-session/src/pending/**` (new),
`crates/lattice-session/src/git_graph/pending.rs` (new),
`crates/lattice-session/src/git_graph/{mod,commit}.rs`,
`crates/lattice-session/src/git_graph/tests/{pending_fixtures,pending_journal,pending_crash}.rs`
(new), `crates/lattice-session/src/{lib.rs,Cargo.toml}`,
`crates/lattice-store/src/repo.rs`,
`crates/lattice-api/src/session.rs` (exhaustive-match mapping of the new
`JournalCorruption` variant), `AGENTS.md`,
`crates/lattice-session/AGENTS.md`,
`docs/plans/2026-08-16-issue144-pending-turn.md` (new).

## GitHub #142：建立 InvocationId 与 lattice-conversation 纯数据 contract — 2026-08-13

Scope: feature implementation (ADR 0005 §1/§7 prerequisite, ordered #141 → #142
→ #143 → #144 → #139). Additive only: no production JSON-RPC/transport/
events/cancellation/SQLite/operator/WorkspaceHistory/dispatch path was
touched, and no second identity seam was introduced.

**What landed:**

- `lancet-protocol::identity`: strict `InvocationId` / `TurnId` (lowercase
  hyphenated UUID v4; rejects uppercase, unhyphenated, non-v4, non-RFC-4122
  variant spellings) and `SessionId` (32 lowercase hex, matching the existing
  `Uuid::new_v4().simple()` production shape). Parse/display/serde/schemars
  only — no generation, no I/O, no tokio, no String alias, no second newtype.
- `lancet-protocol::address`: provider-neutral structured addresses
  (`ModelMessageAddress { SessionId, TurnId, model_step_ordinal }`,
  `ToolCallAddress { message, part_ordinal }`, `ModelAttemptKey`, `PlanKey`).
  Placed in protocol because #139-A's `lancet-audit` InvocationStore consumes
  binding tuples without a `lattice-conversation` edge (ADR 0005 §19).
- New data-only crate `lattice-conversation`: ordered
  `NativeMessage<ToolCallState>` / `MessagePart` (outer struct: semantic +
  optional bounded part-local `OpaqueProviderState`, 64 KiB cap, never
  filtered even on empty parts), `UnboundToolCall`/`BoundToolCall` typestates,
  `ToolResultOutcome` closed sum, distinct `semantic_digest` /
  `replay_state_digest`, strict `validate_tool_call_batch` (arguments must be
  one JSON object; empty string/array/scalar/malformed/duplicate/incomplete
  fail closed; no empty-to-`{}` normalization), canonical `arguments_digest`
  (SHA-256 over domain separator + u64_be byte length + canonical bytes),
  per-attempt `Option<TokenUsage>` (missing = unknown, never zero), typed
  `AttemptInterruption`/`AttemptRejection`, and protocol hard limits (8 MiB
  arguments / 2 MiB assistant / 2 MiB reasoning / 16 MiB attempt; plus-one
  produces a typed `OutputLimitExceeded`, never partial conversation).
- `lattice-llm::provider_projection` (new pure-data seam, not wired into the
  production codecs): official wire JSON ↔ `NativeMessage` for
  OpenAI-compatible / Anthropic / Gemini, preserving part order, real
  provider call IDs, Anthropic thinking signatures, and Gemini
  thoughtSignatures in place (including on empty text parts); same-provider
  replay is fixture-exact; cross-provider projection drops all replay state
  and provider call IDs (verified byte-level non-leakage); missing provider
  IDs stay `None` and no `tc_*` synthetic IDs exist.
- Conversation pure-data ownership migration: `GraphMessage` / `GraphToolCall`
  / `HeadTranscript` / `TurnUsage` moved verbatim (serde shape frozen) into
  `lattice-conversation` and re-exported by `lattice-session`; `lattice-agent`
  now depends on `lattice-llm` + `lattice-conversation` only (verified zero
  `lattice-session` edges via `cargo tree`). `TurnUsage` is a rename of the
  new provider-neutral `TokenUsage { u64 × 3 }`; `lattice_llm::TokenUsage`
  (u32) stays for the wire with the existing checked widening at the Agent
  boundary (#139-B unifies them).
- Root `AGENTS.md` graph/roles, `docs/ARCHITECTURE.md` layer model + mermaid,
  and crate AGENTS files updated; plan doc
  `docs/plans/2026-08-13-issue142-invocation-conversation.md` records the
  acceptance mapping and the four grill decisions.

**Verification:**

- `cargo test -p lancet-protocol` (83), `-p lattice-conversation` (29),
  `-p lattice-llm --test provider_projection` (14 fixture tests), migration
  regression `-p lattice-agent -p lattice-session -p lattice-api`.
- `cargo tree -p lattice-agent --prefix none | grep -c lattice-session` → 0.
- `cargo xtask ci` — all checks passed (fmt, clippy `-D warnings`, builds,
  boundary gates, per-crate tests, tool-build verify).

**Changed files (high level):** new `crates/lattice-conversation/` crate;
`crates/lancet-protocol/src/{identity,address}.rs` + lib wiring; new
`crates/lattice-llm/src/provider_projection/` + `tests/provider_projection.rs`
+ `tests/fixtures/projection/*.json`; ownership edits in
`crates/lattice-session/src/graph.rs` (re-export) and
`crates/lattice-agent/` (dependency swap); workspace `Cargo.toml`
registration; root `AGENTS.md`, `docs/ARCHITECTURE.md`, crate AGENTS files.

### 审查修复轮次 — 2026-08-13

独立只读审查（verdict: REQUEST CHANGES）提出 1 个 P0、1 个 P1、4 个 P2、1 个 P3。
本轮逐条修复，并补齐审查判定为「部分满足」的 2 条验收标准。

- **P0-1 cross-provider projection 重放 source reasoning**
  （`provider_projection/mod.rs::project_cross_provider`）。原实现只清除
  `replay_state` 与 `provider_call_id`，保留 `MessageSemantic::Reasoning`，
  经 `native_to_wire` 后 Anthropic 的思维链会原样出现在 OpenAI 的
  `reasoning_content` 与 Gemini 的 `thought: true` part 中，违反 ADR 0005 §7
  「cross-provider projection does not replay source reasoning」。修复：投影时
  `retain` 掉全部 Reasoning part。原有两处 `assert_eq!(semantic_digest(...))`
  实际上把「reasoning 幸存」错误固化为期望，已改为断言投影不含 Reasoning 且
  digest 必然不同；另补 `cross_provider_projection_preserves_non_reasoning_semantics`
  证明非 reasoning 语义仍无损跨越。
- **P1-1 seam 绕过 protocol hard limits**。`AttemptAssembly` 此前在模块外零调用方，
  三个 `wire_to_native` 只继承 per-call 8 MiB 限制。修复：新增
  `ProjectionError::OutputLimitExceeded { provider, interruption }`，三个 projector
  边解析边经 `AttemptAssembly` 计量（text→`push_assistant`、
  reasoning/thinking→`push_reasoning`、每个 call 前 `begin_tool_call()` 后
  `push_tool_arguments`），任一 push 越限即 fail closed，绝不返回部分 `NativeMessage`。
- **P2-1 Anthropic text block 写出 `signature`**（`anthropic.rs`）。`signature` 只属于
  `thinking`/`redacted_thinking`；reader 对 text block 无读取分支，round-trip 会静默
  丢弃。修复：writer 遇到带 `replay_state` 的 Text part 返回 `UnsupportedPart`，
  与 OpenAI writer 已有守卫对齐。
- **P2-2 same-provider replay 的 arguments 归一化**（`openai.rs`）。canonical 化是
  ADR 0005 §7 的执行要求，但此前未被记录，且 6 个 fixture 全是 canonical 拼写导致
  `openai_same_provider_replay_is_exact` 空洞通过。修复：在模块 doc 中显式记录该有损
  归一化，并新增用非 canonical 拼写断言「语义等同、字节不等同」的测试。
- **P2-3 `BoundToolCall` digest 未校验**（`tool_call.rs`）。derive 的 `Deserialize`
  接受任意 64 位 hex，全零 digest 可与真实 arguments 并存。修复：改用
  `#[serde(try_from = "BoundToolCallWire")]`，反序列化时重算并比对，不一致返回
  `ArgumentsDigestMismatch`。序列化形状不变（仍是小写 hex）。
- **P2-4 OpenAI writer 合并交错 text part**。属 OpenAI 单一 `content` 字符串的固有
  限制，语义不变；修复为在模块 doc 中记录并新增测试固化该行为。
- **P3-1 `LimitKind` 未导出**。已加入 `lattice-conversation` 的 `pub use limits::{…}`。

**补齐的验收标准：**

- 「provider fixtures … A→B 不泄漏」：原测试只对 native 结构体断言，漏掉了 wire 这一层。
  新增 `cross_provider_wire_never_carries_source_reasoning`，对
  Anthropic→OpenAI、Anthropic→Gemini、Gemini→OpenAI 三组执行
  `wire_to_native → project_cross_provider → native_to_wire`，断言产出的 wire 中
  不含 source reasoning 正文、不含 `reasoning_content`、不含 `"thought"`。
  已验证该测试在移除 P0 修复后失败、恢复后通过。
- 「protocol hard-limit fixtures；plus-one 产生 typed interruption 而非 partial
  conversation」：新增 4 个 seam 层限额测试，覆盖 assistant / reasoning 的精确边界与
  plus-one、16 MiB attempt 总量（跨两个 tool call，避开 per-call 预算）的精确边界与
  plus-one，以及 Anthropic/Gemini 的 per-call arguments 限额，全部断言返回 `Err`
  而非部分消息。限额数据在测试中程序化构造，不落盘为 fixture。

**本轮验证：** `cargo test` — lancet-protocol 83、lattice-conversation 31（+2）、
lattice-llm 133 + provider_projection 23（+9）、lattice-session 54、lattice-agent
8/4/11；`cargo xtask lancet-boundary` 7/7 PASS；`cargo xtask ci` all checks passed。

**留给 #139-B 的两条前置约束**（两轮审查共同指向，已写入
`crates/lattice-conversation/src/digest.rs` 与 `AGENTS.md`，此处存档）：

1. `AttemptAssembly` 的限额计量是约定式的，没有机器门禁。#139-B 接入 production
   codec 时，任何新的 attempt 组装路径都必须经它计量，否则 P1-1 会以另一种形式复发。
2. binding digest 必须对**存储的原始 message**（含 reasoning）计算，绝不能对
   `project_cross_provider` 的输出计算 —— 后者按 ADR 0005 §7 有意丢弃 reasoning 与
   replay metadata，对其取 digest 会把 Turn 绑定到一条 provider 从未产出的 message。

## GitHub #99：工作区 `.env` 的信任边界 — 2026-07-31

Severity: HIGH（策略绕过 + 对话正文与凭据外泄，前置条件仅「在不受信任的仓库目录里跑一次
`lattice`」）。本票只做 issue 定义的阶段 1；阶段 2（只读 `$HOME/.lattice/.env` + `--env-file`）
仍属破坏性变更，未在此落地。

**Problem:**

`crates/lattice-cli/src/main.rs:79` 无条件 `dotenvy::dotenv().ok()`，把**当前工作目录**
（及其祖先目录）的 `.env` 注入进程环境。而两个高权限开关从环境变量读取：
`LATTICE_AGENTS_DIR`（`commands/run.rs:527`）决定 agent profile 目录，即 system prompt、
`[tools].enabled`、以及全部 `[lancet]` 策略段（写 deny 名单、网络策略、observation 限制）；
`OPENAI_COMPATIBLE_BASE_URL`（`lattice-llm/src/router.rs:84`）决定 LLM 端点，而 net-guard
只拦私网/保留地址，攻击者的**公网**网关照常放行，会话正文与 API key 随请求送出。
`validate_agents_dir` 只拒绝 `/etc /usr /var /sys /proc /dev /boot /tmp` 这类绝对路径，
仓库内相对路径一律放行。这条路径不需要用户执行仓库里的任何代码。

**Fix summary:**

- 新增 `crates/lattice-cli/src/env_boundary.rs`：仍加载工作区 `.env`（把 API key 放在
  `.env` 是它的正当用途），但按**模式**而非枚举清单无条件丢弃五类键 —— `LATTICE_*`、
  provider endpoint（`*_BASE_URL` / `*_API_BASE` / `*_API_URL` / `*_ENDPOINT`）、`*_PROXY`、
  `HOME`/`XDG_*`、`SSL_CERT_FILE`/`SSL_CERT_DIR`。用模式的理由是将来新增的
  `LATTICE_*` 或 `*_BASE_URL` 变量当天即被覆盖，无需回来改边界。匹配大小写不敏感：Unix 的
  环境变量名区分大小写，而 `https_proxy` 这类小写拼法确实会被读到，区分大小写会留下绕过口。
- 后两类是本次自查（而非 issue 原文）新发现的同类载荷，均已独立验证依赖链：`*_PROXY` 由
  `hyper-util-0.1.20/src/client/proxy/matcher.rs:230-233` 默认读取（`reqwest` 的
  `auto_sys_proxy`），改流量与改 base URL 等效；`SSL_CERT_*` 在 Linux 下经
  `reqwest[rustls]` → `rustls-platform-verifier-0.7.0/src/verification/others.rs:92` →
  `rustls-native-certs-0.8.4`（`lib.rs:363-364`）生效，且任一变量被设置时它**替换**而非
  追加平台信任库 —— 比端点改写更强的 MITM 原语。
- 进程环境不受这些规则约束：在 shell / CI / systemd unit 里 export `LATTICE_AGENTS_DIR`
  是用户自己的授权，被检出的仓库不是。`.env` 也一律不覆盖已存在的进程环境值（沿用旧
  `dotenv()` 的 non-override 语义），落败的键记为 `shadowed`。
- 丢弃在启动时**静默**：敌意 `.env` 不该获得打印任何东西的机会，善意 `.env`（开发者在里面
  写了 `HTTPS_PROXY`）也不该让每条命令都变成告警。可观测性交给 `lattice doctor`：新增
  credential/endpoint 的 `source` 列（`process` / `dotenv` / `unset`）、Endpoints 段、以及
  Workspace `.env` 段（路径 / applied / shadowed / 每个被丢弃键及其原因 / 解析错误）。
- `plan_dotenv` 是纯函数缝：入参是文件字节 + `already_set` 谓词，返回 report 与待安装的键值
  对，`set_var` 循环之外不做任何决策。这样优先级、BOM、解析失败三条语义都能在不改动测试
  进程环境的前提下单测（`set_var` 只在 tokio runtime 之前的单线程窗口调用，`main` 因此拆成
  `main` + `#[tokio::main] async_main`）。
- 顺带修的两处（均由对抗性验收发现）：(1) 手工迭代 `dotenvy` 绕过了它私有的 `remove_bom`，
  带 UTF-8 BOM 的 `.env` 会在第 0 行解析失败并**静默丢弃整个文件** —— 相对旧
  `dotenvy::dotenv()` 是回归，现显式剥离 BOM；(2) `doctor` 打印 endpoint 值（URL 本身不是
  秘密），但代理 URL 常带 `user:password@`，现经 `redact_userinfo` 统一脱敏为 `***@`，主机
  仍可见。另外 credential 行原先只来自「能解析的 alias」，而裸 `openai-compatible` alias 需
  要 `OPENAI_COMPATIBLE_MODEL` 才能解析，导致只在 `.env` 里放 compat key 的用户看不到那一
  行的来源；现四个 provider credential 无条件成行。

**Verification:**
- `cargo fmt --all --check` — 通过。
- `cargo clippy --workspace --all-targets -- -D warnings` — 通过，无告警。
- `LATTICE_BIN=... cargo test --workspace` — **1607 passed / 0 failed**（基线 1593）。
- `cargo xtask harness-boundary` / `lancet-boundary` / `tool-codegen --check` — 均通过。
- 新增测试：`env_boundary.rs` 13 个单测（五类拒绝各一、凭据仍放行、被拒键不入 apply 列表并
  带正确 reason、shadowed 不覆盖、BOM 不丢文件、畸形行记 error 并停止解析、重复键取首次、
  `export`/引号拼法仍被拒、`${VAR}` 替换无法把被拒值洗进允许键）；`doctor.rs` 2 个
  （userinfo 脱敏、无 userinfo 的 URL 原样通过，含密码内含 `@` 与 authority 之后的 `@`
  两个边界）；`tests/fast_env_boundary.rs` 11 个黑盒场景。
- 黑盒场景以**差分**方式立论而非只看退出码：同一条 `session new` 跑两次，恶意值只在 `.env`
  里时错误消息命名 HOME 派生目录、export 到进程环境时命名 `.tools/agents`，两个对照都断言。
  端点同理（`.env` → 内置默认值；进程环境 → 生效）。没有对照组的话，「`LATTICE_AGENTS_DIR`
  整体失效」也会让断言通过。
- 变异测试（改完即恢复，工作树已核对干净）：删 BOM 剥离 → `a_utf8_bom_does_not_discard_the_file`
  FAILED；删 `SSL_CERT_*` 规则 → 2 单测 + 2 黑盒场景 FAILED；删 non-override 守卫 →
  `a_key_already_in_the_environment_is_shadowed_not_overridden` + 黑盒
  `fast_dotenv_never_overrides_the_process_environment` FAILED；去掉代理脱敏 →
  `fast_doctor_redacts_proxy_credentials` FAILED；把 `main` 换回 `dotenvy::dotenv()` →
  多个场景 FAILED。
- 对抗性验收（独立 subagent 跑 cargo + 源码变异 + 逐条验收标准复核）发现并已修：BOM 回归、
  `SSL_CERT_*` 缺口、non-override 规则零覆盖、代理密码回显、compat key 无 source 行。被裁决
  丢弃的申诉：「首个畸形行后丢弃余下键」（旧 `dotenv()` 同语义，非回归）、「大小写规则的
  黑盒层未再覆盖一次」（三个单测已两侧覆盖）。
- 非目标已核对未被触碰：未移除 `.env` 支持、未收紧 `validate_agents_dir` 的绝对路径规则、
  未改 net-guard 私网判定、未引入 `--env-file` 或 `$HOME/.lattice/.env`。

**Changed files:**
`crates/lattice-cli/{AGENTS.md,src/main.rs,src/env_boundary.rs,src/commands/doctor.rs}`,
`crates/lattice-system-harness/{AGENTS.md,src/world.rs,tests/fast_env_boundary.rs}`,
`docs/CONFIGURATION.md`, `docs/GETTING-STARTED.md`, `FIX.md`.
## GitHub #30：`observe.workspace` 迁为 Policy-Visible Workspace Component Tool — 2026-07-31

Severity: ENHANCEMENT（ADR 0004 五个内置 Tool 的第二个迁移切片；含三处迁移中对抗
验收发现并已修的真实缺陷，见下）。

**Problem:**

`observe.workspace` 的 read/list/grep/find 四种 mode 跑在 native handler
（`lancet-native/src/handlers/observation.rs`）里，经 legacy policy hook 授权。ADR 0004
要求迁成独立维护的 WASM Component Tool：Component 只做 typed mode dispatch 与 result
shaping，全部 filesystem authority（System Path Exclusion、symlink/root containment、
read denial、ignored-path inclusion、bounded result、observation budget、effects/audit）
必须经 Tool Host workspace imports，且不得重新引入 ExecutionBackend/Upper/projection/
ambient WASI/native fallback 接缝。前置：#29（todo_write 迁移）、#112（versioned
evidence 表）、#24（PRD）均已落地。

**Fix summary:**

- **Local Workspace 下移一层**：`lancet-native/src/workspace/` 整体 git mv 到
  `lancet-runtime/src/workspace/`（`pub(crate)` 放宽为 `pub`，`new` 改
  `test-utils` feature 门控 —— pub 是 reach 不是 seam）。否决了注入式 port（违反
  no-pluggable-backend-seam）与 observation-only 半移（分裂 one concrete host module）
  两个方案。async 原语的闭包体抽成 `*_blocking` 同步方法，async 版委托 —— 一份实现
  同时服务 sync host import 与 async native handler。`lancet-native` 经
  `pub(crate) mod workspace { pub(crate) use lancet_runtime::workspace::*; }` 路径别名
  继续用同一具体类型；`lancet_native::rewind` re-export 保持稳定。分层图新增
  `lancet-runtime → lancet-authority` 边，无环。
- **Tool Host workspace imports（核心权限模块）**：新
  `lancet-runtime/src/host_workspace.rs` 的 `WorkspaceObservationHost` 实现 WIT
  `workspace-observe` interface（read-file/list-dir/grep/find-files 四函数，一个
  interface 保证四 mode 不可拆分链接）。每次调用：parse project-relative path → 经同一
  `authorize_operation` seam 授权 `alloc:read:workspace`（Cell{Alloc, Read, scope}）→
  turn-scoped budget charge → `VisibilityEngine` 可见性裁决（四 mode 共享同一 decision，
  隐藏路径报告为 missing）→ host 侧 `ObservationLimits` 字节/行/条数截断。
  `composition.rs` 的 `channels_for_domain_imports` 把 `workspace-observe` import 映射
  到 Workspace channel，组合门禁 fail-closed 不变。
- **Guest 只做 dispatch 与 shaping**：`tools/observe-workspace/`（no_std）按
  `mode` const 判别分发四分支，结果整形（行窗口 + `[TRUNCATED]` 标记、仅在 host 判定
  truncated 时写入标志）；`describe` 只转发 host 消息。manifest 顶层声明
  `imports = ["workspace-observe"]`，params/result 为 oneOf 四分支 union schema。
- **三态截断证据**：`ObservedEffect.truncated: Option<bool>`（None = 无边界 /
  Some(false) = 有界未截 / Some(true) = 截断），host 计算、guest 不可伪造；
  `skip_serializing_if` 保持 #30 前记录的序列化形状。audit 侧
  `EVIDENCE_SCHEMA_VERSION` 2→3，`evidence_effects` 加 `truncated INTEGER` 可空列。
- **错误直通与措辞契约**：host 拒绝经 `take_refusal`/`host_refusal` 以结构化
  `CepError` 直达调用方；`PathRejection` 结构化枚举 + `classify_path_error` 从
  `WorkspacePathError` 派生，钉住模型可见措辞（`path contains '..' traversal: 'x'` /
  `path must be relative: 'x'`）。
- **catalog 与 authority 同步摘除**：`lancet-catalog` 降为三个 native 方法
  （Create/Edit/Fetch，防 #28 §11 双授权词表冲突）；authority 删 `observe.workspace`
  policy arm 与 `extract_observe_workspace_path`（`resolved.rs` 将其钉为 unknown
  method）；native handler/registration/executor render/effect-contract observation
  fixtures 全部删除，覆盖去向在删除点注释说明。
- **迁移中对抗验收发现并已修的三处真缺陷**：(1) symlink 逃逸拒绝的错误消息泄漏宿主
  绝对路径（`/tmp/.tmpXXX/escape.txt`）—— `observation_io_error` 改为穷举匹配所有
  `LocalWorkspaceError` 变体，`SymlinkRejected` 用固定措辞，加防泄漏断言；(2) host
  结构化拒绝被 guest 包装成 `tool X guest error:` 前缀字符串、错误码退化为
  `ExecutionFailed` —— 加 `take_refusal`/`host_refusal` 直通通道，map guest logical
  result 失败时优先 host refusal；(3) v2 evidence 文件因缺 `truncated` 列被形状守卫
  整体拒绝 —— `truncated` 移出阻断守卫，走 `ALTER TABLE` 可加性迁移 + 迁移后断言列
  就位。
- **语义变化已钉档**：root 逃逸的错误码从 `GuardDenied`（legacy policy hook 归一化）
  变为 `PathContainment`（host 路径解析自报，与 native workspace 层对同类失败已用的
  代码一致），harness 场景 `direct_observation` 断言更新并写明理由。

**Verification:**

- `env -u LATTICE_BIN cargo xtask ci` — **全部通过**（fmt、clippy `-D warnings`、
  build、harness-boundary、lancet-boundary、tool-codegen `--check`、29 个 crate 测试、
  tool-build `--verify`）。注意：本机交互 shell profile 导出
  `LATTICE_BIN=/home/astrin/Dev/lattice/target/debug/lattice`（主仓库旧二进制），
  会让 harness 场景测到旧分发路径（`GuardDenied`/`cep_native`）——跑 harness/CI 前
  必须 `env -u LATTICE_BIN` 或显式指向 worktree 二进制。
- `cargo test -p lancet-runtime` 173 项（含 `host_workspace` 16 项新测试：四 mode
  共享可见性、ignored 默认隐藏+显式包含、denied read、symlink 逃逸+不泄漏宿主路径、
  `..` 词法拒绝、单读超限、list 截断 host 计算+evidence、untruncated Some(false)、
  budget 耗尽、外部 edit/create/delete 可见、content_hash、unadmitted cell、grep root
  target、invalid regex、clamp_limit、无 mutation history）。
- 全 workspace：lancet-catalog 26、lancet-protocol 68+、lancet-audit 88、
  lancet-authority 138、lancet-cep 8、lancet-native 103、lancet-api 9、harness 141 项全绿。
- No-WASI：`cargo tree -p lancet-runtime` 无 `wasmtime-wasi`/`wasi-common`；
  guest WIT 无 `wasi:` import（tool-codegen 测试钉住）。

**Changed files:**
`AGENTS.md`, `Cargo.{toml,lock}`, `FIX.md`,
`crates/lancet-api/src/{audit,runtime}.rs`,
`crates/lancet-audit/{AGENTS.md,src/{audit,audit_tests,evidence,evidence_tests,operator_tests,store_path}.rs,src/provenance/{attestation,projector}.rs}`,
`crates/lancet-authority/{AGENTS.md,src/guard.rs,src/policy/{default,resolved}.rs,tests/{guard_decide_fuzz,policy_coverage,policy_tests}.rs}`,
`crates/lancet-catalog/{AGENTS.md,src/{catalog.rs,methods/{mod,workspace}.rs}}`,
`crates/lancet-cep/src/tool.rs`,
`crates/lancet-native/{AGENTS.md,Cargo.toml,src/{lib.rs,executor/mod.rs,handlers/mod.rs,server/{native_registry,transport}.rs},src/effect_contract_tests/*,tests/{cep_component_round_trip,cep_warn_harness,policy_integration,prov_isomorphism}.rs}`
（删 `src/handlers/observation.rs`、`src/executor/render.rs`、`src/workspace/*`），
`crates/lancet-protocol/{AGENTS.md,src/evidence.rs}`,
`crates/lancet-runtime/{AGENTS.md,Cargo.toml,src/{admission,bindings,composition,evidence,host,host_workspace,lib,runtime}.rs,src/generated/host_composition.rs,src/workspace/*,tests/{call,embedded,negative}.rs}`,
`crates/lattice-api/src/turn/{cep,cep_tests}.rs`,
`crates/lattice-api/tests/{cep_round_trip,direct_local_acceptance,dogfood_impact_matrix,workspace_history_acceptance}.rs`,
`crates/lattice-cli/src/commands/audit.rs`,
`crates/lattice-system-harness/tests/direct_observation.rs`,
`tools/observe-workspace/`（新增）, `tools/{noop,todo-write}/generated/expected-identity.json`,
`xtask/src/tool_codegen.rs`.

## GitHub #120 / #121 / #122：写入上限、配置项读取点、host 匹配语义三处收口 — 2026-07-30

Severity: #120 MEDIUM（资源耗尽面 + 平行写路径契约分叉）、#121 MEDIUM（配置静默失效，
其中 `allowed_roots` 是「权限比预期宽」）、#122 LOW-MEDIUM（fail-closed 的可用性 +
一致性缺陷）。三者共同模式是同一语义存在多份实现或多个真相源。

**Problem:**

- **#120**：单文件写入字节上限只存在于 `mutate.edit`（`edit/apply.rs` 的
  `PatchApplyLimits`，1 MiB）。`mutate.create` 全链（`preflight_create` →
  `write_create` → `create_excl` → `write_all`）无任何字节数比较，而两个方法声明相同
  capability 与 `EffectScopeKind`。Direct-Local 下写操作直落用户工作区、无快照可回滚。
- **#121**：三组配置项被校验、被文档承诺，但执行侧无读取点。(1)
  `[lancet.observation].allowed_roots` 唯一读取者 `resolve_observation_target` 自
  `14ecf043` 起无调用者，真正的读范围只认 `project_root`，配置通过校验却不收窄，属静默
  的权限放宽；(2) 六个观测上限在 `BudgetLimits` 与 `ObservationPolicyConfig` 各存一份，
  靠三处手工复制同步，`merge_external` 分别写入两份后不再同步，导致外部策略文件的
  `[budget]` per-op 上限与 `[observation]` turn 上限各自落进无人读取的槽位；(3)
  `MethodPolicy` 五个字段只有 `enabled` 与 `max_todo_ops` 被消费，
  `max_single_read_bytes` 出生即死，`allowed_sources` / `required_capability` /
  `CapabilitySourcePolicy` 是 `ba3a8e27` 删除 envelope 授权路径后的残留，仍在校验用户输入。
- **#122**：`allowed_hosts` 匹配有三份实现两套语义。两处策略层支持 trim + 小写 + 前导点
  后缀匹配，授权 region 层（`effect/region.rs` 的 `contains`）只做原始字符串精确相等，
  于是 `allowed_hosts = [".example.com"]` 被前两层放行、被 region 层以
  `EffectViolation` 拒绝，且错误消息无法引导用户改对配置。

**Fix summary:**

- **#120**：上限收敛到新模块 `lancet-native/src/edit/limits.rs` 的 `WriteLimits`
  （1 MiB 常量 + `check_write_size`），替代 `PatchApplyLimits`。`mutate.create` 在
  `preflight_create` 的**第一条语句**校验请求内容长度，早于任何文件系统副作用（含
  `create_parents` 建目录）。两条路径经同一 `write_size_limit_error` 返回专属错误码
  `ToolResourceExhausted`（不再是通用 `ExecutionFailed`），顶层 message 与
  `data.stderr` 都带 limit + actual，`data.command` 两侧都是 CEP 方法名 —— 客户端可仅凭
  错误码把「写入过大」与 stale-hash / IO 失败区分开。patch 引擎侧新增
  `PatchErrorCode::SizeLimit`，不再复用 `IoError`。刻意不做成配置轴。Rewind 恢复与
  patch 回滚两处写入显式豁免（重装工作区已持有的字节，不引入新内容），并在
  `lancet-native/AGENTS.md` 记录豁免及理由。
- **#121-1**：删除 `allowed_roots`（profile schema、lattice-api 翻译、
  `ObservationPolicyConfig`、`resolve_observation_target` 及其类型/辅助函数/测试、文档）。
  观测范围的受支持面收敛为 `[lancet.project_root].root` + `[lancet.workspace]` 可见性
  规则。`lattice-profile/src/load.rs` 新增退役键预扫描，仍带该键的 profile 以具名错误
  失败并指出替代项，而非 serde 的 `unknown field`，也绝不按「收窄已生效」加载。
- **#121-2**：`ObservationPolicyConfig` 成为唯一限额真相源，`BudgetLimits` 缩到实际有
  读取点的两个字段并改为派生投影（`budget_limits()`）。`CepPolicyConfig` 与
  `PolicyConfig` 不再存储 `budget` 字段，改为 `budget()` 按需派生 —— 同步点从结构上消失。
  外部策略文件的 `[budget]` 成为 `[observation]` 的 type alias（连 schema 都无法漂移），
  两表经同一 `apply_external_limits` 写入同一限额集，`[observation]` 后应用故冲突时取胜。
- **#121-3**：删除 `MethodPolicy::{allowed_sources, required_capability,
  max_single_read_bytes}`、`CapabilitySourceClass`、`CapabilitySourcePolicy`、
  `PolicyConfig::capability_sources`、`ExternalCapabilitySourcePolicy`、
  `parse_source_classes`、`parse_required_capability` 及对应 TOML 键。`deny_unknown_fields`
  使残留键成为硬解析错误（有意的破坏性变更：这些键从未生效，静默接受才是误导）。同类清理
  顺带删除 `PolicyResolver` 四个无读取点字段（限额的第四份幽灵拷贝）。
- **#122**：`policy::network::host_allowed_by_list` + `normalize_host_pattern` 成为全仓
  唯一 host allow 匹配器，网络 URL 校验、`DefaultPolicyHook`、
  `AuthorizedEffectRegion::contains` 三处共用；`authorized_region_for` 在构造点把
  `region.hosts` 存为规范化形式。空 `allowed_hosts` 的分层语义**有意保留**（URL 校验回退
  `allow_public_https`，region 不授权任何 host，故 enforce 下出站前 fail-closed），该不对称
  由 harness 场景 `direct_tools_fetch_capability_denial_with_public_https` 锁定，并在
  函数文档、crate `AGENTS.md`、`docs/CONFIGURATION.md` 三处写明，禁止以放宽 region 的方式
  「收敛」。文档补齐前导点/大小写/空白语义与外部策略文件完整 schema。

**Verification:**
- `cargo fmt --all` — 已应用。
- `cargo clippy --workspace --all-targets -- -D warnings` — 通过，无告警。
- `cargo test --workspace` — **1495 passed / 0 failed**（exit 0）。
- 新增测试：`network.rs` 6 个（规范化幂等、前导点匹配 apex+子域、非规范形式、空/纯点
  fail-closed、与 URL 校验一致、子域放行）；`effect/region.rs` 4 个（含跨层一致性断言）；
  `policy/default.rs` 6 个（hook 与 region 一致、region 存规范化形式、外部 `[budget]`
  到达 per-op 读取点、外部 `[observation]` 到达 turn 投影、别名冲突优先级、退役键解析失败）；
  `observation.rs` 2 个；`lattice-profile/load.rs` 2 个（退役键具名错误、其余限额仍可加载）；
  `mutation.rs` 6 个（超限拒绝且无残留、边界值、跨路径错误码/消息/evidence 一致、
  `create_parents` 不建目录、按 UTF-8 字节而非字符计数、edit 侧超限可与 stale-hash 区分）。
- 变异测试（临时副本，worktree 未改）确认测试不是「回滚后仍通过」的空测试：把 region
  的 `contains` 改回精确相等 → `region_grants_apex_and_subdomains_for_leading_dot_pattern`
  与 `region_agrees_with_the_shared_matcher` FAILED；把 create 侧上限检查移到安装之后 →
  两个 create 测试 FAILED。
- 对抗性复核（独立 subagent 读 diff + 跑 cargo）指出并已修的缺口：#122 测试完全缺失、
  空列表语义未记录、`region.hosts` 规范化无测试锁定、#120 的 `create_parents` 顺序保证与
  字节计数未钉住、`lancet-native/AGENTS.md` 关于 Rewind 也执行上限的断言不实、以及
  Issue 要求的「SizeLimit 或等价错误码」原本只以自由文本存在且 `mutate.edit` 顶层消息与
  其他 patch 失败逐字相同（现以 `ToolResourceExhausted` 修正）。
- 第二轮独立验收（三个 agent 各自跑 cargo + 源码变异，共 15 次变异全部被测试检出，零
  blocker）另外发现并已修的三点：(1) 删除 `allowed_roots` 测试时顺带清掉了
  `[lancet.project_root].root` 逃逸拒绝的**唯一**自动化覆盖 —— 现补
  `cep_context_profile_rejects_project_root_outside_runtime_root`（同时覆盖
  `cep_policy_from_profile` 与 `load_policy_config` 两条装配路径）与
  `..` traversal 负向测试；(2) `[lancet.Observation]` 这类大小写变体会被
  `LancetSection::extensions` 吞掉，使退役键与同表内仍生效的限额一起静默丢失 —— 现由
  `check_lancet_sub_table_spelling` 以具名错误拒绝，同时保留未知子表落入 `extensions` 的
  前向兼容语义（两条测试各自锁定）；(3) `ResolvedNetwork` 的 `url`/`port`/`scheme`/
  `is_localhost`/`is_private_ip` 五个字段无任何读取点，属 #121 同类残留，已缩到唯一被读
  的 `host`，这些判定归 `validate_fetch_url` 单独拥有（它才解析 DNS）。
- 第三轮裁决 agent 指出上一轮修正自身引入了一处同类同步点：`TYPED_LANCET_SUB_TABLES`
  是 `LancetSection` typed 子表清单的第二份手工拷贝，删掉其中一项后测试全绿（即将来加
  typed 子表忘记同步时，该子表的大小写变体会重新回到静默丢弃）。现把该常量移到
  `schema.rs` 紧邻 `LancetSection`，并加 `typed_lancet_sub_tables_match_the_struct_fields`
  从序列化结果反推清单、断言两者一致（变异验证：删一项即失败）。

**Changed files:**
`crates/lancet-authority/{AGENTS.md,src/effect/region.rs,src/policy/{budget,default,mod,network,observation,resolved}.rs}`,
`crates/lancet-native/{AGENTS.md,src/edit/{limits.rs,apply.rs,mod.rs},src/handlers/{mutation,observation,external}.rs}`,
`crates/lancet-api/src/runtime.rs`, `crates/lattice-api/src/turn/cep.rs`,
`crates/lattice-profile/src/{load,schema}.rs`, `docs/CONFIGURATION.md`, `FIX.md`.

## GitHub #113：`tool-codegen` 支持 manifest `[render]` 节并生成 `render.json` 投影 — 2026-07-29

Severity: ENHANCEMENT（#116 / #118 的上游格式契约；不改变任何现有运行时行为）。

**Problem:**

`tool-codegen` 的 `Manifest` 是 `deny_unknown_fields`，任何 `[render]` 节都会 parse
error，因此 Tool Descriptor 的所有者无法声明自己结果的**呈现意图**。#29 是第一个
production Source Tool manifest，若不先立这个格式，五个工具的 manifest 都要在迁移
（并被 drift gate 冻结）之后回头补写。

**Fix summary:**

1. `[render]` 是 `Manifest` 上的可选节，根与每层子表都保持 `deny_unknown_fields`。
   注解建模为 internally tagged enum（`#[serde(tag = "role", deny_unknown_fields)]`），
   每个 variant 带自己的必填字段，所以拼错 role 是 `unknown variant`、写错role
   的键是 `unknown field`、少写 `diff.after` 是 `missing field`。用 `role: String`
   加一堆 `Option` 会让这三种情况全部 fail open —— 每个键都是那一个扁平 struct 的
   已知字段，`deny_unknown_fields` 看不见（这正是本仓库 `[[capabilties]]` 回归的形状，
   只是深一层）。空 `[render]` 与无注解的 arm 同样按 fail-closed 拒绝。
2. 字段路径是 `FieldPath` 类型，`Deserialize` 时按语法解析
   （`("params"|"result") ("." name ("[]")?)*`），所以畸形字符串在 parse 期就报
   语法错误，而不是稍后表现为费解的“悬空路径”。前缀是位置性的：`paramsx.y` 被拒，
   `params.params.x` 合法。
3. 解析器针对 `schemars` 的**真实**输出形状写，而非理想化 fixture：`Option<primitive>`
   是 `type: [T, "null"]`（`truncated` 这个 role 命名所指的字段本身就是它）；
   `Option<NamedType>` 是 `anyOf: [{$ref}, {null}]`；doc comment 让 `description`
   成为 `$ref` 的兄弟键；内部 tag 的 union 把分支载荷放在 `properties: {tag}` 的
   **兄弟** `$ref` 上（不合并兄弟属性，`observe.workspace` 的 params 字段一个都取不到）；
   `EditOperation` 的 union 藏在数组 `items` 里，所以 arm 支持
   `at = "params.edits[]"`。`$ref` 只认同文档 `#/$defs/<name>`，其余（外部 ref、
   深指针、未定义名、tuple-form `items`、无 `type` 的节点、递归 ref、`$ref` 旁的未知
   关键字）一律 fail closed，递归有 32 层深度上限。
4. arm 的判别属性由 schema **推导**（在每个分支都带 `const` 的那唯一一个属性），
   而不是由作者手写 tag 路径——后者是第二个事实源，可以与真实 serde tag 悄悄不一致。
   无判别、判别歧义、未知 case、重复 arm 均报错并列出合法 case。
5. `render.json` 是投影产物，与 `llm-projection.json` 同级，**不进** `ToolDescriptor`、
   `descriptor.json`、`descriptor-sha256`、`expected-identity.json`，也不进 component
   bytes。`render` 全程不经过 `lower_manifest`，`render_outputs` 只读已构造好的
   descriptor。
6. `--check` 门禁把 `render.json` 建模为**条件产物**：`expected_files` 用
   `Option<&str>`，`None` 意味着“该文件必须不存在”。原来的 check 循环只比对它期望
   **找到**的文件，所以删掉 `[render]` 后遗留的 `render.json` 对 `tool-codegen` 和
   `cargo xtask ci` 都是干净的，却继续把 manifest 已不再声明的注解喂给消费者。现在
   写路径删除该孤儿文件，`--check` 在它存在时失败。

**对抗性验收期间修掉的四处（findings 由独立 verifier 复跑确认）:**

7. **单槽 role 按 schema 根分侧计数**（原为按 scope 计数）。#116 的错误卡只用
   `params.*`、跳过全部 `result.*`，所以“这次调用的输入摘要”与“输出摘要”是两个槽；
   合成一个会让扁平 schema 的工具（`fetch.web`、`mutate.create`）在“标注请求”与
   “标注响应”之间二选一。同时补上跨 arm 的槽位竞争检查：同一 union 的两个分支互斥，
   各自填槽合法；**不同** union 上的两个 arm 可以在一次调用里同时命中，因此竞争同一
   个槽，报错。
8. **`status` 不再强制封闭值集**。`status` 声明的是“这个字段是本次调用的生命周期
   状态”，不是“这个字段是 enum”。`fetch.web` 的 HTTP 状态码是 `type: integer`，是该
   调用最自然的状态信号，强制 `enum` 会让它完全没有拼法。封闭值集仍是常见情形（
   `TodoStatus`、serde tag），也是消费者可穷举分派的依据；非标量仍拒绝——状态是一个
   值，不是一个结构。不把值集复制进 `render.json`：消费者手上已有 `result_schema`，
   复制会造出第二份可漂移的副本。
9. **AC3 测试补上磁盘层证明**。原测试只比对内存里的 `Outputs` 字段，看不见**文件名
   级**冲突：若 render 投影被路由到某个 identity 产物的文件名，原有断言全部依旧成立，
   而磁盘上的 descriptor 已被覆盖。现在测试断言产物文件名两两不同，并走真实
   `write_or_check` 在临时目录里比对加 `[render]` 前后**磁盘字节**，要求唯一新增文件
   就是 `render.json`。该测试当即抓到 auditor 注入的同名冲突探针。
10. `--check` 只把 ENOENT 报成“文件缺失”；权限或编码错误改为 `cannot read`，
    否则会把作者引向重跑 codegen——那什么也改变不了。
11. **`$ref` 解引用的兄弟键规则修正**（两条 verifier CONFIRMED 的 finding，均已实测
    `schemars` 1.2.1 真实输出）。原规则是“`$ref` 必须是节点的唯一键”，但：
    (a) doc comment 会让 schemars 把 `description` 作为 `$ref` 的**兄弟**发出
    （`{"description": "...", "$ref": "#/$defs/Sha"}`），于是**每一个带文档注释的字段**
    都会被判为悬空——`mutate.edit` 的 `expected_hash` 正是这个形状；
    (b) `Option<NamedType>` 是 `anyOf: [{$ref: T}, {type: null}]`，与
    `Option<primitive>` 的 `type: [T, "null"]` 不同，原代码把它当可空丢弃却不解引用。
    现在按“兄弟键是否**约束实例**”判断：注解类关键字（`description`/`title`/`format`/
    `default`/`examples`/…）旁的 `$ref` 照常解引用；`properties`/`type`/`required`
    等约束类兄弟不折叠（内部 tag 分支就靠这个保持两半都可见）；**未知**关键字按约束
    类处理，即本解析器未建模的构造 fail closed 而非被静默忽略。可空的具名类型解包到
    目标类型；`Option<Vec<T>>` 的 `type: ["array","null"] + items` 仍可逐元素走。

**#116 可用性实测**：五个 production Tool 的 `[render]` 节全部实跑通过（用
`schemars` 的真实 schema 形状 + 真实 generator）：`observe.workspace` 四模式
（read_file 的 `code`/`summary`、grep 的 `columns`+`truncated`、list_directory 的
`columns`、find_files 的 `tree`）、`mutate.create`（`summary`/`code`/`created_dirs`
作 `tree`）、`mutate.edit`（`params.edits[]` 上按 `replace_text` 分臂的 `diff`，以及
`insert_before` 分臂的 `code`）、`fetch.web`（`status`+`summary`+`code`+`truncated`）、
`session.todo_write`（`columns` + `$ref` enum 上的 `status`）。

**Verification:**

- `cargo run -p xtask -- tool-codegen` 后 `tools/noop/generated/` 六个既有产物 sha256
  与加 `[render]` 之前逐字节相同（`diff` 空）：`descriptor.json`
  `5d9a0967…`、`expected-identity.json` `0acbd1a4…`、`llm-projection.json`
  `0f0f2f63…`、`registration.rs` `bf661f43…`、`tool-host.wit` `97928b29…`、
  `noop_echo.component.wasm` `b677a9b3…`。
- `cargo run -p xtask -- tool-build --verify` — Component 摘要仍复现
  `b677a9b339c4703d8a02af54def031509ff17d73c66a83fc59176a528d646f08`，即 AC3 的
  artifact 一半是实测而非论证。
- `--check` 门禁五种情形实测：committed tree 通过；两次生成字节相同；被改写的
  `render.json` 报 drift；缺失报 missing；删掉 `[render]` 后遗留文件报 stale，写路径
  删除它后 `--check` 复归干净。
- `cargo test -p xtask` — 106 passed（新增 5 个 wiring 测试 + 37 个格式测试）。
- `cargo fmt --all --check`、`cargo clippy -p xtask --all-targets -- -D warnings` — 通过。
- `cargo xtask ci`（含 `tool-codegen --check`）— 通过。
- 对抗性验收：四个独立 auditor 分别攻 strictness/路径、确定性/门禁、identity chain、
  scope 围栏与 #116 可用性，每条 finding 由独立 verifier 复跑确认或否证。

**Changed files:**
`xtask/src/tool_codegen.rs`；`xtask/src/tool_codegen/{render.rs,render_tests.rs}`（新增）；
`tools/noop/manifest.toml`；`tools/noop/generated/render.json`（新增）；`AGENTS.md`；`FIX.md`。

## GitHub #112 + #29：versioned Cell evidence 表落地，`session.todo_write` 迁为首个内嵌 Component Tool — 2026-07-29

Severity: N/A（feature：ADR 0004 决策 1/2/3/7 + ADR 0005 决策 2/3 的实现切片）。

**Problem:**

两票串行阻塞。#112：Cell evidence（`AdmissionEvidence` / `ObservedEffectsRecord` / `DeniedOperationsRecord`，#28 交付）只存在于 `cep_audit_entries.metadata_json` 这个不透明 blob 里，而该 store 会被 ADR 0004 决策 8 删除；evidence 既不可按 turn/call 检索，也没有 ADR 0005 决策 6 所需的稳定标识。#29：`session.todo_write` 仍是 native handler，`EXPECTED_COMPONENTS` 为空，`from_embedded()` 组合出空目录 —— 生产二进制里一个 Component Tool 都没有，且 `tools/noop/generated/registration.rs` **从未被编译进任何 crate**（noop 只经手写的 `noop_fixture.rs` 进入测试），所以此前不存在"把 Tool 链接进宿主"的机制。

**Fix:**

*#112（`lancet-audit`）*：新增 `src/evidence.rs` —— 三张 versioned 表 `evidence_calls` / `evidence_effects` / `evidence_denials`，`PRAGMA user_version` 从 1 升到 2（v1 库加性升级并保留历史；已声明 v2 但表形状不符的库报可操作错误且不改动）。`evidence_id`（store 生成的自增整数）是稳定标识，**刻意不是 `audit_id`** —— 后者随 CEP store 一并消失。Cell 按列拆存（`ctx_op`/`ext_impact`/`cell_scope`/`scope`/`scope_trust`/`scope_channel`/`target_kind`/`target_value`）而非 JSON blob，evidence 面才真正可查；`cell_scope` 与 `scope` 分列，因为 `Cell::scope` 与 `ObservedScope::scope` 是两个协议字段。读取时校验每条记录的 `version`，不支持的版本报 `UnsupportedRecordVersion` 而非按本二进制形状误解析；损坏的枚举标签报 `UnreadableField` 而非静默回落默认值。拒绝与失败同样落表（`EvidenceOutcome::{Completed,AdmissionDenied,Failed,Cancelled}`）：admission 失败携带其 `AdmissionEvidence`，per-operation 拒绝落 `DeniedOperation`，result-cell 违规保留其之前已记录的 operation effects。`metadata_json` 不再承载结构化 evidence，只留 `evidence_id` 指针与派生的 `component_cells` 查询键；PROV 投影与 attestation 构建改经 `EvidenceLookup` 从表中解析（`project_rows_with_evidence` / `build_in_toto_statement_with_evidence` / `rebuild_in_toto_statement_with_evidence`），operator replay 与 attestation 写入路径接同一个 store。

*#29（Component 迁移）*：`tools/todo-write/` 新增 source manifest 独占 method/result contract、Cell/capability/scope、description、version。**全部校验下沉到 manifest 的 params schema**：`pattern: "[^\\p{White_Space}]"` 覆盖空白 content，`not/contains/minContains: 2` 覆盖"至多一个 in_progress"，`maxItems: 100` 是编译期上限 —— host 的 `params_validator` 在进入 Component **之前**以单一结构化 `SchemaMismatch` 拒绝，guest 里不留第二份命令式规则副本。descriptor 只声明 `session.result = alloc:none:session` 这一条真实发生的 effect（返回值进 context，host 度量），**不声明任何 `mutate` capability** —— 该 method 从来没有 session store，本票也没有新增。

manifest 新增必填 `[tool] distribution = "builtin" | "fixture"` 字段（无默认值：Tool 不得因遗漏而进入封闭目录），`cargo xtask tool-codegen` 据此**生成** `crates/lancet-runtime/src/generated/host_composition.rs`：built-in Tool 的 `inventory` 注册（`include_bytes!` committed descriptor + Component）与 host 侧 pin 的 `EXPECTED_COMPONENTS` 身份表。这既是把 Tool 链接进宿主的机制，也顺手消掉 #103 记录的"`generated/` 实为手写、digest 无同步校验"缺口。`parse_hex_digest` 改为 `const fn` 并脱离 `test-utils` gate，pin 的 digest 成为构建期常量。

native handler 与注册删除（`handlers/session_state.rs`、`register_native_session_state`）；`lancet-catalog` 的 `TodoWrite` 类型及 `methods/session.rs` 整体删除，`CATALOG` 从 5 降到 4（shipped catalog 仍是五个 Tool：四个 native + 一个 Component）。约 20 个此前把 `TodoWrite` 当通用 typed-method fixture 的测试改用新增的 `lancet_catalog::test_method::TestEcho`（`test-method` feature，dev-only，刻意不在 `catalog()` 中）；真正走生产路径的测试改用真实 shipped Tool。authority 层随之删除不可达的 `session.todo_write` 分支、`SessionStateOperation`、`ResolvedSessionState` 与 todo budget 检查。

**Behavior change (honest):** profile 可配的 per-turn `[lancet.observation] max_todo_ops` 预算**不再生效**。原先由 native handler 调 `budget.try_charge_todo_ops` 扣减，而 Component 的 result accounting 按 #27/#28 设计只度量不扣费（charge/credit seam 明确推迟）。单次调用的上限保留为 descriptor 的 `maxItems: 100`（等于原 `max_todo_ops` 默认值），跨调用累计上限消失。残留记录在 `FINDINGS.md` L-GH29-01；evidence 表的三处读取健壮性残留记录在 L-GH112-01。

**对抗性验收查出并修掉的（同一票内）：**

1. **`\S` 与 `trim()` 不等价** —— 真实的行为收紧丢失。Rust 的 `char::is_whitespace`（`trim` 的依据）走 Unicode `White_Space` 属性，正则 `\s` 类更窄。逐字符枚举全部 `is_whitespace()` 码点对照编译后的 schema：`\S` **接受 16 个 native handler 会拒绝的纯空白 content**（U+0085、U+1680、U+2000..U+200A、U+2028、U+202F、U+205F、U+3000）。改为 `[^\p{White_Space}]`（分歧为 0），并加 `blank_content_rejection_matches_trim_over_all_unicode_whitespace` 逐字符钉住。
2. **result schema 比 params schema 宽松**。host 用 result schema 校验 guest **返回值**，因此只出现在 `[params]` 的 `maxItems` / `pattern` / `minContains` 是行为不端的 guest 在返回路径上可违反的规则。两份文档改为完全一致，加 `result_schema_is_identical_to_params_schema` 钉住。
3. **#28 设计 §11 的 name-collision hazard 此前无执行点**。该 hazard 在 `EXPECTED_COMPONENTS` 为空时不可达，本票让它可达却没加检查。实测构造：一个名为 `observe.workspace` 的 builtin Tool 能通过 composition，其调用被 legacy pre-dispatch effect-kind deny 以 `EffectViolation`（`declared_effect: [WorkspaceObservation]`，native descriptor 的，不是 Component 的）拒绝 —— 两套授权词表同时武装。新增 `ServerRunConfig::validate_no_component_native_collision`，在两套词表唯一共存的 seam（`run_native` 与 `LancetServer::spawn_composed`）fail closed；谓词拆为纯函数 `component_native_collisions` 并加三项测试，因为真实冲突无法经任何公开 API 构造。
4. **evidence 版本校验有三处漏口**：`evidence_effects` / `evidence_denials` 的 per-entry `record_version` 列每次写入却从不读回（未来的 v2 effect 形状会被按 v1 解析）；`evidence_calls` 的 `effects_version` / `denials_version` 为 NULL 时被静默当作当前版本。前者改为逐条校验并与记录头比对，后者报新增的 `MissingRecordVersion`（带 operator action）。各加一项测试。
5. **生产读取路径吞掉 `EvidenceError`**。`operator.rs` 与 `attestation.rs` 用 `.ok().flatten()`，使「读取被拒」与「该调用没有 evidence」不可区分 —— 而 `version_from_rows` 正是按 lookup 是否解析来选 attestation 版本，于是 attestation 形状变成 store 健康度的函数。改为记录后降级（投影层无错误通道，但失败不再隐形）。
6. **必需列清单漏两列 + legacy import 携带失效指针**。`REQUIRED_EVIDENCE_CALL_COLUMNS` 只列了 9/11 列，漏掉两个 version 列 —— 一个缺这两列的 v2 库能通过 open，之后**每次** evidence 写入在运行时失败（history 被丢弃却无 open 期错误）。另外 legacy read-only import 只搬 audit 行不搬 evidence 表，而被导入行的 `metadata_json` 仍带旧库的 `evidence_id`；新库自增 id 从 1 重开，历史行因此可解析到一条**无关的**新 evidence 行（实测确认）。import 现清除该指针 —— 该指针在新库中无意义，"无 evidence"才是事实。各加一项测试。
7. **死代码与过时注释**：删掉 `compact_success_metadata` 里已不可达的 `session.todo_write` 分支（Component 调用总带 durable evidence metadata，该 fallback 永不触发），并说明 `compact_success_summary` 的同名分支**是**活的；修正 `transport.rs` / `lancet-api/src/runtime.rs` / `lancet-runtime/src/generated/mod.rs` / `lancet-api/AGENTS.md` 四处仍写「production 目录为空，直到 #29–#32」的注释 —— #29 就是本票。

**Verification:**

```
cargo test -p lancet-audit           # 86 passed（含 evidence_tests 14 项新契约测试）
cargo test -p lancet-runtime         # 67 + 10 + 15 + 11 + 11 passed（tests/embedded.rs 为新增）
cargo test -p lancet-native          # 147 + 17 + 1 + 4 + 4 passed
cargo test -p lancet-catalog         # 27 passed
cargo test -p lancet-dispatch        # 10 passed
cargo test -p lancet-authority       # 125 + 6 + 2 + 10 passed
cargo test -p lancet-api             # 9 passed
cargo test -p lattice-api            # 93 + 各集成套件 passed
cargo test -p lattice-profile        # 96 passed
cargo test -p xtask                  # 55 passed
cargo xtask tool-codegen --check     # 无漂移（幂等）
cargo xtask tool-build --verify      # session-todo_write Component 逐字节复现 committed digest
LATTICE_BIN=... cargo test -p lattice-system-harness   # 19 套件全绿
```

产品级证据（criterion 9）：`direct_tools_todo_write_success` 断言默认二进制发出的 `cep_call_started` 携带 `semantic_stage = "cell_component"` / `guard_stage = "cell_admission"` —— native handler 永不发出这组标签；并断言该行**不**claim native 的 `session_state_mutation` impact kind。新增 `direct_tools_todo_write_result_survives_session_reopen` 证明 Component 结果经持久 tool-role 消息进入重开后的 transcript。`migrated_todo_write_has_no_native_fallback` 证明未内嵌 Component 的运行时把该 method 报为 unrouted 而非静默由 native 服务。

**Changed files:** `crates/lancet-audit/src/{evidence.rs,evidence_tests.rs,audit.rs,audit_tests.rs,lib.rs,operator.rs,store_path.rs,provenance/{projector.rs,attestation.rs,mod.rs}}`、`crates/lancet-audit/Cargo.toml`、`crates/lancet-runtime/src/{composition.rs,generated/host_composition.rs}`、`crates/lancet-runtime/tests/embedded.rs`、`crates/lancet-native/src/{lib.rs,server/{transport.rs,native_registry.rs},handlers/mod.rs,effect_contract_tests/**}`（删 `handlers/session_state.rs`、`effect_contract_tests/runtime_fixtures/session.rs`）、`crates/lancet-native/tests/{cep_component_round_trip.rs,cep_warn_harness.rs,prov_isomorphism.rs}`、`crates/lancet-catalog/src/{catalog.rs,lib.rs,test_method.rs,methods/mod.rs}`（删 `methods/session.rs`）、`crates/lancet-catalog/Cargo.toml`、`crates/lancet-dispatch/src/{registry.rs,transport.rs}`、`crates/lancet-dispatch/Cargo.toml`、`crates/lancet-cep/src/tool.rs`、`crates/lancet-authority/src/policy/{capability.rs,default.rs,resolved.rs}`、`crates/lancet-authority/tests/{guard_decide_fuzz.rs,policy_coverage.rs,policy_tests.rs}`、`crates/lancet-api/src/attestation.rs`、`crates/lattice-api/src/turn/{cep.rs,cep_tests.rs}`、`crates/lattice-api/tests/{audit_turn_provenance.rs,cep_mode_integration.rs,cep_round_trip.rs,direct_local_acceptance.rs,dogfood_impact_matrix.rs}`、`crates/lattice-api/Cargo.toml`、`crates/lattice-profile/src/defaults.rs`、`crates/lattice-system-harness/tests/direct_tools.rs`、`tools/todo-write/**`、`tools/noop/manifest.toml`、`xtask/src/tool_codegen.rs`、`Cargo.toml`。

## GitHub #71：effect-kind deny 的 prevent 语义补齐验收覆盖与文档 — 2026-07-28

Severity: HIGH（安全语义，issue 原始描述）→ 实际为 MEDIUM（写前闸门已存在，缺的是覆盖与文档）。

**Problem:**

Issue #71（迁移自 Codeberg #49）报告 `[lancet.effects] deny` 只是 post-hoc 检查：mutation 先落盘，transport 事后才比对 observed effects 并返回 `EffectViolation`，字节不回滚。

复核后：写前闸门已由 Codeberg #24 slice 3（commit 35b4b70d）落地——`check_declared_effect_kinds_denied` 在 `registry.dispatch` 之前比对 method 静态 `EffectDecl.kinds` 与 `denied_effect_kinds`，命中即拒绝，handler 不执行。issue 描述的核心缺陷已不成立。剩余的真实缺口是：

1. `mutate.edit`、`observe.workspace` 的写前拒绝没有测试（只覆盖了 `mutate.create`）；无反向对照测试，"文件不存在" 断言可能静默变空。
2. 写前拒绝不经任何 handler，因此没有任何代码为该行写 rejection evidence——`AuditQueryFilters::rejection_stage` 与 `lattice audit --capability` 都看不到"写被阻止"的证据行。
3. `effect_deny_rejects_in_warn_mode` / `effect_deny_rejects_in_off_mode` 对 post-hoc 层的覆盖是**偶然**的：`session.todo_write` 声明 `SessionStateMutation` 而测试 deny 的是 `WorkspaceMutation`，写前闸门因此不触发。两个测试断言的是两层共有的子串 `denied by effect-kind policy`，无法区分层级；若有人把 deny 改成 `SessionStateMutation`（该 method 实际声明的 kind），写前闸门开始拦截、测试照样通过，post-hoc 层将静默失去全部覆盖。
4. 文档仍描述旧模型：`docs/LANCET.md` 写 "enforced at checker time, after runtime effects are observed"、"keyed on observed impact kind"；`CONFIGURATION.md` 与 `schema.rs` 只写 "Runtime effect kinds to deny"，读者无法推断 deny 是否阻止写入、post-hoc 命中是否回滚。

**Fix:**

- 写前拒绝补 rejection evidence：`transport.rs` 在拒绝分支写入 `rejection_stage=policy_gate`、`denial_source=effect_kind_deny`、`required_capability`（`required_capability_label`，与 `lattice audit --capability` 同词汇）、`policy_enforced=true`，并把 `durable_metadata.take_json()` 传给 `finalize_audit_for_dispatch`。
- `lancet-native` 新增三个 transport-seam 测试：`denied_effect_kind_blocks_mutate_edit_before_backend`（用**正确**的 `content_hash` 作 `expected_hash`——bogus hash 会让 handler 在 Mutation Precondition 处先失败，从而让"字节不变"断言变空；已实测：移除写前闸门后该文件被改写为 `rewritten content\n`，测试失败）、`denied_effect_kind_blocks_observe_workspace_before_backend`（断言 audit metadata 含 rejection evidence，且**不含** handler 的观测证据键 `observation_source`/`bytes_read`/`content_hash`/`visibility_evidence`——断言错误文本不含文件内容是恒真的，`CepError` 消息是固定模板）、`unrelated_denied_effect_kind_does_not_block_mutate_create`（反向对照，证明同一 wiring 下写入本会成功）。deny 服务器构造抽为 `config_with_denied_effect_kind`，断言抽为 `assert_pre_dispatch_denial`（钉住 `declared effect denied`，不再接受两层共有子串）。
- 新增 `cep_native_registry_registers_nothing_outside_the_catalog`：两层 effect-kind 检查都以 `lancet_catalog::catalog()` 为键，catalog 之外的 native 方法会同时豁免两层，故生产 registry 不得注册这类方法。为让这条成为**真**子集检查（而非硬编码坏名单），`lancet-dispatch` 的 `MethodRegistry` 新增 `registered_methods()` 枚举器。
- `cep_mode_integration.rs` 的 warn/off 两个测试改用 `assert_post_dispatch_denial`（钉住 `observed effect denied`），并在注释中写明 decl/deny 错配是这层覆盖的承重条件、不要"整理"成 `SessionStateMutation`。
- `lattice-api` 补配置路径覆盖 `turn::cep_tests::effects_deny_profile_path`（3 例）：TOML 里的 snake_case 名字（`workspace_mutation` / `workspace_observation`）经 `load_policy_config` 落到 `denied_effect_kinds`；无 `[lancet.effects]` 时不 deny 任何 kind；拼错的 kind 名（`workspace_mutuation`）解析失败而非静默变成空 deny 集。此前配置链只有 `mutate.create` 经 `LATTICE_BIN` harness 覆盖。
- `lancet-authority` 补两个 checker 单元测试：`declared_observation_kind_denied_before_any_observation`（验收标准 2 的单元级正向用例 + 空 deny 集不生效）、`observed_effect_denial_is_distinguishable_from_declared_denial`（直接调用 `check_denied_effect_kinds`，并断言两层消息互不包含）。
- 文档改写：`docs/LANCET.md` Mode semantics、`docs/CONFIGURATION.md` `[lancet.effects]`（含 §"Denial semantics: prevent first, then detect"）、`crates/lattice-profile/src/schema.rs` `LancetEffectsSection` doc comment、`crates/lancet-authority/src/effect/mod.rs` 的 Enforcement Model 列表，统一说明两层语义——写前按声明 kind 阻止（无文件变更）、写后按 observed effects 检测且按 ADR 0003 decision 8 不回滚；两层都不受 `mode` 调节；两层都以 native catalog 为键，Component Tool 由 `[lancet.capabilities]` admission 管辖；deny 是按 kind 而非按 path，路径级写禁用 `[lancet.workspace] denied_write_patterns`。

方案 B（补偿回滚）未采纳：与 ADR 0003 decision 8 冲突，需要 ADR 修订。

**Verification:**

- `cargo test -p lancet-native` — 154 passed, 0 failed（含 4 个 deny 测试与 registry 不变量）。
- `cargo test -p lancet-authority` — 128 + 6 + 2 + 11 passed, 0 failed。
- `cargo test -p lattice-api` — 全 target passed, 0 failed（`cep_mode_integration` 8 passed，`workspace_history_acceptance` 13 passed）。
- `cargo test -p lattice-profile` — 96 passed, 0 failed。
- `cargo clippy -p lancet-native -p lancet-authority -p lattice-api -p lattice-profile --all-targets -- -D warnings` — 无告警。
- `cargo fmt --all` — 已应用。

**Changed files:**
`crates/lancet-native/src/server/transport.rs`, `crates/lancet-native/src/server/native_registry.rs`, `crates/lancet-dispatch/src/registry.rs`, `crates/lancet-authority/src/effect/checker.rs`, `crates/lancet-authority/src/effect/mod.rs`, `crates/lattice-api/src/turn/cep_tests.rs`, `crates/lattice-api/tests/cep_mode_integration.rs`, `crates/lattice-api/tests/workspace_history_acceptance.rs`, `crates/lattice-profile/src/schema.rs`, `docs/CONFIGURATION.md`, `docs/LANCET.md`, `FIX.md`.

## GitHub #124：rollout 发布门禁的输出捕获用两个独立 fd 写同一文件，互相覆盖 — 2026-07-27

Severity: MEDIUM（发布门禁判定不可复现 + 证据文件被破坏）。

**Problem:**

`xtask/src/rollout.rs` 的 `run_timed` 是 alpha 发布门禁全部步骤（`Action::Cmd` / `CargoTest` / `Doctor`）的执行入口，由 `.github/workflows/full-gate.yml` 的 `cargo xtask rollout --channel alpha` 每晚实跑。它创建**一个** `NamedTempFile`，然后**两次 `reopen()`** 分别接 stdout 与 stderr：

```rust
cmd.stdout(Stdio::from(file.reopen()?))
   .stderr(Stdio::from(file.reopen()?))
```

tempfile 3.27.0 的 `reopen()`（`src/file/imp/unix.rs:81`）是 `OpenOptions::new().read(true).write(true).open(path)`，**不带 `O_APPEND`**，因此两个 fd 各持从 0 开始的**独立写偏移**。子进程交错写 stdout/stderr 时，两条流在同一文件里互相覆盖而非拼接。实测复现：

```
stderr: "   Compiling lancet-protocol v0.1.0\n   Compiling lattice-api v0.1.0\n    Finished test profile\n"
stdout: "running 12 tests\ntest result: ok. 12 passed; 0 failed\n"
stderr: "     Running tests/direct_local_turn.rs\n"
→ 文件实际内容:
b'running 12 tests\ntest result: ok. 12 passed; 0 failed\nce-api v0.1.0\n    Finished test profile\n     Running tests/direct_local_turn.rs\n'
```

这不是美观问题，被破坏的文本是三处 gating 逻辑的唯一输入：
1. `cargo_test_passed`（`:710`）用 `passed_test_count`（`:720`）从中正则提取通过数，为 0 即判门失败（anti-vacuous 守卫）；
2. `status_check`（`:696`）用 `tail()` 生成写入 record 的失败详情（`:244`）；
3. `Action::Doctor`（`:327-329`）把同一份输出**直接 `fs::write` 成 `target/rollout/evidence/doctor.json`** —— 而 doctor 的 JSON 走 stdout（`lattice-cli/src/commands/lancet.rs:131-135`）、错误走 stderr（`main.rs:94`），两者交错覆盖后，`docs/TESTING.md:423` 声称的发布证据可为**非法 JSON**。

第二个缺陷放大了第一个：`:773` 原为 `fs::read_to_string(..).unwrap_or_default()`。交错覆盖可切断 UTF-8 多字节序列产生非法 UTF-8（实测 `b'xxx\x9c\x93yy\n'` 解码失败），而 `unwrap_or_default()` 把解码失败**静默吞成空串**；此时 `success` 仍为 true，`passed_test_count` 返回 0，`cargo_test_passed` 精确报出 "exited 0 but ran 0 tests" —— 测试全绿的合格制品被 5 个 `CargoTest` 步骤中任一拒绝，且 `detail` 毫无诊断价值。含非 ASCII 的输出在本仓库属常态（`crates/lattice-api/tests/dogfood_impact_matrix.rs` 等测试源码含中文，rustc 诊断亦用 Unicode）。因写序取决于子进程调度，**同一 commit 反复跑结果可不一致**。

第三个独立缺陷（同一失败路径）：`tail()`（`:731`）用**字节索引**切 `&str`（`&trimmed[trimmed.len() - 800..]`），截断处落在多字节码点内部时 panic。已实测：`start byte index 66 is not a char boundary; it is inside '中'`。后果比普通失败更糟——panic 沿 `main` 冒泡为 exit 101，该步 `status` 停留在 seed 的 `Blocked`、`detail` 为 `None`、`failure_artifacts` 不写入 JSON，即恰好丢失该函数存在的意义。

对照：`xtask/src/ci.rs:327` 用 `Stdio::inherit()`，无此缺陷；三条都是 rollout 独有。

**Fix:**

1. 改为 `file.reopen()` 一次拿 stdout fd，再 `try_clone()` 出 stderr fd —— `try_clone` 复制描述符并**共享文件偏移**，两条流按序落盘。
2. `:773` 改为 `String::from_utf8_lossy(&fs::read(..).unwrap_or_default())`，保留到达的字节而不是整份丢弃。
3. `tail()` 改为从 `len-800` 向前走到下一个 char boundary 再切，永不 panic 且不超过上限。

三处均加注释写明为什么不能回退成原写法。

**Verification:**
- `cargo test -p xtask` — 51 passed / 0 failed（新增 3 条）。
- 新增 `run_timed_interleaves_stdout_and_stderr_without_overwriting`：spawn `sh -c 'echo compiling >&2; echo "test result: ok. 7 passed..."; echo running >&2'`，断言 stderr 两行与 stdout 行同时存活、且 `passed_test_count` 正确得 7。
- 新增 `tail_does_not_panic_when_the_cut_lands_mid_codepoint`（fixture 先断言 `!is_char_boundary(len-800)` 以确保真的踩在码点内部）与 `tail_returns_short_output_verbatim_and_trimmed`。
- **反向验证（两条各一次）**：
  - 还原双 `reopen()` → 新测试 FAILED，实测输出被咬成 `"test resulrunning\n passed; 0 failed\n"`（比预估更糟：`test result:` 行被切断，`passed_test_count` 确实会误读）。
  - 还原字节索引 `tail()` → FAILED 并报 `start byte index 66 is not a char boundary; it is inside '中'`。
  - 两次还原后均确认 `git diff` 干净、重跑为绿。
- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` — passed。

**Changed files:**
`xtask/src/rollout.rs`; `FIX.md`.

## GitHub #111 部分收口：给两条断言强度不足的测试补上实质断言 — 2026-07-27

Severity: LOW（**经对抗性复核下调，原评 HIGH 有误——见下方「严重度更正」**）。

收口 **#111** 中 6 条的第 1、2 条（其余 4 条留 #111，见下方 Residual）。

**严重度更正（同日，落地后复核）:** 本条目初稿评为 HIGH，理由是「两条安全不变量的回归网失效」。**该判断是错的**，已实测推翻，故在此更正而非删改原文（append-only）。两个不变量都另有测试守着，且都在每个 PR 必跑的 `cargo test --workspace` 里：

- **deny-all 退化**：给 `decide()` 注入无条件 `Denied` 后，**修复前就存在的** `crates/lancet-authority/tests/policy_tests.rs` 立刻 5 条 FAILED（`allows_observation_without_denied_patterns`、`allows_mutation_with_no_denied_patterns`、`allows_mutation_not_in_deny_list`、`denies_network_host`、`denies_observation_budget_exhausted`）。全仓 `PolicyDecision::Allowed` 断言共 9 处，其中 `policy_tests.rs:50/63/74` 正是「blacklist 误改回 allowlist」的对偶断言，`lancet-native/tests/policy_integration.rs:63/97` 覆盖网络短路场景。fuzz 套件对该退化盲，但它不是唯一防线。
- **System Path Exclusion 被删**：`crates/lattice-system-harness/tests/direct_mutation.rs:475` 的 sys_deny 场景用**空 extra_profile** 尝试写 `.lattice/evil.txt`，其拒绝完全依赖 `default.rs:156-160` 压入的前缀；该文件 `#[ignore]` 计数为 0，`.github/workflows/ci.yml:44` 的必需 job 就是 `cargo test --workspace`。且它验证的正是 effect region——`mutation.rs:226` 传给 `validate_path_facts` 的第三参数就是 `ctx.authorized_region`。

因此本次改动的真实价值是**回归定位速度**（单元测试直接指出是 region 构造问题，而不必等一个 full-turn harness 场景失败后反推）与**测试诚实性**，不是补防护缺口。另外复核纠正了一处事实误读：`biased_allowed_inputs()` 并非「6 条应当放行的输入」——`exec.command_run` 的源码注释明写它预期 `NonDeterminable`，故原属性名 `..._stable` 与其文档串其实是自洽的，不存在初稿所称的「文档/代码矛盾」。

**Problem:**

1. **`guard_decide_fuzz` 的 allow 侧从不断言 `Allowed`。** `crates/lancet-authority/tests/guard_decide_fuzz.rs` 的文件头声明策略 1 是「biased_allowed — exercises positive policy branches」，`biased_allowed_inputs()` 精心构造了 6 条输入，但两个消费它的 proptest 属性只断言**同一输入两次调用结果相等**，全文件 `PolicyDecision::Allowed` 出现 **0 次**。而 `DefaultPolicyHook` 是纯函数（`decide` 直接委托 `decide_with_evidence`，结构体只持 `config` 与 `resolver`，无内部可变状态），所以「同一输入两次同结果」在任何实现下恒真——该断言**不可能失败**。deny 侧有实质断言 `matches!(.., Denied{..})`，allow 侧没有对偶。后果：策略若退化为「全部拒绝」（例如把 blacklist 语义误改回 allowlist，使空 `denied_write_patterns` 变成"什么都不许写"），256 个 case 全返回 Denied 但两两相等，整套 fuzz 零信号。

2. **宣称验证 System Path Exclusion 的测试只断言 vec 非空。** `authorized_region_for("mutate.create", ..)`（`policy/default.rs:156-160`）无条件把 `SYSTEM_PATH_EXCLUSIONS`（`.git` / `.lattice`）压入 `denied_write_path_prefixes`，注释写明这是 host-enforced 且不可 profile 配置（ADR 0003）。唯一覆盖它的测试 `workspace_policy_applies_denied_patterns_to_mutate_region` 传入 `denied_write_patterns: vec!["crates/**"]` 后只断言 `assert!(!...is_empty())` —— 而 `"crates/**"` 自己就会推入**同一个** vec，因此该断言由 profile pattern 独力满足，与两条系统排除毫无关系。全仓确认无任何测试断言 `.git`/`.lattice` 确实在该 vec 中。后果：删掉那个 exclusion 循环，测试仍绿，而任何未显式列出 `.git/**` 的 profile 其 mutate effect region 都不再拒绝 `.git/`、`.lattice/` 写入。

**Fix:**

1. 新增 proptest 属性 `guard_decide_fuzz_biased_allowed_reaches_expected_decision`：对该集合逐条断言实际决策。先用一次性探针确认了各输入的真实决策（5 条 `Allowed`，`exec.command_run` 为 `Denied{NonDeterminable}`——与其源码注释一致，它是已退役方法，有意留在正向集合里），故断言按方法分支：退役方法必须是 `NonDeterminable`，其余必须是 `Allowed`。保留原幂等属性不动（它验证的是纯函数性，仍有价值），只是不再让它独自承担分类正确性。
2. 把 `workspace_policy_applies_denied_patterns_to_mutate_region` 的断言改为按名检查 profile pattern 确实到位（`.any(|p| p.ends_with("crates"))`），并新增 `mutate_region_carries_system_path_exclusions_without_any_profile_pattern`：**故意配空 `denied_write_patterns`**，对 `mutate.create` 与 `mutate.edit` 各断言 `.git` 与 `.lattice` 逐条在 region 里。空 pattern 是关键——只有这样才能把系统排除与 profile 贡献彻底分离。

**Verification:**
- `cargo test -p lancet-authority` — 126 + 6 + 2 + 11 passed / 0 failed。
- **反向验证（本修复的核心证据，两条各做一次）**：
  - 删除 `default.rs:156-160` 的 `for exclusion in SYSTEM_PATH_EXCLUSIONS` 循环 → 新测试 FAILED 并报 `mutate.create: host-enforced exclusion ".git" missing from denied_write_path_prefixes: []`；**同时原有的 `!is_empty()` 断言仍然通过**，正是它当初漏掉这个缺陷的原因。
  - 让 `decide()` 对所有输入返回 `Denied{NonDeterminable}` → 新属性 FAILED 并报 `positive-path input observe.workspace must be Allowed, got Denied{...}`；**同时两个原有幂等属性仍然通过**。
  - 两次改动后均已确认 `git diff` 干净（源码逐字还原），并重跑全部测试为绿。
- `cargo fmt --all --check`、`cargo clippy -p lancet-authority --all-targets -- -D warnings` — passed。

**Residual:** #111 的其余 4 条留在该 issue 追踪，本次未动，因为它们需要的不是断言改写而是新增测试基础设施：**#111-3** `cep_warn_harness` 默认 CI 路径只读已提交 markdown（需把 `generate_warn_report()` 改为默认执行路径）；**#111-4** `fetch.web` 上限测试的 fixture（22 字节）小于上限（64 字节）且断言为析取（需换超限 fixture 并拆成合取）；**#111-5** `direct_secrets` 的 denied-content 哨兵场景 `tools.enabled = []` 致被读路径不存在（需启用 `observe.workspace` 并重写脚本）；**#111-6** Component deadline→`ToolTimeout` 无可失败测试（需要一个会自旋的 fixture guest，与 M-TOOL-02 相邻）。
这 4 条的严重度**同样经复核下调为 LOW**，理由与上面两条同构——每条声称的「缺陷会静默通过」都被另一处测试切断：#111-3 被 `crates/lattice-api/tests/cep_mode_integration.rs:144-169` 对 `effect_warning` 的硬断言切断（且 `server/transport.rs:311-410` 的 effect 检查是方法无关的，无 per-method Warn 分支）；#111-4 被 `crates/lancet-native/src/handlers/external.rs:384-420` 的 `native_web_fetch_truncates_over_cap_response` 切断（500 字节 body 配 100 字节上限，断言 `body.len()==100` 且 `truncated==Some(true)`）——初稿的错误很具体：`rg max_web_response_bytes` 时把范围限定在 harness，漏掉产品 crate 内的真覆盖；#111-5 被 `crates/lattice-system-harness/tests/direct_observation.rs` 的完整读拒绝矩阵切断（`denied_read_patterns` 短路会让 `expected_ok`、无 JSON result、8 条 `GuardDenied` 三处同时变红），且该 world 的 extra_profile 为空、`private.txt` 已 tracked，按 `policy/visibility.rs:78-90` 它其实是 **Visible**，连「denied」都不是；#111-6 的危害论证不可达——当前封闭 world 无任何可阻塞的 host call（只 `import host-control`，`cancelled` 读 flag、`emit_event` 用 `try_send`，无 WASI/IO/sleep），故长跑 guest 必然消耗 wasm 指令并被 `set_fuel` 有界终止，deadline 是 fuel 之上的纵深；且 `EXPECTED_COMPONENTS` 为空使生产路径不可达，属有意的迁移中间态，已关闭的 #69 亦已在位记录该测试形态。复核另指出两个比原发现更有价值的方向：`direct_secrets` 相对 slice6 plan 真正缺的是 `audit query --tail`／`audit replay`／provenance JSON 三个 sink；epoch 回调的 **cancellation** 分支（`runtime.rs:269-272`）与 deadline 分支同样零覆盖。

**Changed files:**
`crates/lancet-authority/tests/guard_decide_fuzz.rs`; `crates/lancet-authority/tests/policy_tests.rs`; `FIX.md`.

## 两把互不相干的 mutex 守同一个 `XDG_STATE_HOME`，且 poison 后 panic 放大失败 — 2026-07-27

Severity: HIGH（CI 双平台红；本机复现率 11/40 ≈ 27%）。

**Problem:**

`cargo test -p lancet-api --lib` 在 CI 与本机随机失败，两种形态：

- `replay: Config("audit database not found at /var/folders/.../lattice/projects/<hash>/cep_audit.db; run an agent turn in this project first")` —— 测试自己 seed 过的 DB 找不到了。
- `xdg test lock: PoisonError { .. }` 一次性 **9 个测试**同时失败。

根因两条，叠加：

1. **两把互不相干的 mutex 守同一个进程全局变量。** `crates/lancet-api/src/config.rs:200` 的 `audit_state_root_prefers_absolute_xdg_state_home` 里有一把私有的 `static ENV_LOCK`，`crates/lancet-api/src/audit.rs:385` 的 `IsolatedProjectAudit::setup` 里有另一把 `static XDG_TEST_LOCK`。两者都 `set_var("XDG_STATE_HOME")`，而 `config.rs:46` 的 `audit_state_root()` 读它。两把锁**互不排斥** —— 同一 test binary 内并发时，`config` 那个测试把变量改成自己的 tempdir（甚至故意改成 `"relative/path"` 以验证 XDG 规范），持 `XDG_TEST_LOCK` 的 audit 测试随即解析到错误的 state root，于是"自己刚 seed 的 DB 不存在"。加锁给了**虚假的安全感**：代码看起来是串行化的，实际完全没有。

2. **poison 后 panic，把一次失败放大成雪崩。** `audit.rs:395` 用 `.expect("xdg test lock")`。该 guard 跨断言持有，所以任何一个持锁测试失败都会 poison 这把锁，之后每个测试都以 `PoisonError` 失败 —— CI 上 9 failed 里只有 2 个是真失败，另外 7 个是噪音，而且**真正的首个失败被埋在噪音里**，这正是这条 flake 长期没被定位的原因。

**Fix:**

- 收敛为**一把** crate 级锁 `config::XDG_STATE_HOME_LOCK`，放在读取该变量的模块里，并提供 `config::lock_xdg_state_home()` 访问器。两处调用点改为共用它，删除 `audit.rs` 的 `XDG_TEST_LOCK`。
- 访问器用 `unwrap_or_else(|poisoned| poisoned.into_inner())` —— guard 里的数据是 `()`，panic 不可能破坏任何不变量，因此 poison 容忍是正确语义而非将错就错。文档注释写明"永远不要用 `.unwrap()`/`.expect()` 取这把锁"及其原因，防止回归。
- 在 `XDG_STATE_HOME_LOCK` 的 doc 注释里明确记下"必须是全 crate 唯一一把"，并附上这次的失败率数据作为反面教材。

**Verification:**
- **压测**：`cargo test -p lancet-api --lib` 连跑 **60 次 0 失败**（修复前 11/40 ≈ 27%）；修掉顺带的 unused import 后再跑 **40 次 0 失败**。
- `cargo test -p lancet-api` 全 target 绿（18 passed）。
- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` — passed。
- 顺带全仓 sweep 了同类模式：其余三处 env mutex（`lattice-llm`、`lattice-cli/commands/test_env.rs`、`lattice-api/src/test_env.rs`）分属不同 test binary，互不干扰，且后两者已是 poison 容忍的；`lattice-api/src/test_env.rs` 的注释已说明与 `lattice-cli` 副本需保持同步。无需改动。

**Changed files:**
`crates/lancet-api/src/config.rs`; `crates/lancet-api/src/audit.rs`; `FIX.md`.

**后续（合并 GitHub #39 时）:** 上述 `config::XDG_STATE_HOME_LOCK` / `lock_xdg_state_home()` 在与 #39 合并时被移除，**不是回退**：#39 把 XDG 隔离整体下沉到 `lancet-audit::test_support`，`lancet-api` 内再无任何 `set_var("XDG_STATE_HOME")` 调用点，唯一写入者变成持 `ENV_MUTEX` 的 `ScopedXdgStateHome`。「一把锁守一个进程全局变量」的不变量由此从 crate 级升为 workspace 级；此处的锁在合并后已无消费者。

## 并发测试改写进程全局 CWD 导致的跨测试 flake（CI 双平台红）+ 依赖图与 crate 计数文档漂移 — 2026-07-27

Severity: HIGH（阻断 CI 双平台）/ MEDIUM（文档漂移）。

**Problem — CWD flake:**

推送 #80 review 修复后 ordinary CI lane 在 **ubuntu 与 macos 同时**变红，失败形态是 `lattice-api` 的 `consistency` 测试报：

```
create session: Internal("git error: open repository: \"/var/folders/.../T/.tmpo3HKle/sessions\"
does not appear to be a git repository; init_bare: Could not obtain the current directory")
```

诊断：**不是本轮改动引入的回归** —— `git diff 103478f8..HEAD` 对 `crates/lattice-api`、`crates/lattice-session` 零触碰。是一条既有 flake 被这次推送暴露。本机压测复现 **2/40**，与 CI 同签名。

根因链：
1. `crates/lattice-api/tests/{consistency,usage_turn,release_e2e,audit_turn_provenance}.rs` 的请求构造函数调用 `std::env::set_current_dir(&ws.root)` —— CWD 是**进程全局**的，而这些文件各有多个 `#[tokio::test]` 在同一进程内**并发**执行，每个持有自己的 `TempDir`。
2. 测试 A 把进程 CWD 指向自己的 TempDir；A 结束、`TempDir` drop 删目录后，进程处于「CWD 指向已删除路径」状态。
3. 此时并发的测试 B 调 `gix` —— **`gix` 即使对绝对路径也会读取进程 CWD**，于是 B 以 `Could not obtain the current directory` 失败，尽管 B 自己的目录完好无损。
4. `crates/lattice-session/src/test_utils.rs:32-50` 已有针对该现象的恢复逻辑（且注释明确点名 lattice-api 的这三个 fixture 是肇事者），但**生产路径** `git_graph/mod.rs` 的两处 init 没有。

**关键观察**：这四处 `set_current_dir` 全都是**多余的** —— 同一个请求结构体里已经显式传了 `test_project_root`（`consistency.rs:169` 等），而 `crates/lattice-api/src/turn/mapping.rs:36-43` 的 `resolve_project_root` 只在 `test_project_root` 为 `None` 时才回落到 `current_dir()`。也就是说这些测试从不依赖 CWD，纯粹在制造危害。

**Fix:**

1. **删掉四处多余的 `set_current_dir`**（治本：消除竞态源头，而非用 mutex 串行化掩盖）。每处留注释说明为何**故意不设** CWD，避免被"好意"加回。
2. **给生产路径补 CWD 恢复**：`git_graph/mod.rs` 新增 `init_recovering_cwd()`，包裹 `gix::init_bare` 与 worktree init 两条路径。失败信息含 `CurrentDir` 时，把 CWD 指到 `temp_dir()` 后重试一次。这不只是测试便利 —— 真实用户在 checkout 被删、卷被卸载后会撞上同一个无从下手的错误。
   实现中踩到并修掉一个坑：**朴素重试会失败**。gix 在需要 CWD 之前就已创建 `.git` 目录，所以首次失败会留下半成品，重试报 `Refusing to initialize the existing '...' directory`（压测实测到这一形态）。故 `init_recovering_cwd` 接受一个 `reopen` 闭包，第二次尝试失败时改为 `open` 已存在的仓库。

**Problem — 文档漂移（顺带修，见 #102 / #86 / #108）:**

- `AGENTS.md:45-62` 的分层依赖图与实际 `Cargo.toml` 有 **7 条差集**：`lattice-llm -> net-guard` 未画（且这使 `net-guard` 与 `lattice-llm` 不能同列 Layer 0）；`lattice-api` 实际还直连 `lancet-protocol`/`lancet-catalog`/`lancet-authority`/`lancet-native`，图里概括成 "+ lattice runtime crates"；`lattice-cli` 实际还直连 `lancet-api`。全部边都是**向下**的，不构成分层违规，但概括式表述无法与 `cargo metadata` 比对，因而藏住了这些边。
- crate 计数在四份文档里各说各话：README 15、`docs/ARCHITECTURE.md` 15、`docs/DEVELOPMENT.md` 16、`docs/GETTING-STARTED.md` 16，实际 **19**；README 的 crate 表还漏了 `lancet-runtime`、`lattice-tool-prelude`、`lattice-system-harness` 三个。
- `docs/DEVELOPMENT.md` 说需要 Rust **1.88+**，实际 `Cargo.toml` 要求 1.94、`rust-toolchain.toml` 钉 1.96.0。
- README 的 `lancet-native` 角色写着 "verify, edit/shell/web helpers" —— `exec.command_run` 与 post-mutation verify 已由 #26/#37 **永久退役**，属活文档里的退役词汇。

**Fix:** 逐条改正；`AGENTS.md` 与 README 的图改为**逐条列出每条真实边**（并说明为何不再用概括写法），补上 `lancet-runtime`，并注明 `lattice-tool-prelude`/`lattice-system-harness` 按契约不在图内（已核实二者确无 workspace 内部依赖）。新增门禁测试 `documented_crate_counts_match_the_crates_directory`，把四份文档的计数钉到 `crates/` 目录实际条目数。同时在 `AGENTS.md` 图下显式记录「今天只有一条边被机器检查」并指向 #108。

**Verification:**
- **压测（本修复的核心证据）**：`consistency` 连跑 **100 次 0 失败**（修复前 2/40 ≈ 5%）；`usage_turn` / `release_e2e` / `audit_turn_provenance` 各 30 次 0 失败。
- `cargo test -p lattice-api` — 全部 target 绿（91 + 8 + 5 + 5 + 10 + 其余）。
- `cargo test -p lattice-session` — 54 passed / 1 ignored。
- `cargo test -p xtask` — 48 passed（含新增计数门禁）。
- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` — passed。
- 计数门禁**反向验证**：把 `docs/GETTING-STARTED.md` 改成 "18 crates" → FAIL 并给出可执行提示；改回后 PASS。

**Changed files:**
`crates/lattice-api/tests/{consistency,usage_turn,release_e2e,audit_turn_provenance}.rs`; `crates/lattice-session/src/git_graph/mod.rs`; `AGENTS.md`; `README.md`; `docs/{ARCHITECTURE,DEVELOPMENT,GETTING-STARTED}.md`; `xtask/src/ci.rs`; `FIX.md`.

## GitHub #80 review 收口：Component 里的宿主绝对路径、verify 证据丢失、encoder 指纹缺失 — 2026-07-27

Severity: MEDIUM（阻断 #73 验收标准的结构性缺陷 + 证据收集失效）。

对 PR #104（#80 的实现）做对抗性审查后落地的六项修复。三批独立 lens 高度重合地指向同一组缺陷，逐条实证核实后修复。推进 `FINDINGS.md` 的 **L-5-02**（仍为 ACCEPTED RESIDUAL，跨机证据未取得）。

**Problem:**

1. **committed `.wasm` 里烧进了维护者主机的 rustup sysroot 绝对路径（最严重）。** `xtask/src/tool_build.rs` 的 `build_guest` 只设了一条 `--remap-path-prefix`（workspace root），管不到 rustc 通过**本地 rustup 安装**解析的 std 源码路径。实测 `strings tools/noop/generated/noop_echo.component.wasm` 命中 `/home/astrin/.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/core/src/str/pattern.rs` —— 而同一文件里其余 std 路径已被 rustc 规范化为 `/rustc/ac68faa2.../library/...`，唯独这一条漏网。后果：**任何跨机 `--verify` 都必然报 digest 漂移**，#80 刚接进 CI 的那条 lane 注定每晚误报，#73 要的跨机 bit-for-bit 证据在结构上无法取得。
2. **verify 步骤缺 `if: always()`，且排在最易失败的 rollout 之后。** `.github/workflows/full-gate.yml` 把 `tool-build --verify` 放在 `cargo xtask rollout --channel alpha` 后面（意图是"跑最后，避免漂移警告掩盖前面的硬失败"），但没有 `if: always()`。rollout 是整条 lane 里最容易失败的 gating 步骤，它一失败 verify 直接被跳过 —— 恰好丢掉最值得检查的那些运行的证据。
3. **verify 结果只存在于 job stdout。** 不进任何 `upload-artifact`，`#73` 所谓"可被引用的证据"实际取不回来。
4. **步骤名标 `warn-only` 但该步骤有 6 条硬失败路径。** `tool_build.rs` 里只有 digest 不匹配是 warn（`:119-123`）；guest build、`wasm-tools component new`、`validate`、`wasi:*` import 断言、WIT world 一致性全部 `bail!`。名字与文档都在暗示"这步不会让 lane 变红"，与实际不符。
5. **`EXPECTED_WASM_TOOLS` 的 doc 声称与 manifest 的 `[artifact] toolchain` 指纹保持同步，但该指纹里根本没有 wasm-tools**（原值 `rustc-1.96.0+wit-bindgen-0.59`）。而 encoder 恰恰决定 Component 字节，指纹漏掉它使 sha256 无法被解释。
6. **`wasm_tools_version` probe 在工具缺失时仍写 `ok: true`**（`xtask/src/rollout.rs`），与紧邻的 `git_version`（探测失败写 `ok: false`）处理相反，release record 会声称成功探测了一个从未找到的工具。另外 `check_toolchain` 把"存在但不可用"也报成 `not found on PATH`，引导用户做无效重装。

**Fix:**

1. `build_guest` 改为设置**两条** `--remap-path-prefix`：workspace root，以及 `<sysroot>/lib/rustlib/src/rust` → `/rustc/<commit-hash>`（新增 `rustc_sysroot_and_commit_hash()` 从 `rustc --print sysroot` 与 `rustc -vV` 的 `commit-hash:` 读取，任一失败返回 `None` 而不影响构建 —— 少一条 remap 只损失可复现性，不该让构建失败）。映射到 rustc 自己用的同一前缀，使两种表示一致。重新 pin 后 digest 由 `6fcc7fc8...` 变为 `b677a9b3...`。
2. verify 步骤补 `if: always()`，并在注释里写明理由（前置 rollout 最易失败，跳过就等于丢掉最该看的证据）。
3. 输出 `tee` 到 `target/rollout/component-verify.log`，由既有的 `if: always()` upload 一并带走。用 `set -o pipefail` + `shell: bash`，否则 `tee` 会吞掉硬失败的退出码；`mkdir -p` 兜住 rollout 尚未创建目录的情况。
4. 步骤名改为「Component 可复现性（digest 漂移 warn-only，#73 证据）」，并在 workflow 注释与 `docs/TESTING.md` 里明确列出 6 条硬失败路径 —— 是"漂移 warn-only"，不是"整步 warn-only"。
5. `tools/noop/manifest.toml` 的指纹补为 `rustc-1.96.0+wit-bindgen-0.59+wasm-tools-1.254.0`；`EXPECTED_WASM_TOOLS` 的 doc 改为列明必须一致的三处；新增门禁测试 `every_tool_manifest_fingerprint_names_the_expected_wasm_tools` 逐个 manifest 断言指纹含当前 encoder 版本，防止改 pin 而不重 pin 制品。
6. probe 改为 `(value, ok)` 二元组，缺失时写 `ok: false` 并给出原因，与 `git_version` 对齐；新增 `WasmToolsProbe` 枚举区分 `NotOnPath`（仅 ENOENT）/ `Failed`（权限、非零退出、输出不可解析），`check_toolchain` 据此给出"安装"还是"修复"的正确指引。

**顺带修复（由 1 触发的连带缺陷，实证了 #103）:** 重新 pin 后 `crates/lancet-runtime/src/generated/noop_fixture.rs` 里**手写**的两个 digest 与生成物脱节，composition gate 会 fail-closed 拒绝 fixture。该文件位于 `generated/` 目录下却无人生成、无人校验，正是 #103 记录的缺口。除同步两个 digest 外，新增 `fixture_digests_match_the_generated_expected_identity` 测试：`include_str!` 读入 `expected-identity.json` 并逐项比对 artifact/descriptor digest 与 abi-version，漂移时给出"跑完 `tool-build` 后把它抄过来"的可执行提示。反向验证：故意改末位一字符 → 测试 FAILED 且报出正确的期望值。

**文档同步:** `docs/TESTING.md` 更正三处 —— pinned `wasm-tools` 安装不再被算作"ordinary lane 也跑的步骤"（ordinary lane 确实不装它）、补齐 warn-only 的准确边界、记录 `if: always()` 与 log 落盘位置。`CONTRIBUTING.md` 补齐两处 —— §2 新增 `wasm-tools 1.254.0` 前置（`cargo xtask ci` 缺它直接 fail，而这是强制的 pre-PR 门），§7 的 `xtask ci` 步骤清单由 5 步过时描述更正为实际 9 步（原先漏 toolchain precheck、`lancet-boundary`、`tool-codegen --check`、`tool-build --verify`，且顺序有误）。

**Verification:**
- `cargo fmt --all --check` — passed。
- `cargo clippy --workspace --all-targets -- -D warnings` — passed。
- `cargo xtask tool-codegen --check` — passed（重新生成后一致）。
- `cargo xtask lancet-boundary` / `harness-boundary` — passed。
- `cargo xtask tool-build --verify` — `verify: noop-echo Component reproduces committed digest (b677a9b3...)`，即修复后本机仍幂等复现。
- `strings` 核验：`/home/` 命中数由 1 降为 **0**；`core/src/str/pattern.rs` 现归入 `/rustc/ac68faa2.../` 与其余 std 路径一致。
- `cargo test -p xtask` — 47 passed / 0 failed（含 2 条新增/相关门禁测试）。
- `cargo test -p lancet-runtime --features test-utils` — lib 7 + 11 + 11 passed / 0 failed，含新增 digest 同步测试；负向验证该测试确实会 FAIL。

**Residual:** 跨机 bit-for-bit 证据**仍未取得** —— 本次只是移除了让它必然失败的结构性阻碍（sysroot 路径）与让它无法被收集的流程缺陷（跳过、不上传）。需要一次 nightly full-gate 实跑产出 `component-verify.log`，把 runner 的 rebuilt digest 与本机 `b677a9b3...` 比对，才能按 #5 decision 1 escalation 决定是否把 `--verify` 升为硬门禁。详见 `FINDINGS.md` 的 **L-5-02**。另外 `noop_fixture.rs` 仍是"位于 generated/ 却手写"的状态，本次只加了漂移检测，真正的生成化归 **#103**。

**Changed files:**
`xtask/src/tool_build.rs`; `xtask/src/ci.rs`; `xtask/src/rollout.rs`; `.github/workflows/full-gate.yml`; `tools/noop/manifest.toml`; `tools/noop/generated/{descriptor.json,expected-identity.json,noop_echo.component.wasm}`; `crates/lancet-runtime/src/generated/noop_fixture.rs`; `docs/TESTING.md`; `CONTRIBUTING.md`; `FINDINGS.md`; `FIX.md`.

## GitHub #105 / #106: harness 进程组信号 fail-closed + git_graph fixture 唯一路径 — 2026-07-27

Severity: CI 阻塞（#105，整条 ubuntu lane 长期红）/ test isolation（#106）。

收口 `FINDINGS.md` 的 **L-47-01** 与 **L-47-02**。

**Problem:**
- **#105** `crates/lattice-system-harness/src/driver.rs` 用 `kill -INT -<pgid>` 向整个进程组投递 SIGINT，pgid 直接取 `child.id()`（`:82`），等式仅在 `process_group(0)` 生效、子进程自成组长时成立，而代码里没有任何断言保护该前提。前提一旦不成立，`-<pgid>` 就落到**测试进程自己的进程组**，把 `cargo test` 一并 SIGINT。信号还经由外部 `kill(1)` 发送（`:269`），负号参数的解析多了一层 `kill` 实现差异，且广播错目标时 `kill` 仍返回成功——失败是静默的。实测后果：ubuntu-latest 上 `cargo test --workspace` 以 **exit 130** 终止而 **0 failed、无 panic**（PR #104 的 run 30222133081 停在 `lattice-system-harness` lib 的 `artifacts::tests::*` 中途），macos 同 commit 全绿，本地不复现。ordinary CI lane 的 ubuntu 作业因此长期红，#77/#78/#104 全卡在同一处。
- **#106** `crates/lattice-session/src/git_graph/tests/mod.rs:23-29` 的 `temp_dir(name)` 拼出主机全局固定路径 `$TMPDIR/lattice-session-git-graph-tests/<name>` 并在返回前 `remove_dir_all` 它，唯一性只靠调用方传的 `name`。同主机并行的两个测试二进制（两个 `CARGO_TARGET_DIR`，或两个 git worktree）会互删对方正在用的 fixture，报 `Refusing to initialize the existing '...' directory` 或 `NotARepository`。这是仓内唯一不用 `tempfile` 的手写共享路径 fixture。

**Fix:**
- `driver.rs`：新增 `resolve_child_pgid()`，spawn 后用 `libc::getpgid(child_pid)` 从内核**回读**实际 pgid，不再假定等于 `child.id()`；`setpgid` 失败或不受支持时不会静默留下指向自身组的 pgid。
- `driver.rs`：新增 `reject_own_process_group()`，任何组信号发送前先比对 `libc::getpgrp()`，命中自身组则返回 `Err` 而非广播。该守卫同时覆盖 SIGINT 路径与 `kill_process_group()` 的 SIGKILL 路径（后者广播错目标更致命）。
- `driver.rs`：`signal_process_group` 改为直接 `libc::killpg(pgid, sig)`，删除 spawn `kill(1)`，消除负号参数在 coreutils/shell builtin/busybox 间的解释差异，并能拿到真实 `errno`。信号常量改用 `libc::SIGINT`/`libc::SIGKILL` 而非字符串。
- `git_graph/tests/mod.rs`：`temp_dir()` 改为返回持有 `tempfile::TempDir` 的 `Fixture` 守卫，路径由 `tempfile` 保证唯一、drop 时自动清理；`name` 退化为可读前缀（`TempDir::with_prefix`）便于失败辨认。删除手写的 `remove_dir_all` 前置清理。30 个调用点机械改写为 `dir.path()`。

**Verification:**
- `cargo fmt --all --check` — passed。
- `cargo clippy -p lattice-session -p lattice-system-harness --all-targets -- -D warnings` — passed。
- `cargo test -p lattice-system-harness --lib` — 57 passed / 0 failed（含两条新增回归：`signalling_own_process_group_is_refused` 断言对自身组发信号被拒且 SIGKILL 包装保持 no-op；`spawned_child_pgid_is_read_back_and_distinct` 断言回读的 pgid 既不等于 harness 自身组、又等于子进程 pid）。
- `cargo test -p lattice-system-harness --test direct_failures` — 8 passed / 0 failed，`direct_failure_sigint_while_stalled_commits_cancelled` 仍在跑且通过。SIGINT 协作取消的覆盖**未被削弱**（#46 cancellation 契约的核心证据保留）。
- `cargo test -p lattice-session` — 54 passed / 0 failed / 1 ignored。
- **#106 反向验证**：并行两份 `CARGO_TARGET_DIR=/tmp/lat-par-{a,b} cargo test -p lattice-session git_graph`，两侧各 29 passed / 0 failed。修复前同样的并行会有 2 个测试因 fixture 互删而失败。
- `cargo xtask harness-boundary` — passed（3/3 组）。`cargo xtask lancet-boundary` — passed。

**Note:** `libc` 在 harness 清单里写死 `"0.2"` 而不用 workspace 继承——`harness-boundary` 的 `manifest_workspace_inheritance` 检查是**逐行文本匹配**，连注释里出现该字面量也会 FAIL（其自身测试 `workspace_true_in_comment_still_detected` 正是有意如此），本次为此调整了注释措辞。

**CI 实跑确认（2026-07-27，补记）:** ubuntu lane 已转绿，#105 建议清单第 4 步（`#[ignore]` + 专用 serial lane）确认**不需要**落地。证据是一组差一个提交的对照：PR #104（`44ceabc1` + `38af35bb`）在 ubuntu-latest 上仍以 `Process completed with exit code 130` 失败，失败日志中所有已报告测试均为 `ok`、无 FAILED、无 panic（run 30222133081 job 89944793831），同 run 的 macos-latest 通过；PR #107 在这两个提交之上仅多出本修复提交 `103478f8`，ubuntu-latest 与 macos-latest 双双通过（run 30257981171）。两次运行的唯一差异即本修复，故信号源确在 harness 内且已消除。

**Changed files:**
`crates/lattice-system-harness/src/driver.rs`; `crates/lattice-system-harness/Cargo.toml`; `crates/lattice-session/src/git_graph/tests/mod.rs`; `crates/lattice-session/src/git_graph/tests/{commit_turn,head_transcript,no_remote,pointer_ops,session_meta}.rs`; `FINDINGS.md`; `FIX.md`.

## GitHub #79 / #80: harness repo-root determinism + Component 构建工具链身份 — 2026-07-27

Severity: test correctness (cross-checkout write) / release-evidence gap.

**Problem:**
- **#79** `crates/lancet-native/tests/cep_warn_harness.rs` 的 `find_repo_root()` 向上搜索 `.planning/` 作为 repo root 标记。`.planning/` 被 `.gitignore:10` 忽略、被 `.githooks/pre-commit` 拦截强制添加，因此在 CI 与全新 clone 里**从不存在**——注释描述的机制只在维护者主检出上生效，其余环境全靠 fallback。更严重的是：`git worktree` 建在 `.claude/worktrees/<name>/` 下且没有 `.planning/`，向上搜索会越过工作树根命中**主检出**。实测从工作树运行时读的是 `/home/astrin/Dev/lattice/docs/lancet-warn-report.md`，`LATTICE_UPDATE_WARN_REPORT=1` 下会写进另一份检出的被跟踪文件。同文件 `:404` 的 doc 注释写 `all 8 CATALOG methods`，封闭目录实为 5 个。
- **#80** `cargo xtask ci` 第 9 步跑 `tool-build --verify`（唯一把已提交 `.wasm` 绑回其源码的门），但 `.github/workflows/ci.yml` 与 `full-gate.yml` 都不调用它，且两条 lane 都不安装 `wasm-tools`——`#73` 验收标准 A 要求的「CI runner 上的跨机 bit-for-bit 证据」在结构上无法产生。`wasm-tools` 作为决定 Component 字节的编码器，既不在 `check_toolchain()` 的校验里、不在 rollout capability probes 里、也不是任何清单中的依赖。`wit-bindgen` 用 caret `"0.59"` 而非 `=` 精确 pin，与 wasmtime/inventory/jsonschema 的口径不一致。

**Fix:**
- `cep_warn_harness.rs`：`find_repo_root()` 改为编译期 `env!("CARGO_MANIFEST_DIR")` 上溯两级，删除运行时向上搜索与 fallback；注释写明为何不得再用标记目录。`:404` 的方法数改为 5。
- `.github/workflows/full-gate.yml`：新增按 SHA 固定的 `taiki-e/install-action` 安装 `wasm-tools@1.254.0`；在 rollout 之后新增 `cargo xtask tool-build --verify` 步骤（warn-only，置于最后以免漂移告警掩盖更早的硬失败）。放 full-gate 而非 ordinary lane，以保持 PR 门轻量，同时让每晚一次的运行持续累积 `#73` 需要的跨机数据。
- `xtask/src/ci.rs`：`check_toolchain()` 增加 `wasm-tools` 探测——缺失即 fail（早于第 9 步给出可操作错误），版本不符只告警（与 `#5` decision 1 的 warn-only 口径一致）。新增 `EXPECTED_WASM_TOOLS` 常量与可单测的 `parse_wasm_tools_version()` 纯函数。
- `xtask/src/rollout.rs`：`collect_probes()` 增加非 gating 的 `wasm_tools_version` probe，让 release 记录能回答「这份 release 的 Component 由哪个编码器构建」。
- `Cargo.toml`：`wit-bindgen = "=0.59.0"` 进 `[workspace.dependencies]`；`crates/lattice-tool-prelude` 与 `tools/noop` 改用 `{ workspace = true }`，单一 pin 源。顺带修正 prelude 中「guest 从不直接依赖 wit-bindgen」的失实注释（`FINDINGS.md` L-5-02 记录的正是相反事实）。
- `docs/TESTING.md`：登记 `wasm-tools 1.254.0` 为构建 Source Tool 的外部前置依赖；同步 `xtask ci` 第 1 步、rollout capability probes 与 full-gate 步骤清单。

**Verification:**
- `cargo fmt --all --check` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed。
- `cargo test -p xtask` — 46 passed（新增 3 条：版本解析正/负用例，以及断言 `EXPECTED_WASM_TOOLS` 与 full-gate 安装 pin 一致）。
- `cargo test -p lancet-native --test cep_warn_harness -- --nocapture` — passed；输出路径由修复前的 `/home/astrin/Dev/lattice/docs/lancet-warn-report.md`（主检出）变为 `/home/astrin/Dev/lattice/.claude/worktrees/code-audit/docs/lancet-warn-report.md`（本工作树）。
- 反向证明：在本工作树的报告尾部追加一行 `| probe | overreach | NO | fix-now |` 后该测试 FAILED（修复前不会失败，因为断言作用在另一份检出上）；文件已还原，`git diff` 干净。
- `cargo xtask tool-codegen --check` — passed。
- `cargo xtask tool-build --verify` — `verify: noop-echo Component reproduces committed digest (6fcc7fc8570be61f887278661f4a5a6353c3ca2c46e249dcc8de843500f73df4)`。该结果同时构成 `#73` 的一份追加证据：本次是在与原构建**不同的绝对路径**（git worktree）下复现的，说明 `--remap-path-prefix` 对路径差异有效；仍缺的是异构机器/CI runner 那一半。
- `Cargo.lock` 无变化（`=0.59.0` 与既有锁定版本一致），committed `.wasm` digest 未变。

**Residual:** `#80` 的「把 `wasm-tools` 版本写进 `tools/*/manifest.toml` 的 `[artifact] toolchain` 指纹」未在本次落地。原因：该字段进入 canonical descriptor，会改变 `descriptor-sha256`，进而需要手工同步 `crates/lancet-runtime/src/generated/noop_fixture.rs:30` 里手抄的常量（该目录名为 `generated/` 但无任何工具生成，见 #103）。留在 `#80` 内跟进。

**Changed files:**
`crates/lancet-native/tests/cep_warn_harness.rs`; `.github/workflows/full-gate.yml`; `xtask/src/ci.rs`; `xtask/src/rollout.rs`; `Cargo.toml`; `crates/lattice-tool-prelude/Cargo.toml`; `tools/noop/Cargo.toml`; `docs/TESTING.md`; `FIX.md`.

## GitHub #39: sink operator audit read into `lancet-audit` + four Local Workspace defects — 2026-07-27

Severity: architecture / ownership boundary (half 1) + security-hardening and correctness (half 2).
GitHub #39 = Codeberg #17; the bare `#37/#38/#39/#40` workstream-A references elsewhere in this
file are Codeberg numbers (GitHub #59–#62) and are a different ticket.

**Problem:**
- **Half 1 — misplaced ownership.** `lancet-api::audit` owned the operator audit read
  *behavior* (DTOs, the tail pagination loop, filter construction, wire projection, the replay
  attestation comparison) even though every byte it touches belongs to `lancet-audit`'s store.
  The external project-keyed store-path helpers sat in `lancet-api::config` for the same
  accidental reason. Six operator-only DTOs squatted in `lancet-catalog/src/methods/audit.rs`,
  inflating the central catalog surface that #34 has to delete, and inviting a future reading of
  operator audit as a Tool contract. The "operator reads never compose a CEP server" invariant was
  asserted by two source-string-scan tests that RFC #41 decision 6 had already deleted, leaving
  three module doc comments pointing at nothing. Two byte-identical copies of the
  `XDG_STATE_HOME` test helper (`lattice-api/src/test_env.rs`,
  `lattice-cli/src/commands/test_env.rs`) each carried their own process-global env mutex — the
  exact duplication shape that produced the ENV-OOM-02 flake.
- **Half 2 — four PR #74 review defects in the Local Workspace** (`lancet-native`
  `src/workspace/local.rs`): grep read whole files with `fs::read_to_string` (unbounded memory on
  a large file, and one invalid UTF-8 byte discarded every match in the file); `list_dir` used
  following `metadata()`, so a single dangling symlink failed the entire listing instead of
  reporting `PathKind::Symlink`; `glob_match` was a substring/prefix approximation rather than a
  real matcher; and the write path resolved securely but then re-touched the filesystem *by path*,
  leaving a TOCTOU window between `resolve_secure` and `create`/`rename`/`unlink` in which a
  parent component could be swapped for a symlink.

**Fix:**
- New `lancet-audit/src/operator.rs` owns `AuditRequest`, `AuditQueryOutput`, `AuditTailOutput`,
  `AuditReplayRequest`, `AuditReplayResult`, `AUDIT_TAIL_MAX_ROWS`, `AUDIT_TAIL_MAX_BATCHES`,
  `OperatorAuditError{Config,Internal}`, `OperatorAuditResult`, and async `query_audit` /
  `tail_audit` / `replay_audit`. Error message strings are byte-identical to the old `lancet-api`
  ones. New `operator_tests.rs` holds the moved contract tests; new `store_path.rs` holds
  `ExternalAuditPathError`, `audit_state_root`, `project_storage_key`,
  `audit_db_path_for_project`, `audit_db_path_for_project_in` and their tests.
- New `lancet-audit/src/test_support.rs` behind feature `test-support` is now the single
  workspace-wide `ScopedXdgStateHome`; both crate-local copies were deleted and `lattice-api` /
  `lattice-cli` consume it as `lancet-audit = { workspace = true, features = ["test-support"] }`
  dev-dependencies. `lancet-audit` gained `dirs` + `sha2` and optional `tempfile`.
- `lancet-api/src/audit.rs` is now a facade: re-exports plus three one-line delegations mapping
  `OperatorAuditError` → `LancetError`. `src/config.rs` keeps `AuditBacking` +
  `LancetRuntimeConfig` and re-exports the moved path helpers; `src/lib.rs` re-exports
  `AuditReplayResult` from `lancet_audit`. Every `lancet_api::*` public path is unchanged, and
  the now-unused `dirs` / `sha2` deps were dropped.
- `lancet-catalog/src/methods/audit.rs` deleted with its `methods/mod.rs` wiring, removing all six
  operator DTOs (`AuditQueryParams`, `AuditQueryResult`, `AuditTailParams`, `AuditTailResult`,
  `AuditReplayParams`, `AuditReplayResult`). None became a Tool descriptor, generated inventory
  entry, Agent catalog method, or Component.
- **Structural consequence:** `lancet-audit` depends only on `lancet-protocol`, `lancet-catalog`,
  `blake3`, `dirs`, `rusqlite`, `serde`, `serde_json`, `sha2`, `thiserror`, `tokio`, `tracing`
  (+ optional `tempfile`) — no `lancet-native` / `lancet-dispatch` / `lancet-cep` /
  `lancet-runtime`. "Operator audit read never composes a CEP server" is therefore a crate
  dependency-graph guarantee, not a source-string-scan assertion. Three module doc comments
  (`lancet-audit/src/operator.rs`, `lancet-api/src/audit.rs`, `lattice-cli/src/commands/audit.rs`)
  were rewritten to say exactly that, replacing the dangling references to the deleted scan tests.
- **Grep byte budget:** `GREP_MAX_FILE_BYTES` (8 MiB), `GREP_MAX_TOTAL_BYTES` (64 MiB walk
  aggregate), `GREP_MAX_LINE_BYTES` (64 KiB), `struct GrepBudget`; new `grep_file` streams with
  `BufReader` + `Read::take`. Files above the per-file cap are skipped before being opened.
- **`list_dir` symlinks:** uses `DirEntry::file_type()` (no following) via the new
  `file_type_kind`, so symlinks — including dangling ones — list as `PathKind::Symlink` instead of
  failing the whole listing. `metadata_kind` remains for the `resolve_secure` callers.
- **`glob_match`:** rewritten as an iterative two-pointer fnmatch matcher supporting `*`, `?`, and
  `[abc]` / `[a-z]` / `[!...]` classes (unclosed `[` is literal), with a `match_char_class` helper.
  It backs both `find_files` patterns and grep's glob file filter.
- **Write-path TOCTOU closed:** new private `LocalWorkspace::open_parent_dir_secure` walks the
  parent chain component-by-component with `rustix` `openat`
  (`O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`) relative to the already-pinned fd, with `mkdirat`
  + EEXIST-tolerant retry when creating. `create_excl`, `install_atomic`, and `remove_file` now
  reach the filesystem only through that pinned fd with single-component leaf names (`openat`
  `O_CREAT|O_EXCL`, `renameat`, `unlinkat`, `fsync` on the dirfd); the path-based `fsync_dir` is
  gone. Portable across Linux + macOS (no `openat2`/`O_PATH`). It is deliberately NOT claimed as
  `openat2(RESOLVE_BENEATH)`-equivalent: adversarial review of this branch demonstrated (C replica)
  that pinning defeats symlink substitution but NOT a rename of the pinned directory out of the
  workspace between the pin and the leaf operation, because the fd follows the inode and nothing in
  that attack is a symlink. Recorded as accepted residual `L-39-04`; the original reported defect
  (a parent component swapped for a **symlink** after `resolve_secure`) is closed.
  `rustix` (features `["fs"]`) added to root `[workspace.dependencies]` and
  `crates/lancet-native/Cargo.toml`.
- **`lancet-native` public surface contained:** `pub mod audit` now re-exports `lancet_audit::audit`
  (persistence only) instead of globbing the `lancet_audit` crate root, which since this issue also
  carries the operator read path — a root glob would have republished `query_audit` / `tail_audit` /
  `replay_audit` as `lancet_native::audit::*`.
- **New boundary gate:** `cargo xtask lancet-boundary` gained
  `lancet_audit_forbidden_dependency_graph`, failing if `lancet-audit` ever gains a `lancet-native` /
  `lancet-dispatch` / `lancet-cep` / `lancet-runtime` dependency (dev-dependencies included). The
  triage ledger justified deleting the two source-string-scan boundary tests with
  "架构边界由 crate 依赖/边界检查负责" — that owner did not previously exist. Negative-tested:
  adding `lancet-cep` to `lancet-audit` makes the gate FAIL.
- **Deliberate behavior deltas:** an over-cap file is silently skipped by grep (callers see fewer
  matches, no error); grep keeps matches found before a mid-file IO/invalid-UTF-8 error instead of
  dropping the whole file; a parent path component that is a regular file now surfaces as
  `SymlinkRejected` (ENOTDIR from the `O_DIRECTORY` walk) rather than `Io`.
- Ledger: 10 identities moved `lancet-api` → `lancet-audit` (decision/target-layer/reason
  unchanged), 4 line-only refreshes stayed in `lancet-api`, and
  `audit_replay_params_require_turn_id_field` flipped keep → delete with its DTO. Totals
  keep 1191→1190 / delete 216→217 / elevate 42 unchanged (1449 identities unchanged).

**Adversarial acceptance pass (13 agents, one skeptic per acceptance criterion plus a refuter
per PASS).** 6 criteria PASS with the refutation upheld; the Local Workspace criterion came back
PARTIAL and produced four real corrections, all applied here:
- The `open_parent_dir_secure` doc claimed `openat2(RESOLVE_BENEATH)` equivalence. A C replica of
  the walk disproved it (renaming the pinned directory out of the workspace between pin and leaf
  op lands the create outside). Doc comment, `AGENTS.md`, and this entry corrected; residual filed
  as `L-39-04`. The originally reported symlink-substitution defect remains closed.
- `pub mod audit` in `lancet-native` globbed the `lancet_audit` crate ROOT, so this issue silently
  republished `query_audit`/`tail_audit`/`replay_audit` as `lancet_native::audit::*`. Narrowed to
  `lancet_audit::audit` (persistence only).
- The install temp name had no `EEXIST` retry, so pid recycling after a SIGKILL (or two PID
  namespaces over one bind-mounted workspace) surfaced a bare `Io` error. Now retries up to
  `INSTALL_TMP_NAME_ATTEMPTS` fresh sequence numbers.
- `lancet-api` carried `rusqlite` as an unused dev-dependency after the legacy-schema tests moved
  out; removed. Three `lattice-api` integration fixtures still set `XDG_STATE_HOME` with a raw
  unlocked `unsafe set_var`; converted to the shared `ScopedXdgStateHome` guard, so
  `grep -rn 'set_var("XDG_STATE_HOME"' crates/` is now empty workspace-wide.
- Two tail-cursor edges the move could have silently regressed had no test: added
  `tail_audit_empty_result_keeps_requested_cursor` (the `unwrap_or(since)` fallback) and
  `tail_audit_exactly_max_rows_is_a_single_batch` (the `>` vs `>=` cap boundary).
- Remaining review findings accepted as residuals `L-39-04` … `L-39-07`.

**Verification:**
- `cargo fmt --all --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test -p lancet-audit --features test-support` — 52 passed.
- `cargo test -p lancet-api` — 7 passed.
- `cargo test -p lancet-catalog` — 26 passed.
- `cargo test -p lancet-native` — 139 lib + 16 integration passed (155 total).
- `cargo test -p lattice-cli` — 108 lib + 4 integration passed.
- `cargo test -p lattice-api` — 91 lib + 45 integration passed (136 total), including the three
  converted `ScopedXdgStateHome` fixtures.
- New gate `cargo xtask lancet-boundary` → `PASS [lancet-audit forbidden dependency graph
  (operator read stays CEP-server-free)]`, negative-tested: adding `lancet-cep` to
  `crates/lancet-audit/Cargo.toml` makes it FAIL.
- Ledger recount: 1449 identities; keep 1190 / elevate 42 / delete 217; prose per-crate headers
  and the §1 summary tables re-derived from `docs/test-triage-decisions.json` with zero mismatches.
- `cargo xtask harness-boundary` / `lancet-boundary` / `tool-codegen --check` — all passed.
- Operator audit black-box lanes (`LATTICE_BIN=target/debug/lattice`):
  `cargo test -p lattice-system-harness --test fast_audit_cli` — 4 passed (missing-store
  query json/text, tail, replay); `--test direct_operator` — 3 passed
  (`direct_operator_audit_session_lancet_cross_validate` cross-validates a real seeded turn:
  query rows + `--method` / `--verdict` filters + `--tail` + replay with
  `stored_statement_match=true`; plus unknown-session-empty and unknown-turn-fails).
- `cargo xtask ci` — **All checks passed** (`CARGO_BUILD_JOBS=4`; no `LATTICE_SKIP_KNOWN_HANGS`
  needed). Run twice: once before the adversarial pass (2:58.82 wall) and once after its
  corrections landed.

**Changed files:**
`Cargo.toml`; `Cargo.lock`;
`xtask/src/lancet_boundary.rs`; `xtask/src/lancet_boundary/check.rs`;
`crates/lancet-native/src/lib.rs`;
`crates/lattice-api/tests/{audit_turn_provenance,dogfood_impact_matrix,release_e2e}.rs`;
`crates/lancet-audit/Cargo.toml`; `crates/lancet-audit/src/lib.rs`;
`crates/lancet-audit/src/{operator,operator_tests,store_path,test_support}.rs` (new);
`crates/lancet-api/Cargo.toml`; `crates/lancet-api/src/{audit,config,lib}.rs`;
`crates/lancet-catalog/src/methods/audit.rs` (deleted); `crates/lancet-catalog/src/methods/mod.rs`;
`crates/lancet-native/Cargo.toml`; `crates/lancet-native/src/workspace/local.rs`;
`crates/lattice-api/Cargo.toml`; `crates/lattice-api/src/lib.rs`;
`crates/lattice-api/src/test_env.rs` (deleted); `crates/lattice-api/src/turn/{cep,cep_tests}.rs`;
`crates/lattice-cli/Cargo.toml`; `crates/lattice-cli/src/commands/{audit,mod}.rs`;
`crates/lattice-cli/src/commands/test_env.rs` (deleted);
`docs/test-triage-decisions.json`; `docs/TEST-TRIAGE.md`; `docs/ARCHITECTURE.md`; `docs/LANCET.md`;
`crates/{lancet-audit,lancet-api,lancet-catalog,lancet-native,lattice-api,lattice-cli}/AGENTS.md`;
`AGENTS.md` (root); `FINDINGS.md`; `FIX.md`.

## ENV-OOM-02 third occurrence: `lancet-api` audit tests race `config`'s `XDG_STATE_HOME` test — 2026-07-27

Severity: test flakiness / gate-blocking.

**Problem:** The second `cargo xtask ci` run of the #28 sweep failed `cargo test -p lancet-api` (exit 101, 9 failures) where the first run passed. `config.rs`'s `audit_state_root_prefers_absolute_xdg_state_home` and `audit.rs`'s `IsolatedProjectAudit` each guarded `XDG_STATE_HOME` with their **own** mutex, which serializes nothing: the config test's restore path (`remove_var`, since the outer environment has no such variable) landed inside an audit test's read window, so `tail_audit` resolved the real `~/.local/state/lattice/…` path and panicked with "audit database not found". The panic then poisoned `XDG_TEST_LOCK`, and `expect("xdg test lock")` turned one failure into nine. Pre-existing: both tests and both locks are unchanged at base `538b476e`; #28 only altered the scheduling around them.

**Fix:** One process-wide `config::lock_xdg_state_home()` shared by both modules, recovering poison instead of propagating it (`unwrap_or_else(|p| p.into_inner())`) so a panicking test fails alone. Test-only change; no product behavior.

**Verification:**
- `cargo test -p lancet-api` — 20 passed.
- `cargo test -p lancet-api --lib` ×25 — 25/25 green (0 failures).
- `cargo xtask ci` — All checks passed.

**Changed files:** `crates/lancet-api/src/{config,audit}.rs`; `FINDINGS.md` (ENV-OOM-02 third occurrence); `FIX.md`.

**合并说明（#28 与 #111 分支合并时）:** 上方同日条目「两把互不相干的 mutex 守同一个 `XDG_STATE_HOME`」是 **#111** 对同一处缺陷的独立发现与独立修复。两侧修法语义相同（单锁 + poison 恢复），合并时保留 #111 那一份实现（doc 注释含失败率数据与「必须全 crate 唯一」的回归告警），删除本条引入的重复 `lock_xdg_state_home` 定义——否则同一 crate 内两个同名函数直接编译失败。两条记录均按 append-only 保留。

## GitHub #28: Cell capability admission, effect ledger, audit/provenance evidence — 2026-07-27

Severity: feature / fail-closed authorization / audit schema.

**Problem:** #27 landed the WASM Component substrate, but a Component Tool call had no authorization step of its own: nothing decided which declared capabilities an operator had approved, every Tool Host import ran unconditionally, the result Cell was checked only against the descriptor, and audit compressed a whole call into one `component_cells` key that mixed "why authorized" with "what happened". Design: `docs/specs/2026-07-26-issue28-cell-admission-design.md`.

**Fix (per crate):**
- `lancet-protocol`: `evidence` (admission evidence kept separate from the observed-effect ledger, plus `ObservedScope`/`ObservedTarget` and the fail-closed `CapabilityAdmissionPolicy`), `canonical::canonical_json` (one canonical-bytes writer), dotted capability-name grammar as the first check in `ToolDescriptor::validate`, additive `EffectErrorData` cell fields, v2 attestation predicate constant. No new error-code variants.
- `lattice-profile`: typed `[lancet.capabilities]` (`deny`, `max_boundary`, default `in`) that can never fall into `extensions`; `AgentProfile::capability_admission_policy` lowers it plus `[tools].enabled`.
- `lancet-runtime`: `admission.rs` — `admit_tool` with the fixed enabled → deny → boundary stage order, `authorize_operation` per Tool Host import, host-owned `resolve_scope` (undeclared trust = `ex`), `validate_policy_against_catalog`; `emit-event` bound to `Cell{Mutate, None, session-scope}` with a `Session{turn_id}` target; result Cell checked against descriptor ceiling *and* admitted set; host-computed `context_measure`; composition refuses one capability name over two cells.
- `lancet-audit`: `PRAGMA user_version` with actionable open errors for newer/old-shape databases (file untouched); `admission_evidence` / `observed_effects` / `denied_operations` recorded separately with `component_cells` derived from the ledger; turn-granular v1/v2 attestation selection with stored `predicate_type` winning on replay.
- `lancet-authority`/`lancet-native`/`lancet-api`/`lattice-api`: `CepPolicyConfig.capability_admission` threaded fail-closed from profile to transport; the Component branch admits before `rt.call`, answers `CapabilityNotGranted` with evidence on denial, and merges evidence at finalize; unknown deny names fail server construction.
- Legacy vocabulary is **not** deleted (design §11): the `transport.rs` dispatch split stays the single documented seam and Component Tools never consult the legacy types.

**Verification:**
- `cargo xtask ci` — All checks passed (fmt, clippy `-D warnings`, debug build, harness/lancet boundaries, `tool-codegen --check`, 18 crates × `cargo test -p`, `tool-build --verify`): 88 test binaries, 1242 tests, 0 failures.
- `cargo test -p xtask` — 43 passed (the gate excludes `xtask` from its per-crate test list).
- Design §10 scenario sweep: capability alias, partial admission, uncovered operation, default `ex` + `max_boundary=in`, scope escape, result-cell drift, effect ceiling, `Free` measure `None`, old-shape audit DB, audit attribution, attestation determinism, capability/cell conflict, multi-capability + multi-effect call, unknown profile capability, and the noop B11 e2e all have tests.
- Pre-existing, unrelated: `cargo check --workspace --all-features` fails on the `inspect-scaffold` scaffold test (also broken at base `538b476e`) — recorded as FINDINGS `L-38-01`.

**Changed files:** `crates/lancet-protocol/src/{evidence,canonical,cell,error,provenance,lib}.rs`; `crates/lattice-profile/src/{schema,load,lib}.rs`; `crates/lancet-runtime/src/{admission,composition,evidence,host,runtime,error_map,lib}.rs` + `tests/{call,gate,negative}.rs`; `crates/lancet-audit/src/{audit,audit_tests}.rs` + `src/provenance/{attestation,canonicalize,projector,mod}.rs`; `crates/lancet-authority/src/policy/{mod,default}.rs`; `crates/lancet-native/src/server/transport.rs` + `tests/{cep_component_round_trip,prov_isomorphism}.rs`; `crates/lancet-api/src/{attestation,audit,config,runtime}.rs`; `crates/lattice-api/src/turn/cep.rs` + `tests/{audit_turn_provenance,direct_local_acceptance}.rs`; `tools/noop/manifest.toml` + `generated/`; `docs/{CONFIGURATION,LANCET}.md`; crate `AGENTS.md` files; `FINDINGS.md`; `FIX.md`.

## GitHub #64: explicit Rewind that preserves external changes and detects divergence — 2026-07-27

Severity: feature / data safety.

**Problem:** #63 recorded every successfully installed Atomic Local Mutation into a private bare gix history and bound the resulting object id to each completed Turn, but nothing could reverse it. `session reset` moved only the Session History ref, so the conversation target and the Local Workspace drifted apart, and there was no way to undo LATTICE's file changes without a whole-workspace restore that would also destroy the user's own edits.

**Fix:** Rewind is layered so each crate owns exactly the half it can see, and the refs move last.

- `lancet-runtime` (`history.rs`): `plan_rewind` walks first-parent from `current` back to the target identity, parsing the mutation/baseline trailers, and yields the reversed `RewindStep`s newest-first plus one `RewindPathRestore` per affected path — `expected` is the recorded post-state at the tip, `restore` is the parent-tree state of the *oldest* reversed mutation of that path, so external drift a baseline captured mid-range comes back rather than LATTICE's older content. `rewind_with` runs plan → caller's apply closure → compare-and-set of `current` (deleting the ref for an empty target) under the same advisory `flock` that serializes recording, so a concurrent recorder cannot interleave. Record pins are never touched. A target that is not on the chain — including a tip abandoned by an earlier Rewind — is `TargetUnreachable`, never a silent full unwind.
- `lancet-native` (`workspace/rewind.rs`): `preflight_and_restore` is strictly two-phase. Phase 1 parses every affected path, rejects System Path Exclusions, resolves through the same symlink-rejecting secure walk mutations use, and compares on-disk bytes against the recorded post-state — reporting *every* divergent path as a structured conflict and writing nothing. Phase 2 runs only after all paths verified: parent-tree bytes install atomically (same-directory tempfile + fsync + rename) and a reversed `mutate.create` deletes its file. `install_atomic`/`remove_file` were split into shared synchronous cores so the restore reuses the production primitives instead of copies. The executor records no history: reversing a mutation must not append compensation commits.
- `lancet-api`: `rewind_workspace` composes the two, mapping a preflight conflict onto an apply-closure `Err` so the ref move is skipped, and carrying the applied report inside the ref-failure error so callers can still record what happened on disk. `record_rewind_audit`/`rewind_audit_records` follow the `query_audit` operator shape.
- `lancet-audit`: new `rewind_records` table + `RewindAuditRecord`. One durable row per attempt (success, conflict, restore failure, session-ref failure) with target Turn, both history identities, reversed steps, per-path preflight revisions, restored paths, and conflicts — paths, hashes, and object ids only, never file bytes. `init_schema` uses `IF NOT EXISTS`, so a pre-#64 database gains the table on first open.
- `lattice-api`: `rewind_to_turn` resolves the target Turn in the current session (`get_turn_commit` already refuses nonexistent and foreign commits; log membership refuses a target unreachable from HEAD), then the workspace half, then the Session History ref — `reset` on a branch, `checkout` when HEAD is detached — after pinning the abandoned head under `rewound/<oid>` so the rewound-away Turns keep an internal ref. It is a free function, not a `SessionApi` method: the session facade stays pointer-movement only.
- `lattice-cli`: `lattice session rewind <session> <turn>`, `--json` emitting the success or conflict report. Unresolvable/unreachable targets exit 2 (input error), a structured conflict exits 1 (runtime outcome), matching the pinned #23 contract.

**Adversarial-review corrections (same patch).** A three-lens adversarial design review over the eleven acceptance criteria found four issues worth acting on before the code landed:
- *A conflict must not move the ref.* The naive wiring — capture the conflict outcome and return `Ok(())` from the apply closure — would have compare-and-set `current` backwards while the workspace was untouched. The closure now returns `Err(Abort::Conflict)` and `lancet-api` unwraps it back into a normal `Conflict` outcome, with unit and acceptance tests asserting the ref is unmoved.
- *A `None` history identity is ambiguous.* The live turn path stamps `workspace_history_oid: None` both for a Turn from before any mutation and when the end-of-turn identity read failed (`live.rs`). Treating `None` as "unwind everything" would reverse mutations installed *before* the target. `rewind_to_turn` now honours a `None` target only when no ancestor Turn carries an identity either, and otherwise refuses with an explicit conflict (`a_turn_without_a_history_identity_is_refused_when_an_ancestor_has_one`).
- *Every workspace-touching failure needs an audit row.* The ref-move and restore failure paths originally returned before writing anything durable — exactly the partial states an operator most needs recorded. Both now write `history_ref_failure`/`restore_failure` rows carrying the restored paths, and the audit writer moved from `lattice-api` into `lancet-api` to match the `store_turn_attestation` precedent.
- *Kind divergence is a conflict, not an opaque error.* A path replaced by a directory or a symlink is reported as a structured conflict alongside the other divergent paths (nothing written, nothing followed) instead of aborting on the raw resolution error.

**Post-landing adversarial acceptance (same branch).** An eleven-agent pass over the acceptance criteria — one refuter per criterion — found one further real gap, fixed in `c1a26561`'s successor: a path whose *non-leaf* component the user had replaced with a regular file (`ENOTDIR`), or that could not be read at all, surfaced as `RewindRestoreError::Io` rather than a structured conflict. Safety held (nothing moved either way), but the user learned neither which path was at fault nor that the other affected paths were fine. `observe` now returns a state rather than a `Result`: anything that cannot be verified is a conflict entry with a precise detail, because the verify-before-write rule already forbids writing it. Deleting an already-absent path became idempotent in the same pass — after preflight, absence *is* the goal state.

*Round two* found three more:
- **A recorded path could be planned under the wrong name (correctness).** `parse_mutation_step` trimmed the `lattice-path` trailer, but a workspace-relative path may legitimately begin or end with a space: `mutate.create` with `./ a.txt` normalizes to ` a.txt` (`WorkspacePath::parse` trims the raw string, then `normalize_relative` drops the `./`). The plan then verified and restored `a.txt` — a path LATTICE may never have touched — while the real ` a.txt` stayed unverified and unrestored, and `current` moved anyway: a Rewind reporting success over a path it never touched. The trailer now round-trips byte-exactly (only the single separator space is stripped). Independently, `plan_rewind` now diffs the target tree against the tip tree and fails closed with `RewindError::PlanIncomplete` if any changed path has no planned restore, so no future mis-read of a mutation commit can silently under-restore.
- **A full unwind could reverse the session's own pre-target work (correctness, narrow).** The `None`-identity guard scanned the target's same-session ancestors, but the private history is per *project*: with a swallowed identity read, a target whose ancestors are all identity-less can coexist with a chain that already holds this session's mutations. `guard_full_unwind` now plans the reversal read-only and refuses when any step's `<session>:<parent>` turn id names a Turn of this session at or before the target.
- **A FIFO at an affected path hung the Rewind (availability).** `PathKind::File` means "neither directory nor symlink", which also covers FIFOs, sockets, and device nodes, so preflight called `fs::read` on them; on a FIFO `open(2)` blocks until a writer appears — forever, while holding the history's recording lock, wedging mutation recording along with it. `observe` now confirms a regular file before opening and reports anything else as a conflict.
- **A partial restore was not recorded (audit completeness).** Phase 2 writes one path at a time, so an I/O failure part-way leaves earlier paths restored — the most dangerous state — yet the `restore_failure` row carried no restored paths. `RewindRestoreError::Io` now carries exactly the paths already written, threaded through to the durable record and into the error message.
- **"Ignored content" was not demonstrably ignored (test honesty).** The Layer-3 scenario now asks the project's own Git: `check-ignore` must match *and* `ls-files` must be empty, so the surviving file is genuinely untracked ignored content rather than a second unrelated file.
- **The executor renamed the path the plan had fixed (correctness).** The byte-exact trailer fix was defeated one layer down: `preflight_and_restore` re-parsed each recorded path with `WorkspacePath::parse`, which trims, so ` a.txt` was verified and written as `a.txt` — touching a file LATTICE may never have mutated while leaving the real one unverified, and both refs moved on a success report. A recorded path that does not round-trip through workspace path resolution is now refused outright.
- **The full-unwind guard was same-session only (correctness, narrow).** The private history is per project, so a target with no identity can coexist with pre-target mutations another session installed, which turn ids cannot order. `RewindStep` now carries `recorded_at` from the commit, and `guard_full_unwind` refuses when any step was recorded at or before the target Turn was committed — both timestamps written by the same machine — with an unreadable timestamp on either side counting as ambiguity and therefore a refusal.
- **The full-unwind guard failed open and over-refused (correctness + availability).** A planning error skipped the guard entirely (`Err(_) => Ok(())`), so a transient repository read failure that succeeded on the rewind's own second plan let an unguarded full unwind through; it now refuses. And comparing whole-second timestamps refused *legitimate* full unwinds whenever the next Turn's first mutation was recorded in the same wall-clock second as the target Turn's commit — reproduced by the positive test below. Placement is now exact where it can be: a step installed by a Turn of this session is located in the session log directly, and recording time is only the fallback for another session's Turn or one this log cannot place.
- **Coverage shortfalls (no functional defect).** "Session reopen" was nominal — every handle in the file was already fresh — and is now a genuine reopen through `open_session` plus prefix resolution on a store handle that never saw the session. The detached-HEAD ref-failure arm, the restore-failure audit row, and the branch case's `status`/`log` assertions were untested and now are.

**Verification:**
- `cargo fmt --all --check` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo xtask harness-boundary` / `lancet-boundary` / `tool-codegen --check` — passed.
- `cargo test -p lancet-runtime --lib history::` — 29 passed (16 new: plan ordering, earliest-parent restore, create reversal, baselined external drift, empty plan, full unwind + ref delete, unreachable and post-rewind-abandoned targets, apply abort, ref-move sabotage, pin survival, post-rewind recording, disabled history, whitespace path round-trip, plan-coverage fail-closed, create-after-external-delete reversal).
- `cargo test -p lancet-native --lib workspace::rewind` — 11 passed (atomic restore, all-paths conflict with no writes, directory/symlink kind divergence, expected-absence conflict, System Path Exclusion and traversal rejection, restore I/O failure).
- `cargo test -p lancet-audit` — 38 passed (2 new: durable rewind records across reopen, pre-#64 schema compatibility).
- `cargo test -p lattice-api --test rewind_acceptance` — 17 passed (runtime layer: selective reversal with external create/edit/delete and ignored content preserved, same-path conflict with nothing moved, no compensation commits + pins reachable + project `.git` untouched, kind divergence, off-chain target; turn layer: conversation target and workspace agreeing after success with the durable audit row, conflict moving neither, unresolvable/foreign/unreachable targets, reopen + branch + detached HEAD, session-ref failure not reported as success, ambiguous identity refused, cross-session in-range reversal, three full-unwind guard cases — refused for this session's earlier work, refused for another session's, allowed for a Turn that really predates every mutation).
- `LATTICE_BIN=target/debug/lattice cargo test -p lattice-system-harness` — 138 passed, including the new Layer-3 `direct_rewind` (through the built binary: three turns, external changes, `session rewind` restores the edit and removes the create while everything external survives, session log lands on the target Turn, project refs unmoved, pins survive `git gc --prune=now`; and a conflict scenario exiting 1 with a structured report while nothing moves).
- Per-crate: `lancet-runtime` 67, `lancet-native` 153, `lancet-audit` 38, `lancet-api` 18, `lattice-session` 54, `lattice-cli` 114, `lattice-api` 166 — all green.

**Changed files:**
`crates/lancet-runtime/src/{history.rs,lib.rs}`; `crates/lancet-native/src/{lib.rs,workspace/mod.rs,workspace/local.rs,workspace/rewind.rs}`; `crates/lancet-audit/src/{audit.rs,audit_tests.rs}`; `crates/lancet-api/src/{lib.rs,rewind.rs}`; `crates/lattice-api/src/{lib.rs,rewind.rs}` + `tests/rewind_acceptance.rs`; `crates/lattice-cli/src/commands/session.rs`; `crates/lattice-system-harness/{Cargo.toml,tests/direct_rewind.rs}`; root `AGENTS.md` + per-crate `AGENTS.md` (`lancet-runtime`, `lancet-native`, `lancet-audit`, `lancet-api`, `lattice-api`, `lattice-cli`, `lattice-system-harness`); `FINDINGS.md` (M-REWIND-01 → RESOLVED, new `L-64-01`); `Cargo.lock`; `FIX.md`.

## GitHub #63: persist successful Atomic Local Mutations to a private gix history — 2026-07-26

Severity: feature / durability.

**Problem:** A successfully installed Atomic Local Mutation left no durable record outside the Local Workspace. The Session History records Turn outcomes but never the workspace state a Turn arrived at, so an explicit Rewind (#64) had nothing to restore from, and a mutation that survived a later Tool/model/Turn failure was invisible after the fact. AGENTS.md already reserved this ground for `lancet-runtime` ("private gix history"), but nothing implemented it.

**Fix:**
- New `lancet-runtime/src/history.rs`: `WorkspaceHistory` over a private **bare** `gix` repository living outside the Local Workspace and outside the user's project `.git` (no Git subprocess, no `git2`). Blobs/trees/commits express before/after state for exactly the paths LATTICE has mutated — not a projection, checkpoint, or whole-project snapshot. `refs/lattice/workspace-history/current` tracks the newest recorded mutation; `refs/lattice/workspace-history/records/<oid>` pins every recorded commit so prior object ids stay reachable with no time-based expiry. A first `mutate.edit` on a path LATTICE has not touched writes a preceding `baseline` commit so the mutation commit's parent tree really is the before state. Recording is serialized, so multiple mutations in one Turn keep real execution order and parent relationships.
- `lancet-native`: `LocalWorkspace::with_history` attaches the run's history to the one concrete host module; `record_mutation` appends. `mutate.create`/`mutate.edit` record only **after** the atomic install returned success, so Mutation Precondition failures, `ReplanRequired`, Guard/policy/effect denials, and pre-install errors record nothing and never move the current ref. Recorded commit ids land in durable audit metadata under `workspace_history`; a history failure after a successful install is recorded as evidence and never triggers a compensating write.
- `lancet-api`: `LancetRuntimeConfig.workspace_history` + `WorkspaceHistoryBacking`; `project_state_dir{,_in}` and `workspace_history_backing{,_in}` resolve `<state>/lattice/projects/<key>/workspace-history.git` next to the CEP audit database. `LancetRuntime::workspace_history()` exposes the handle.
- `lattice-api`: the live turn path reads `WorkspaceHistory::current` after the server drains and passes it into `build_commit_input`, which now requires the value explicitly. A Turn with no mutation records the inherited current identity. `SessionApi::get_turn_commit` resolves a Turn's history object id by commit id.
- `lattice-session`: `workspace_history_oid` on `TurnCommitInput`/`TurnCommit`, persisted in the git session manifest (`skip_serializing_if`, so history-less turns produce byte-identical manifests). Manifest-only — no canonical seed or turn-id derivation changed, so no existing commit id moves.

**Adversarial-review corrections (same patch).** A 12-agent adversarial pass over the ten acceptance criteria found two real defects, both fixed and covered before landing:
- *Before-state loss on workspace drift.* The baseline decision tested only whether the path was **present** in the parent tree. When the Local Workspace drifted between two LATTICE mutations of one path (the user edits the file in their editor, a `git checkout` swaps branch contents, an external delete precedes a re-create), the mutation commit's parent tree held LATTICE's stale after-state and the genuine pre-mutation bytes were dropped even though the handler had supplied them — a Rewind built on that history would have overwritten the user's own edit or resurrected a deleted file. The recorder now compares the parent tree's **bytes** against the before state the installer observed and writes a baseline whenever they differ, including removing the path when the real before state was absence.
- *Lost mutations under concurrent recorders.* Two LATTICE runtimes sharing one project (two processes, or two runtimes in one process) each hold their own handle and their own mutex; a compare-and-set alone dropped roughly a tenth of successfully installed mutations from the recorded chain, reported only as `workspace_history.recorded=false` audit metadata. Recording now also takes an advisory `flock` on a lock file inside the private repository (kernel-released on process death, so a crash cannot wedge it), and a losing compare-and-set rebuilds on the winner's tip instead of failing. Proven by a 4-handle × 12-mutation contention test, stable over repeated runs.
- Also from the review: `open_or_init_bare` now tolerates a concurrent first-open race instead of aborting the turn; a history-recording failure after a successful install is logged via `tracing::error!` as well as recorded in audit metadata; and the `.git`-unchanged test now asserts that recording actually happened, so it can no longer pass with the feature switched off.

**Verification:**
- `cargo fmt --all --check` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo xtask harness-boundary` / `lancet-boundary` / `tool-codegen --check` — passed.
- `cargo test -p lancet-runtime --lib history::` — 13 passed (adds external drift, create-after-external-delete, single-commit common case, 4-handle concurrency).
- `cargo test -p lattice-api --test workspace_history_acceptance` — 13 passed (create, edit, first edit of a pre-existing file, external drift, multi-mutation Turn, no-mutation Turn, pre-install failure at both the runtime and Turn layer, post-install Turn failure, session reopen, branch movement, object reachability, project `.git` unchanged, no new Agent-visible method).
- `LATTICE_BIN=target/debug/lattice cargo test -p lattice-system-harness` — 55+ scenarios passed, including the new Layer-3 `direct_workspace_history` (a real turn through the built binary records into the private history under an isolated HOME; a pre-install failure records nothing).
- Per-crate (the `cargo xtask ci` shape): all 19 crates green — `lancet-runtime` 51, `lancet-native` 142, `lancet-api` 18, `lattice-session` 54, `lattice-api` 149, plus the untouched crates; 1231 tests, 0 failures.
- Two pre-existing test-isolation races were observed while verifying under `cargo test --workspace` on a loaded dev machine and are recorded as `FINDINGS.md` `L-63-01`; both files are untouched by this change and neither reproduces per crate.

**Changed files:**
`crates/lancet-runtime/{Cargo.toml,src/lib.rs,src/history.rs}`; `crates/lancet-native/src/{workspace/local.rs,server/transport.rs,handlers/mutation.rs}`; `crates/lancet-api/src/{config.rs,runtime.rs,lib.rs}`; `crates/lattice-api/src/{session.rs,turn/cep.rs,turn/live.rs,turn/execute.rs}` + `tests/workspace_history_acceptance.rs`; `crates/lattice-session/src/{graph.rs,git_graph/helpers.rs,git_graph/commit.rs}`; `crates/lattice-system-harness/tests/direct_workspace_history.rs`; root `AGENTS.md` + per-crate `AGENTS.md`; `CONTEXT.md` (Workspace History / Workspace-History Object ID); `docs/adr/0003-...md` (ownership amendment note); `FINDINGS.md` (M-REWIND-01 → PARTIAL + residuals); `Cargo.lock`; `FIX.md`.

## GitHub #46 residual finalization: fetch capability pre-outbound + scenario tighten — 2026-07-24

Severity: test refactor / product fail-closed / review closeout.

**Problem:** Independent reviews of the #46 residual elevate closeout found `direct_tools_fetch_capability_denial_with_public_https` was not a faithful FetchUrl-hosts denial: with `allow_public_https=true` + empty `allowed_hosts`, URL validation still allowed public HTTPS, `fetch.web` performed ambient outbound I/O first, and post-dispatch effect checking ran only after a successful handler — so `tool_failed` also passed on DNS/HTTP failure. Comments claimed pre-network denial. The sonnet routed-provider scenario under-asserted the credential failure half it relied on.

**Fix:**
- In `lancet-native` `fetch.web`, Enforce mode now checks the authorized FetchUrl host region after URL validation and before outbound HTTP; empty/mismatched hosts return `EffectViolation` with authorized-region language and no backend effect.
- Strengthened `direct_tools_fetch_capability_denial_with_public_https` to require effect/authorized-region tool error text, restore `turn_committed(status=completed)`, and correct comments.
- Tightened `direct_turn_sonnet_routed_provider_event` to require credential-class terminal, non-completed turn, and Failed exit alongside `model_started` anthropic/sonnet.
- Added native unit proof `native_web_fetch_empty_hosts_denies_before_outbound_in_enforce`.

**Verification:**
- `cargo +1.96.0 fmt --all --check` — passed.
- `cargo +1.96.0 clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo +1.96.0 xtask harness-boundary` / `lancet-boundary` — passed.
- `native_web_fetch_empty_hosts_denies_before_outbound_in_enforce` — passed (~0.00s).
- Focused residual harness scenarios (fetch/sonnet/stale-parent) — passed; fetch ~0.33s (no ambient egress).
- `cargo +1.96.0 test -p lattice-api --test usage_turn` / `--lib turn::live_tests` — passed.
- `LATTICE_BIN=... cargo +1.96.0 test -p lattice-system-harness --test 'direct_*'` — 39 scenarios passed.
- Ledger recount: 1449 / keep 1193 / elevate 42 / delete 214; no `planned_direct_*` replacements.
- `cargo +1.96.0 xtask ci` — All checks passed.
- `lens_diagnostics mode=all` — not available in this workspace; compensated via fmt/clippy/boundaries/focused+harness/`xtask ci`.

**Changed files:**
`crates/lancet-native/src/handlers/external.rs`; `crates/lattice-system-harness/tests/{direct_tools,direct_turn_contract}.rs`; `docs/TEST-TRIAGE.md`; `docs/test-triage-decisions.json`; `FIX.md`.

## GitHub #46 residual: land 3 elevate Direct-Local scenarios — 2026-07-24

Severity: test refactor / ledger closeout.

**Problem:** After #72, three Direct-Local handoff identities remained `elevate` with planned replacements: stale-parent `CommitFailed`, default `sonnet`→Anthropic `model_started`, and `allow_public_https` + empty FetchUrl hosts denial with CEP labels.

**Fix:**
- Added exact external scenarios: `direct_tools_fetch_capability_denial_with_public_https`, `direct_turn_sonnet_routed_provider_event`, `direct_failure_stale_parent_commit_failed`.
- Deleted superseded internals only after green supersets: `public_https_policy_requires_fetch_capability`, `usage_stream_turn_model_started_includes_routed_provider`, `stream_turn_live_with_agent_commit_failure_deterministic` (+ unused `BlockingLlmClient` / CEP network helper).
- Synced machine/prose ledger: elevate→delete for the three identities; totals keep 1193 / elevate 42 / delete 214 (1449 unchanged). Handoff partition now keep 14 / elevate 0 / delete 30.

**Verification:**
- `cargo +1.96.0 xtask harness-boundary` — passed.
- Focused harness scenarios for the three new names — passed.
- `cargo +1.96.0 test -p lattice-api --test usage_turn` and `--lib turn::live_tests` — run after deletes.
- Ledger invariant: 1449 identities; no remaining `planned_direct_*` replacements.

**Changed files:**
`crates/lattice-system-harness/tests/{direct_tools,direct_turn_contract,direct_failures}.rs`; `crates/lattice-api/tests/usage_turn.rs`; `crates/lattice-api/src/turn/live_tests.rs`; `docs/TEST-TRIAGE.md`; `docs/test-triage-decisions.json`; `FINDINGS.md`; `FIX.md`.

## GitHub #66: translate M-TOOL-01 residual to GitHub issue numbers — 2026-07-24

Severity: docs / review-record correctness.

**Problem:** After the Codeberg→GitHub migration, `M-TOOL-01` still listed REMAINING/Plan as Codeberg `#6/#7–#10/#12–#14`. Those bare numbers collide with unrelated MERGED/CLOSED GitHub issues and fail #66's residual consistency criterion (must be `#28/#29–#32/#34–#36`). The ledger note says new references use GitHub numbers.

**Fix:**
- Updated `FINDINGS.md` `M-TOOL-01` REMAINING/Plan to GitHub `#28` (capability admission), `#29–#32` (Tool migration), `#34` (delete migration crates), `#35` (Bundle), `#36` (docs).
- Translated the mutation dependency `#42` → `#64` (explicit Rewind) in the same Plan line.
- Left historical Codeberg-numbered findings elsewhere untouched; did not rewrite append-only FIX history.

**Verification:**
- `rg -n "slices A|slice D|REMAINING|Execute slice D" FINDINGS.md FIX.md` — M-TOOL-01 uses past-tense A–D landed; no active "Execute slice D".
- `rg -n "REMAINING: #6|#7–#10|#12 \(delete|Execute #6" FINDINGS.md` — zero hits in M-TOOL-01.
- `gh issue view` for `#28/#29–#32/#34–#36/#64` — all OPEN with matching ADR 0004 / Rewind titles.

**Changed files:**
`FINDINGS.md`; `FIX.md`.

## GitHub #72: reconcile #45→#46 assertion-level test handoff — 2026-07-24

Severity: test refactor / ledger correctness.

**Problem:** The #46 completion record described the 44-row Direct-Local handoff as both 20 strict-superset deletions and 20 half-retained rows. The machine ledger still had 20 `planned_direct_*` residuals without final assertion-level dispositions.

**Fix:**
- Re-audited all 44 handoff identities. Final disposition is **keep 14 / elevate 3 / delete 27**; the newly reviewed residual 20 split is **keep 7 / elevate 3 / delete 10**.
- Strengthened existing public-binary scenarios with exact durable log/error/cancellation assertions, CEP lifecycle metadata/order, seeded LANCET status, requested replay turn/activity plus stored-attestation match, unknown-turn diagnostic, and human session-log rendering.
- Deleted 10 newly superseded internal tests only after their exact external scenarios passed. Reclassified API return, raw graph/transcript, and audit-store/projection seams as `keep/local_invariant`.
- Retained only `planned_direct_commit_failure`, `planned_direct_routed_provider_event`, and `planned_direct_fetch_capability_denial`, each with explicit covered/uncovered halves and an owner.
- Preserved the 1449-row identity snapshot and synchronized machine/prose totals to keep 1193 / elevate 45 / delete 211 and local 1372 / fast-binary 45 / Direct-Local 32. Corrected `FINDINGS.md` T-23-01.

**Verification:**
- `cargo +1.96.0 fmt --all -- --check` — passed.
- `cargo +1.96.0 test -p lattice-agent --features test-utils`; `cargo +1.96.0 test -p lattice-api`; `cargo +1.96.0 test -p lattice-cli` — passed.
- `LATTICE_BIN=target/debug/lattice cargo +1.96.0 test -p lattice-system-harness --test direct_turn_contract --test direct_failures --test direct_operator --test direct_tools -- --test-threads=1` — 18 passed.
- `cargo +1.96.0 clippy --workspace --all-targets -- -D warnings` — passed.
- `LATTICE_BIN=target/debug/lattice cargo +1.96.0 test --workspace --all-targets -- --test-threads=1` — full workspace passed.
- Ledger invariant check — 1449 unique identities, expected decision/layer totals, and exactly three remaining `planned_direct_*` IDs.

**Changed files:**
`crates/lattice-agent/tests/agent_loop.rs`; `crates/lattice-api/src/turn/{live_tests,stream_tests}.rs`; `crates/lattice-cli/src/commands/{audit,lancet,run_tests}.rs`; `crates/lattice-cli/tests/release_smoke.rs`; `crates/lattice-system-harness/tests/{direct_failures,direct_operator,direct_tools,direct_turn_contract}.rs`; `docs/TEST-TRIAGE.md`; `docs/test-triage-decisions.json`; `FINDINGS.md`; `FIX.md`.

## Codeberg #24 slice 7: public session graph isolation — 2026-07-22

Severity: test refactor (测试重构 5/6, slice 7 of #46). Plan: `docs/plans/2026-07-22-issue46-slice7-session-isolation.md`.

**Problem:** Session graph isolation (branch/checkout/reset/diff scoped to the addressed session) had no external-binary coverage; the five internal CLI tests used the internal API.

**Fix:**
- New `tests/direct_session_isolation.rs` (5 scenarios): two sessions seeded with real turns, then branch-listing/checkout/branch-create/reset/diff isolation asserted via public `--json` commands only.
- Deleted the five internal `d25_scoped_session` tests (strict superset).

**Verification:**
- `LATTICE_BIN=... cargo +1.96.0 test -p lattice-system-harness --test direct_session_isolation -- --test-threads=1` — 5 passed.
- `cargo +1.96.0 test -p lattice-cli` — 117→112 (5 internals removed), all green.
- clippy + harness-boundary — green.

**Changed files:**
`crates/lattice-system-harness/tests/direct_session_isolation.rs` (new), `crates/lattice-cli/src/commands/session.rs`, `docs/TEST-TRIAGE.md`, `docs/test-triage-decisions.json`, `FIX.md`.

## Codeberg #24 slice 6: operator audit paths + secret discipline + turn_id — 2026-07-22

Severity: test refactor + additive event field (测试重构 5/6, slice 6 of #46). Plan: `docs/plans/2026-07-22-issue46-slice6-operator-audit-secrets.md`.

**Problem:** Public audit/session/lancet operator paths had no external-binary cross-validation; audit replay needed a stable public turn identity; secret-sentinel discipline had no black-box coverage.

**Fix:**
- Additive `turn_id` on `TurnEvent::TurnStarted` (audit replay needs it); harness parser + fixtures updated.
- New `tests/direct_operator.rs` (3 scenarios): audit query/replay/tail + lancet status + session status/log cross-validation after success+denial turns; unknown-session empty query; unknown-turn replay nonzero.
- New `tests/direct_secrets.rs` (2 scenarios): provider-credential + denied-content sentinels never leak into any public sink; failure bundle recursively scanned/redacted.
- Deleted 4 simple `Ok`-assertion audit CLI internals (superseded by JSON-fragment assertions); kept complex attestation-match/lancet-status internals.

**Verification:**
- `cargo +1.96.0 test -p lattice-api` / `-p lattice-cli` — green (4 audit internals removed, 121→117).
- `LATTICE_BIN=... cargo +1.96.0 test -p lattice-system-harness --test direct_operator --test direct_secrets` — 5 passed.
- clippy + harness-boundary — green.

**Changed files:**
`crates/lattice-api/src/turn/{types,live,types_tests,collector}.rs`, `crates/lattice-cli/src/commands/{audit,run_tests,lancet}.rs`, `crates/lattice-system-harness/src/session_send.rs`, `tests/direct_operator.rs` (new), `tests/direct_secrets.rs` (new), `docs/TEST-TRIAGE.md`, `docs/test-triage-decisions.json`, `FIX.md`.

## Codeberg #24 slice 5: provider failures, SIGINT cancellation, deadline — 2026-07-22

Severity: test refactor + product controls (测试重构 5/6, slice 5 of #46). Plan: `docs/plans/2026-07-22-issue46-slice5-failures-cancel-deadline.md`.

**Problem:** `session send` had no public cooperative cancellation or per-turn deadline; the agent loop only checked the cancel flag after each provider event (a stalled provider would not observe SIGINT); provider failure/disconnect/malformed-SSE/deadline had no external-binary coverage.

**Fix:**
- Profile: additive `[llm] timeout_secs` (Option<u64>, validated nonzero & <= 3600) in `lattice-profile`.
- API mapping: `timeout_secs` → `AgentTurnRequest.timeout` (`Duration`) in `lattice-api/turn/mapping.rs`.
- Agent loop: new `await_with_deadline_and_cancel` races `stream_chat()` and `stream.next()` against *both* the deadline and the cancel flag (bounded 50ms poll); a stalled provider now observes SIGINT without a deadline. New `StallOutcome::{DeadlineElapsed, Cancelled}` split at both call sites. Focused unit test `cancel_during_stalled_stream_terminates_cancelled`.
- CLI: `run_inner` installs a scoped Tokio SIGINT listener that flips a shared `Arc<AtomicBool>` cancel flag, passed through `RunTurnRequest.cancel_flag`; the listener is aborted after the stream drains.
- Harness: `SseScript::HttpError` (non-2xx startup); new `tests/direct_failures.rs` (7 scenarios): http-error-no-retry, disconnect-before-output, disconnect-after-partial-token, malformed-sse, sigint-while-stalled, profile-deadline-while-stalled, mutation-survives-provider-failure.
- Ledger: 4 entries half-covered (event-chain half landed; durable log/transcript/terminal_reason half residual), all retained.

**Verification:**
- `cargo +1.96.0 test -p lattice-profile` — 85 passed.
- `cargo +1.96.0 test -p lattice-agent --features test-utils` — all passed (incl. new stall-cancel test).
- `cargo +1.96.0 test -p lattice-cli` — 128 passed.
- `LATTICE_BIN=... cargo +1.96.0 test -p lattice-system-harness --test direct_failures -- --test-threads=1` — 7 passed.
- clippy + harness-boundary — green.

**Changed files:**
`crates/lattice-profile/src/{schema,load}.rs`, `crates/lattice-api/src/turn/mapping.rs`, `crates/lattice-agent/src/agent_loop/mod.rs`, `crates/lattice-agent/tests/live_sink_and_cancel.rs`, `crates/lattice-cli/src/commands/run.rs`, `crates/lattice-system-harness/src/fake_llm.rs`, `crates/lattice-system-harness/tests/direct_failures.rs` (new), `docs/TEST-TRIAGE.md`, `docs/test-triage-decisions.json`, `FIX.md`.

## Codeberg #24 slice 4: fetch/todo/catalog/retired tools external coverage — 2026-07-22

Severity: test refactor (测试重构 5/6, slice 4 of #46). Plan: `docs/plans/2026-07-22-issue46-slice4-tools-and-retired-methods.md`.

**Problem:** Default five-Tool distribution and retired-method fail-closed behavior lacked external-binary coverage; fetch needed a localhost fixture separate from the fake LLM.

**Fix:**
- New harness `FixtureHttpServer` (`src/fixture_http.rs`): OS-assigned port, localhost URL, fixed status/body, captured method/path, bounded shutdown.
- New `tests/direct_tools.rs` (7 scenarios): fetch success+cap, fetch disabled (no fixture hit / no cep_call_started), fetch host-policy denial, todo_write success, todo validation reject, catalog wire-name exposure, retired methods fail-closed.
- Deleted internal `rejects_profile_disabled_web_fetch` (strict external supersets). Kept `public_https_policy_requires_fetch_capability` (capability half residual).

**Verification:**
- `cargo +1.96.0 test -p lattice-system-harness --lib fixture_http` — passed.
- `LATTICE_BIN=... cargo +1.96.0 test -p lattice-system-harness --test direct_tools` — 7 passed.
- `cargo +1.96.0 test -p lattice-api --test usage_turn` — 4 passed.
- clippy + harness-boundary — green.

**Changed files:**
`crates/lattice-system-harness/src/fixture_http.rs` (new), `src/lib.rs`, `tests/direct_tools.rs` (new), `crates/lattice-api/tests/usage_turn.rs`, `docs/TEST-TRIAGE.md`, `docs/test-triage-decisions.json`, `FIX.md`.

## Codeberg #24 slice 3: atomic mutation, effect denial, external concurrency — 2026-07-22

Severity: test refactor + product fix (测试重构 5/6, slice 3 of #46). Plan: `docs/plans/2026-07-22-issue46-slice3-mutation-concurrency-effects.md`.

**Problem:** Effect-kind denial only ran post-dispatch on observed effects, so a denied `mutate.create` could still change Local state before the check failed. Direct-Local mutation/concurrency/effect-denial coverage was also missing from the external binary harness.

**Fix:**
- Pre-dispatch effect-kind denial: `check_declared_effect_kinds_denied` in `lancet-authority` compares a method's static declared effect kinds with `denied_effect_kinds` before handler dispatch; `lancet-native` transport applies it and audits the denial without calling the backend. Post-dispatch observed-effect check remains defense in depth.
- System Path Exclusions (`.git`, `.lattice`) are always injected into mutation authorized-region write-deny prefixes (not profile-configurable).
- `SessionSendDriver::spawn` / `wait_spawned` for concurrent send races; `ScenarioWorld::set_env` is `&self` via interior mutability so multi-phase fake-LLM rewiring works while a driver is live.
- New `tests/direct_mutation.rs` (7 scenarios): create atomic + existing-target; edit hash/anchor; stale edit + external visibility; no rollback after provider disconnect; write/system/effect denials; concurrent commit race; tool activity before commit.

**Verification:**
- `cargo +1.96.0 test -p lancet-authority --lib effect::checker` — 11 passed.
- `cargo +1.96.0 test -p lancet-native denied_effect_kind_blocks` — passed.
- `LATTICE_BIN=... cargo +1.96.0 test -p lattice-system-harness --test direct_mutation -- --test-threads=1` — 7 passed.
- Related harness tests (direct_local_turn/observation/turn_contract) + clippy + harness-boundary — green.

**Changed files:**
`crates/lancet-authority/src/effect/checker.rs`, `crates/lancet-authority/src/policy/default.rs`, `crates/lancet-native/src/server/transport.rs`, `crates/lancet-native/src/server/native_registry.rs`, `crates/lattice-system-harness/src/{world,session_send,artifacts}.rs`, `crates/lattice-system-harness/tests/direct_mutation.rs` (new), plus unused-mut cleanups in existing harness tests.

## Codeberg #43: rewrite retired KVM-lane vocabulary in historical ADR/plans/spec — 2026-07-22

Severity: docs / vocabulary hygiene (#65 `ready-for-agent`). Plan: `docs/plans/2026-07-22-issue65-retire-kvm-lane-vocab-in-historical-docs.md`.

**Problem:** After #62 deleted the Upper/KVM lane and after #45 slice 3/4 cleared retired tokens from active code, machine ledger, and `docs/TEST-TRIAGE.md`, six historical documents still presented Microsandbox/MSB-era field values and physical assumptions as current decision context (ADR 0003, completed #62 plan, #45 parent + slices 3/4, #27 effect-model spec).

**Fix (docs-only):**
- ADR 0003 rewritten to Direct-Local vocabulary; Upper/KVM/MSB retained only as retirement/migration narrative (meta "no MSB daemon" allowed).
- #62 completed plan archived under `docs/archive/plans/` with Direct-Local archive header; `docs/archive/README.md` documents `plans/`; relative links retargeted (`../../plans/…`); overview/gix-only plans point at the archive path.
- #45 parent + slice 3/4 marked completed and rewritten in Direct-Local terms; slice 3 renamed to `…-slice3-retired-lane-vocabulary.md`; historical closeout scripts avoid forbidden active field literals.
- #27 effect-model spec aligned to ADR 0003/0004: `in`/`ex` = Tool Host domain / external world; network = host Tool Host + `net-guard` / `[lancet.network]`; Mode B (hosted backends) marked superseded; WASM Component is the current carriage. Soft-wrap ATX `#35` escaped.
- Removed all `⚠️ 过时词汇警告` banners from rewrite targets.

**Verification:**
- Targeted `rg -i 'microsandbox|msb_full_turn|planned_[a-z_]+_msb|LATTICE_HARNESS_MSB|real_microsandbox'` on ADR 0003, #45 rewritten plans, and #27 spec → empty.
- `rg '过时词汇警告'` on those targets → empty.
- Standards + Spec review of the working-tree #65 delta → clean (rename/archive links; Mode B superseded; network/Tool Host framing).

**Changed files:**
- `docs/adr/0003-lancet-operates-directly-on-local-workspace.md`
- `docs/archive/README.md`
- `docs/archive/plans/2026-07-17-issue62-remove-upper-msb-residue.md` (moved from `docs/plans/`)
- `docs/plans/2026-07-17-direct-local-overview.md`
- `docs/plans/2026-07-17-gix-only-cleanup.md`
- `docs/plans/2026-07-19-issue45-fast-residual-ledger-closeout.md`
- `docs/plans/2026-07-19-issue45-slice3-retired-lane-vocabulary.md` (renamed from `…-slice3-msb-vocabulary-retirement.md`)
- `docs/plans/2026-07-19-issue45-slice4-ledger-prose-consistency.md`
- `docs/specs/2026-07-01-issue27-effect-model-toolctx-design.md`
- `docs/plans/2026-07-22-issue65-retire-kvm-lane-vocab-in-historical-docs.md`

## Codeberg #24 slice 2: Direct-Local workspace observation visibility/bounds + ledger closeout - 2026-07-22

Severity: test refactor (测试重构 5/6, slice 2 of #46; `ready-for-agent`). Plan: `docs/plans/2026-07-22-issue46-slice2-observation-visibility.md`.

**Problem:** Slice 0/1 built the harness foundations and the completed-Turn contract. Slice 2 lands the `observe.workspace` visibility/bounds matrix over the built binary - tracked/untracked/ignored/included-ignored/read-denied/system-excluded/budget-exceeding paths across all four modes (read/list/grep/find) - and takes over the mapped internal tests whose assertions are a strict superset of the new external scenario.

**Fix:**
- `crates/lattice-system-harness/tests/direct_observation.rs` (new): one comprehensive Direct-Local scenario driving 10 `observe.workspace` calls (read_file/list_directory/grep/find_files) through the public `session send` path against the full visibility matrix, inspecting each tool result in the next captured provider request. Asserts canonical tool name, 10-call ok/fail pattern, tool+CEP activity precedes commit, completed commit, and per-mode visibility/bounds: tracked+untracked visible, ignored hidden, included-ignored visible, read-denied hidden, system-exclusion (`.lattice`) hidden, single-read budget denied, list/grep/find truncated to configured limits with no hidden/denied paths leaking.

Assertion-level superset review deleted 4 internal tests (elevate -> delete):
- `usage_run_turn_can_read_workspace_file_with_default_profile` (lattice-api): direct_observation.rs reads tracked.txt/notes.txt content through a `.lattice/agents/` profile (strict superset).
- `tool_executor_uses_workspace_root_not_profile_package_dir` (lattice-cli): direct_observation.rs reads project-root files with profile in `.lattice/agents/dl-obs/` (strict superset; content-match stronger than ok-only).
- `stream_turn_emits_tool_activity_before_committed` (lattice-api): direct_observation.rs asserts `tool_call_started < turn_committed` + all-10 `tool_call_finished` + completed commit; `tool_call_finished < committed` is an agent-loop structural invariant (commit is terminal).
- `stream_turn_emits_activity_before_commit` (lattice-api): slice-1 `direct_turn_contract.rs` asserts the full lifecycle subsequence `[turn_started, model_started, token_delta, model_finished, turn_committing, turn_committed]` (strict superset).

Kept 2 internal tests as half-covered (elevate, ledger reason updated):
- `full_turn_audit_rows_and_replay_fragment`: observation half covered; audit rows/replay fragment not queried by any landed harness.
- `stream_turn_emits_cep_lifecycle_events`: observation half covered (tool_call events); `CepCallStarted/Finished` `semantic_stage`/`guard_stage`/`impact_kind` not asserted by any landed harness.

Updated `docs/TEST-TRIAGE.md` (per-crate rows, count tables 79->75 elevate / 184->188 delete, scenario directory, 精确移交 table, slice 2 execution section) and `docs/test-triage-decisions.json` (4 elevate->delete, 2 reason updated).

**Verification:**
- `cargo +1.96.0 fmt --all --check` - clean.
- `cargo +1.96.0 test -p lattice-system-harness --lib` - 54 passed.
- `cargo +1.96.0 build -p lattice-cli --bin lattice` + `LATTICE_BIN=$(pwd)/target/debug/lattice cargo +1.96.0 test -p lattice-system-harness --test direct_observation` - 1 passed.
- `cargo +1.96.0 test -p lattice-api` - all targets green (stream_tests/consistency/usage_turn each -1 deleted test).
- `cargo +1.96.0 test -p lattice-cli` - 39 passed (run_tests -1 deleted).
- `cargo +1.96.0 clippy -p lattice-api -p lattice-cli -p lattice-system-harness --all-targets -- -D warnings` - clean.
- `cargo +1.96.0 xtask harness-boundary` - passed.

**Changed files:**
`crates/lattice-system-harness/tests/direct_observation.rs` (new), `crates/lattice-api/tests/usage_turn.rs`, `crates/lattice-cli/src/commands/run_tests.rs`, `crates/lattice-api/src/turn/stream_tests.rs`, `crates/lattice-api/tests/consistency.rs`, `docs/TEST-TRIAGE.md`, `docs/test-triage-decisions.json`, `FIX.md`.

## Codeberg #48: cooperative Component cancellation 统一映射为 `CallCancelled` — 2026-07-22

Severity: High (#70 review follow-up to #27; Spec review of `d659b067^...a628d0e2`, verified by two independent skeptics; tradeoff confirmed on the issue after pre-implementation verification). Plan: `docs/plans/2026-07-22-issue70-callcancelled-unified-mapping.md`.

**Problem:** Call-scoped cancellation produced divergent outcomes depending on which observation point fired. Epoch interruption mapped to `CallCancelled`, but a guest observing the same cancellation via `host-control.cancelled()` and returning `Err("cancelled")` was unconditionally mapped to `ExecutionFailed` — so identical call-scoped cancellation surfaced different CEP error codes by guest/epoch race. Two further gaps confirmed before implementation: (1) the audit finalizer's `Cancelled` arm matched the exact message `"call cancelled"`, whose only producer (`lancet_dispatch::call_cancelled()`) ran exclusively on the client side — the arm was dead code, so even correctly-mapped epoch cancellations were audited as `Error`; (2) the dispatch client's `select` returned a client-synthesized `call_cancelled()` (`ExecutionFailed`) as soon as the cancel watch fired, never surfacing the server-mapped structured error. `is_incomplete_dispatch` likewise matched `ExecutionFailed + message.contains("cancelled")`, so re-mapping cooperative cancellation without extending it would have regressed the partial-effects durable-metadata path.

**Fix:** cancellation state is now the sole classification authority end to end; no guest/client-controlled error string participates anywhere.
- `lancet-runtime`: shared `cancelled_error()` helper (also used by the epoch `AbortReason::Cancelled` arm); `call()` short-circuits pre-cancelled calls with `CallCancelled` after params validation, before entering Wasmtime; the guest logical-error path goes through `map_guest_logical_result()`, which maps any guest `Err` to `CallCancelled` when call-scoped cancellation is set and keeps `ExecutionFailed` otherwise.
- `lancet-native` transport: `finalize_audit_for_dispatch` records `AuditOutcome::Cancelled` on `CepErrorCode::CallCancelled` (dead message arm deleted); `is_incomplete_dispatch` matches the `CallCancelled` code only (string clause dropped).
- `lancet-dispatch`: after cancellation the client future awaits the server's structured response instead of synthesizing `call_cancelled()` — the server is the authority for the call outcome; `call_cancelled()` and its `lancet-dispatch`/`lancet-cep` re-exports deleted; `start_call_raw` promoted to `#[doc(hidden)] pub` so round-trip tests can hold `CallControl`. Drop-future semantics (`CancelGuard`) unchanged; Component-path responsiveness stays bounded by the epoch ticker + deadline, native handlers run to completion (decision D2 in the plan).

**Verification:**
- `cargo +1.96.0 test -p lancet-runtime` — 38 passed (new: `precancelled_call_returns_call_cancelled`, 3 `map_guest_logical_result` unit tests).
- `cargo +1.96.0 test -p lancet-dispatch` — 10 passed (new: `cancel_resolves_with_server_structured_error`, `drop_future_signals_cancellation_to_server`).
- `cargo +1.96.0 test -p lancet-native --test cep_component_round_trip` — 7 passed (rewritten `cancelled_component_call_returns_call_cancelled_and_audits_cancelled` asserting client `CallCancelled` + audit `cancelled` + server survival; new `dropped_component_call_audits_cancelled_and_server_survives`).
- `cargo +1.96.0 test -p lancet-native` / `-p lattice-api` — all pass (dispatch AGENTS.md transport-change requirement).
- `cargo +1.96.0 test --workspace` — 82 suites ok, zero failures.
- `cargo +1.96.0 clippy -p lancet-runtime -p lancet-dispatch -p lancet-native -p lancet-cep --all-targets -- -D warnings` — clean.
- `cargo +1.96.0 fmt --all -- --check` — clean (also fixed pre-existing fmt drift in `lancet-runtime/tests/negative.rs` from #47).

**Changed files:**
- `crates/lancet-runtime/src/error_map.rs` — `cancelled_error()` helper; epoch arm delegates; module doc.
- `crates/lancet-runtime/src/runtime.rs` — pre-cancel check; `map_guest_logical_result()`; call doc; 3 unit tests.
- `crates/lancet-runtime/tests/call.rs` — pre-cancel test renamed/re-asserted.
- `crates/lancet-runtime/tests/negative.rs` — fmt-only (pre-existing drift).
- `crates/lancet-dispatch/src/transport.rs` — cancel arm awaits server response; `start_call_raw` pub; docs; 2 transport tests.
- `crates/lancet-dispatch/src/{registry.rs,lib.rs}` — `call_cancelled()` deleted.
- `crates/lancet-cep/src/lib.rs` — re-export cleanup.
- `crates/lancet-native/src/server/transport.rs` — code-based `Cancelled` audit + `is_incomplete_dispatch`.
- `crates/lancet-native/tests/cep_component_round_trip.rs` — rewritten/new cancellation round-trip tests.
- `docs/plans/2026-07-22-issue70-callcancelled-unified-mapping.md` — implementation plan (D1–D5 decisions).

## Codeberg #46: composition gate rejects duplicate host-pinned expected identity — 2026-07-22

Severity: MEDIUM (#46 review follow-up to #27; Spec + Zoom-out review of `d659b067^...a628d0e2`, verified by two independent skeptics).

**Problem:** `ClosedCatalog::compose` collected `expected_identities` directly into a `BTreeMap<&str, &ExpectedToolIdentity>` via `.collect()`, so two host-pinned expected identities sharing a name silently used last-wins: the later pin overwrote the earlier descriptor/artifact/import pin while composition still succeeded. The existing `DuplicateName` check only covered inventory registrations, not the host-pinned expected set. Production `EXPECTED_COMPONENTS` is still empty, so the defect had not yet fired, but once #29–#32 migrate production Tools it would weaken closed-catalog approval semantics. `xtask tool-codegen` generates each manifest's expected-identity candidate independently and does not deduplicate across manifests, so uniqueness cannot be left to the generation flow.

**Fix:**
- `crates/lancet-runtime/src/composition.rs`: added a distinct `CompositionError::DuplicateExpectedName(String)` variant (`#[error("duplicate expected Tool identity: {0}")]`), separate from `DuplicateName` so callers and tests can distinguish registration-name duplicates from host-pinned-identity duplicates.
- `ClosedCatalog::compose` now builds the `expected` map with an explicit insert loop that fails closed with `DuplicateExpectedName` on the first repeated expected-identity name, before any registration is processed. The rest of the gate (registration duplicate, unapproved, digest/artifact/ABI/import/channel drift, missing names) is unchanged.
- Module doc updated to state the gate also rejects duplicate host-pinned expected identities, checked before registration processing.
- `crates/lancet-runtime/tests/gate.rs`: added `gate_rejects_duplicate_expected_name` — one valid registration plus two same-name expected identities with different descriptor digests, asserting `DuplicateExpectedName("noop.echo")` (no last-wins). Rewrote `gate_rejects_duplicate_name` to call `ClosedCatalog::compose` directly with two same-name registrations and a single expected identity, isolating the registration-duplicate check (the old form passed two same-name `SyntheticTool`s through the `compose` helper, which builds expected identities 1:1 and now correctly trips the expected-identity check first).

**Verification:**
- `cargo +1.96.0 test -p lancet-runtime --test gate` — 13 passed (incl. new `gate_rejects_duplicate_expected_name` and rewritten `gate_rejects_duplicate_name`).
- `cargo +1.96.0 clippy -p lancet-runtime --all-targets -- -D warnings` — clean.
- `cargo +1.96.0 test -p lancet-runtime` — gate 13 / call 8 / negative 6, all pass.
- `cargo +1.96.0 fmt -p lancet-runtime -- --check` — clean.

**Changed files:**
- `crates/lancet-runtime/src/composition.rs` — `DuplicateExpectedName` variant + duplicate-detecting expected collection + module doc.
- `crates/lancet-runtime/tests/gate.rs` — new `gate_rejects_duplicate_expected_name` test; rewritten `gate_rejects_duplicate_name`.

## Codeberg #45: close stale FINDINGS T-23-01/T-23-02 to the #46 Direct-Local residual — 2026-07-22

Severity: MEDIUM (#45 review follow-up to #45; spec review of `df62ec84...8d6926d9`,
verified by two independent skeptics).

**Problem:** The open `FINDINGS.md` entries `T-23-01` and `T-23-02` still
described completed work as pending. `T-23-01` claimed "55 条提升仍待替代场景
（54 current-MSB + 1 fast）" with MSB/KVM-lane reasoning and a `planned_lancet_cli`
fast residual; `T-23-02` claimed a fast residual of 1 `planned_lancet_cli`, 2 status
tests待删, 2 project-branch scenarios待补, and 15+1 current-MSB residual. Current
facts (landed 2026-07-19 in #45 slices 1–4, commits `dbb31b14` / `55128410` /
`ce8880c0` / `4c2c861d` / `9f238b79`, with `8d6926d9` closing the #45 review
findings and T-23-03): #45 fast residual is zero, status and project-branch
create/reopen are owned by external-binary scenarios with the internal tests
deleted, the MSB/KVM lane is retired (#62), and the only remaining work is 44
`planned_direct_*` assertion-level scenarios handed to #46. The stale open text
conflicted with the final machine ledger (`docs/test-triage-decisions.json`: 44
unique `planned_direct_*` IDs; distribution keep 1179 / elevate 86 / delete 184,
total 1449) and with `docs/TEST-TRIAGE.md` §1.5, which was already correct.

**Fix:**
- `FINDINGS.md` `T-23-01` (open MEDIUM): rewritten to "#46 Direct-Local
  assertion-level replacements 尚待落地". It now states fast residual is zero,
  names the landed takeovers (`fast_lancet_cli` / `fast_session_cli` + deleted
  internal tests), records the 11 retired-contract deletes, and describes only the
  44 `planned_direct_*` residual with the verified ledger counts. MSB/KVM is
  mentioned only as retired (#62), not as a current lane. The `T-23-01` ID is
  retained so cross-refs in `docs/TESTING.md` and `FIX.md` stay valid.
- `FINDINGS.md` `T-23-02`: removed from open MEDIUM and closed into the
  `OBSOLETE / CLOSED` section (dated 2026-07-19, placed beside `T-23-03`), noting
  fast/status/branch work is done by #45 slices 1–2 and the remaining Direct-Local
  work is handed to `T-23-01` / #46.
- No change to `docs/TEST-TRIAGE.md` or `docs/test-triage-decisions.json`: both
  were already consistent with the current口径.

**Verification:**
- `rg -n "T-23-01|T-23-02|planned_lancet_cli|current-MSB|KVM|待删" FINDINGS.md docs/TEST-TRIAGE.md`
  — open `T-23-01` uses only Direct-Local/#24口径; `T-23-02` is closed; no open
  finding describes fast/status/branch as pending or KVM/MSB as a current lane
  (`KVM`/`MSB` appear only in closed findings as historical context, or in the open
  `T-23-01` as retired/prohibited — never as a current lane).
- `python3` recount of `docs/test-triage-decisions.json`: 44 `planned_direct_*`
  rows (44 unique IDs); decision distribution keep 1179 / elevate 86 / delete 184.
- `FINDINGS.md` open MEDIUM now lists `T-23-01` (open, #46 residual) and no longer
  lists `T-23-02`; `T-23-02` appears once in `OBSOLETE / CLOSED`.

**Changed files:**
- `FINDINGS.md` — rewrote `T-23-01` (open MEDIUM) to the 44-row #46 residual;
  moved `T-23-02` from open MEDIUM to `OBSOLETE / CLOSED` as a 2026-07-19 closure.

## Codeberg #5 slice D: closed Component dispatch, descriptor-driven adapter, no-op CEP round-trip — 2026-07-22

Severity: FEATURE (#27 slice D of 4; docs/plans/2026-07-19-issue27-sliceD-cep-adapter-roundtrip.md).

**Problem:** ADR 0004 requires Component Tools to dispatch through the same
CEP server/audit lifecycle as the 5 production native handlers, with the
LLM adapter driven by generated descriptors rather than a hardcoded catalog
match gate. Slices A–C landed the Cell core, manifest/codegen/WIT/prelude/
no-op guest, and the Component engine + composition gate; this slice wires
them into the existing native dispatch path and deletes the adapter's
hardcoded five-arm typed dispatch.

**Fix:**
- `lancet-dispatch` `CepClient::call_named<P, R>`: name-dynamic, type-static
  transport helper. `call<M>` delegates to it. Serialize/deserialize failure
  maps to `invalid_params`/`internal_error`; no JSON Schema carried. #34
  absorbs this into `lancet-runtime`.
- `lancet-runtime`: `ComponentRuntime::call` changed from `&mut self` to
  `&self` (epoch ticker via `OnceLock`; `set_limits` stays `&mut` for tests).
  Added `contains(&self, name)` for the D.3 dispatch split.
  `CellEvidenceLedger` changed to `Arc<Mutex<Vec<…>>>`-backed + `Clone` so
  a pre-call clone retains records added during execution; added
  `snapshot_keys() -> Vec<String>` (4-segment `ctx:ext:scope:trust` keys).
- `lancet-audit` `merge_finalize_metadata`: extended with
  `component_cells: &[String]` parameter. Component calls record Cell audit
  keys as a `component_cells` JSON array; native calls pass `&[]` (no
  behavior change). Audit records cells, not capability names (spec §6.3).
- `lancet-native` + `lancet-api`: gained production dependency on
  `lancet-runtime`. `ServerRunConfig` carries
  `Option<Arc<ComponentRuntime>>`. `handle_request` branches Component vs
  native dispatch after audit start and before finalize, sharing one
  durable audit path. Component calls produce Cell evidence; native calls
  produce effect-ledger evidence as before. Production `from_embedded()`
  yields an empty Component catalog so all 5 native methods are unchanged.
- `lancet-api` `LancetRuntime::build`: composes
  `ComponentRuntime::from_embedded()` once; the same `Arc<ComponentRuntime>`
  threads into `ServerRunConfig.components` (D.3) and projects into LLM
  tool definitions (D.4). `LancetServer::spawn` composes from embedded
  inventory and delegates to `pub(crate) spawn_composed`; no public path
  to inject Component bytes/paths/handlers.
- `lattice-api` `CepExecutorAdapter`: `new` takes
  `definitions: &[ToolDefinition]` + `enabled_names`; the available set is
  driven by runtime definitions, not `catalog()`. Deleted the hardcoded
  five-arm `dispatch_typed`/`call_typed`/`schema_mismatch_for_method` +
  `extract_json_field_path` + five method type imports. All methods go
  through `CepClient::call_raw`; params/result validation is server-side.
- D.6 end-to-end tests (`crates/lancet-native/tests/cep_component_round_trip.rs`):
  `noop.echo` round-trips through CEP + audit (asserts
  `alloc:none:session:in` in metadata); native method coexists; Component
  events forwarded; server survives method-not-found and schema mismatch.
  `lattice-api` adapter tests: `CepExecutorAdapter` executes a Component
  Tool via descriptor-projected definition; rejects disabled Component Tool.

**Verification:**
- `cargo test -p lancet-dispatch -p lancet-runtime -p lancet-native -p lancet-api -p lattice-api` — all pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo xtask ci` — all checks pass (codegen determinism, build, boundary, test).
- Manual gate: `rg "dispatch_typed|schema_mismatch_for_method" crates/lattice-api/src/turn/cep.rs` — empty.

**Changed files:**
- `crates/lancet-dispatch/src/transport.rs` — `call_named` helper + tests.
- `crates/lancet-runtime/src/runtime.rs` — `&self` call, `OnceLock` ticker, `contains`, `Mutex` limits.
- `crates/lancet-runtime/src/evidence.rs` — `Arc<Mutex>`-backed ledger, `Clone`, `snapshot_keys`.
- `crates/lancet-runtime/tests/call.rs` — `contains` + `snapshot_keys` tests, `&self` call fixes.
- `crates/lancet-runtime/tests/negative.rs` — `&self` call fixes.
- `crates/lancet-audit/src/audit.rs` — `merge_finalize_metadata` + `component_cells`.
- `crates/lancet-audit/src/audit_tests.rs` — `component_cells` tests + existing test updated.
- `crates/lancet-native/Cargo.toml` — `lancet-runtime` dep + dev-dep with `test-utils`.
- `crates/lancet-native/src/server/transport.rs` — `ServerRunConfig.components`, D.3 dispatch split.
- `crates/lancet-native/tests/cep_component_round_trip.rs` — D.6 end-to-end tests (new).
- `crates/lancet-api/Cargo.toml` — `lancet-runtime` dep.
- `crates/lancet-api/src/runtime.rs` — `spawn_composed`, compose once, project defs.
- `crates/lattice-api/Cargo.toml` — `lancet-runtime` dev-dep with `test-utils`.
- `crates/lattice-api/src/turn/cep.rs` — D.5 adapter rewrite, delete hardcoded dispatch.
- `crates/lattice-api/src/turn/cep_tests.rs` — adapter signature update + D.6 Component adapter tests.
- `crates/lattice-api/tests/cep_round_trip.rs` — adapter signature update.
- `crates/*/AGENTS.md` — slice D notes for dispatch, runtime, native, api, lattice-api, audit.

## Codeberg #5 slice C: lancet-runtime Component engine + composition gate + minimal ToolHost — 2026-07-22

Severity: FEATURE (#27 slice C of 4; docs/plans/2026-07-19-issue27-sliceC-component-engine-composition-gate.md).

**Problem:** ADR 0004 requires a Component engine that executes embedded WASM
Tool Components with no ambient WASI, a fail-closed composition gate that
verifies every registration against a host-pinned identity at startup, and a
minimal ToolHost that provides control (cancellation + events) without granting
domain authority before #28 capability admission. Slice B landed the manifest,
codegen, WIT, prelude, and no-op guest; this slice builds the host runtime.

**Fix:**
- `lancet-protocol` Execution family: added `ToolTrap` (-32023),
  `ToolTimeout` (-32024), `ToolResourceExhausted` (-32025), `CallCancelled`
  (-32026) codes + `ToolExecutionErrorData` (method + category + optional
  resource; no guest backtrace/host path/secret) + `CepError::tool_execution`
  helper. Round-trip + leak-free tests added.
- `lancet-dispatch` `EventSink::emit_sync`: synchronous event emit for
  Component ToolHost imports that run inside synchronous wasmtime calls.
- New `crates/lancet-runtime`: wasmtime 46.0.1 (exact-pinned, no
  `wasmtime-wasi`/`wasi-common`), `inventory` 0.3.24, `jsonschema` 0.48.1,
  `wasmparser` 0.251. Sub-modules:
  - `composition.rs`: `EmbeddedTool` (name + bytes, `inventory::collect!`),
    `ExpectedToolIdentity` (host-pinned digests/ABI/imports), `ClosedCatalog`
    (immutable after `compose`), `CompositionError`. The gate verifies:
    unapproved name, duplicate, descriptor digest drift, artifact hash
    mismatch, descriptor JSON/validate/name drift, unsupported ABI, descriptor
    artifact drift, component compile, ambient WASI import, import drift,
    missing channel adapter, params/result schema, missing expected names.
  - `runtime.rs`: `ComponentRuntime` (engine + catalog + linker + limits +
    lazy epoch ticker), `ComponentCallContext` (call_id, turn_id, events,
    cancellation, evidence), `StoreState`, `call` method (params validation,
    instantiation, result CellRef parsing/validation, result schema
    validation, Cell evidence recording, error mapping).
  - `host.rs`: `host-control` Host impl (cancelled + emit_event via
    `emit_sync`).
  - `limits.rs`: `ExecutionLimits`, `RecordingLimits` (wraps `StoreLimits`,
    records which limit rejected), `AbortReason`.
  - `evidence.rs`: `CellEvidenceLedger` (in-memory `(Cell, Trust)` recorder).
  - `error_map.rs`: maps wasmtime errors to `CepError` codes (Cancelled →
    CallCancelled, Deadline → ToolTimeout, Memory/Instances →
    ToolResourceExhausted, fuel=0 → ToolResourceExhausted(fuel), else →
    ToolTrap).
  - `bindings.rs`: `wasmtime::component::bindgen!` for the Tool Host WIT.
  - `generated/`: production `EXPECTED_COMPONENTS` (empty until #29–#32) +
    `test-utils` no-op fixture from committed bytes.
  - `test_helpers.rs` (`test-utils`): synthetic registration/identity
    construction for gate negative-case tests.
- `lancet-runtime` added to workspace members + workspace deps. No
  `lancet-native`/`lancet-api`/LATTICE dependency; no cycle.
- Tests (26 total): 12 gate (missing, duplicate, unapproved, descriptor/artifact
  drift, import drift, unsupported ABI, ambient WASI, missing channel adapter,
  positive compose, immutable catalog), 6 call (round-trip, method-not-found,
  bad params, evidence, events, pre-cancelled), 6 negative (survival after
  method-not-found/schema-mismatch/guest-error, extra params, fuel exhaustion
  + survival, short deadline + survival), 2 unit (hex digest, sha256 verify).

**Verification:**
```
cargo test -p lancet-runtime -p lancet-protocol   # 73 tests, 0 failures
cargo clippy -p lancet-runtime --all-targets -- -D warnings   # clean
cargo tree -p lancet-runtime | grep -E "wasmtime-wasi|wasi-common"   # empty
```

**Changed files:**
- `Cargo.toml` (workspace members + deps: wasmtime, inventory, jsonschema, wasmparser)
- `crates/lancet-protocol/src/error.rs` (new codes + data + helper + tests)
- `crates/lancet-protocol/src/lib.rs` (re-export ToolExecutionErrorData)
- `crates/lancet-protocol/AGENTS.md` (slice C note)
- `crates/lancet-dispatch/src/registry.rs` (emit_sync)
- `crates/lancet-runtime/` (new crate: Cargo.toml, AGENTS.md, src/*.rs, tests/*.rs)

## Codeberg #5 slice B: manifest + deterministic tool-codegen + WIT + prelude + no-op guest — 2026-07-22

Severity: FEATURE (#27 slice B of 4; docs/plans/2026-07-19-issue27-sliceB-manifest-codegen-noop.md).

**Problem:** ADR 0004 makes each Source Tool manifest the sole Tool Descriptor
authority and requires deterministic build tooling to generate WIT bindings,
host descriptor, LLM projection, artifact hash, and `inventory` registration
from it, plus a `lattice-tool-prelude` guest SDK and a no-op tracer Tool whose
Component is committed and bit-for-bit reproducible. Slice A landed the Cell
vocabulary; this slice builds the manifest→codegen→Component substrate.

**Fix:**
- `rust-toolchain.toml` pins rustc 1.96.0 + `wasm32-unknown-unknown`; workspace
  + xtask `rust-version` bumped 1.88→1.94 (wasmtime 46 MSRV). `xtask ci` adds a
  'toolchain precheck' step asserting the pinned compiler + wasm target.
- `tools/noop/manifest.toml` is the sole Tool Descriptor authority for
  `noop.echo` (params/result JSON Schema, one `session/in` result scope, one
  `alloc:none:session` capability, artifact identity).
- `xtask tool-codegen` deterministically generates five artifacts under
  `tools/noop/generated/`: `tool-host.wit` (WIT world), `descriptor.json`
  (canonical host ToolDescriptor, pure JSON for `include_bytes!`+strict
  serde_json), `llm-projection.json`, `registration.rs` (name + include_bytes!
  only — no self-approval), `expected-identity.json` (real SHA-256 descriptor
  digest + artifact digest + expected imports; a CANDIDATE the host composition
  step alone may pin). Manifest parsing is strict (deny_unknown_fields); cells
  parse through `lancet_protocol::parse_cell_ref`; `ToolDescriptor::validate()`
  enforces scope/capability consistency.
- `lattice-tool-prelude` (new crate, `#![no_std]`) re-exports pinned
  `wit_bindgen` and provides `export_tool!` Component export glue; no host
  internals (`cargo tree -p lattice-tool-prelude` has no `lancet-*`).
- `noop-echo` guest crate (`wasm32-unknown-unknown`, `#![no_std]`) implements
  the tracer: probes cancellation, emits one progress event, echoes `message`
  as a trust-free `alloc:none:session` result CellRef. Its import surface is
  exactly `host-control` (no `wasi:*`).
- `xtask tool-build` builds the guest release (with `--remap-path-prefix`),
  encodes the Component (`wasm-tools component new`), validates it
  (`wasm-tools validate` + `component wit` no-`wasi:` assertion + `component
  targets` conformance), pins the sha256 back into the manifest, and re-runs
  codegen. `--verify` rebuilds into a temp path and compares digests
  (warn-only per decision 1).
- `xtask ci` adds `tool-codegen --check` (determinism drift gate) and
  `tool-build --verify` (warn-only) lanes.
- 12 new unit tests (codegen determinism, WIT surface, canonical descriptor
  round-trip, no-self-approval, manifest validation, build name mapping) plus
  4 guest extraction tests.

**Divergences from the plan (toolchain-authority driven):**
- WIT record fields use `,` separators (wasm-tools 1.254 / wit-bindgen 0.59
  grammar), not the plan's `;` sketch (older WIT grammar).
- `wit-bindgen` is a direct guest dep: its `generate!` emits code referencing
  `wit_bindgen::rt::...` by bare crate path, which a prelude re-export cannot
  satisfy. The prelude still pins the version and provides the export glue.
- `export_tool!` is invoked INSIDE `mod bindings` (not the crate root):
  wit-bindgen's generated `export!` resolves `self` at the call site, so the
  export must be wired where `generate!` ran.
- Toolchain fingerprint records `wit-bindgen-0.59` (installed latest), not the
  plan's 0.57.

**Verification:**
- `cargo fmt --all --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo test -p xtask -p lattice-tool-prelude -p noop-echo` — 41 passed / 0
  failed.
- `cargo xtask ci` — all lanes pass; `tool-codegen --check` drift-free;
  `tool-build --verify` reproduces the committed digest bit-for-bit
  (6fcc7fc8...) locally.
- `wasm-tools component targets <component> <wit>` — conforms;
  `wasm-tools component wit` — imports exactly `host-control`, no `wasi:*`.
- `cargo tree -p lattice-tool-prelude` — no `lancet-*` dependencies.

**Changed files:**
- `rust-toolchain.toml` (new), `Cargo.toml`, `xtask/Cargo.toml`
- `xtask/src/{main.rs,ci.rs,tool_codegen.rs,tool_build.rs}` (tool_codegen.rs,
  tool_build.rs new)
- `crates/lattice-tool-prelude/` (new crate)
- `tools/noop/{manifest.toml,Cargo.toml,src/lib.rs}` + `tools/noop/generated/`
  (tool-host.wit, descriptor.json, llm-projection.json, registration.rs,
  expected-identity.json, noop_echo.component.wasm)
- `FINDINGS.md`, `FIX.md`, crate `AGENTS.md` files

---


Severity: FEATURE (#27 slice A of 4; docs/plans/2026-07-19-issue27-sliceA-protocol-cell-core.md).

**Problem:** ADR 0004 needs a closed Cell effect vocabulary in the protocol
contract layer before the manifest/codegen/Component-engine/round-trip slices
(B/C/D) can build on it. The legacy `ImpactKind` machinery in
`effect_types.rs` stays in production until #28/#34, so the new vocabulary
lands add-only alongside it.

**Fix:**
- New `crates/lancet-protocol/src/cell.rs`: closed enums `CtxOp` {Alloc,Free,Mutate}, `ExtImpact` {None,Read,Write}, `Trust` {In,Ex} (Ex default, fail-closed), `ChannelKind` {Workspace,Network,Session}; structs `ScopeDecl`, `Cell`, `Capability`, `ToolDescriptor` (+`ArtifactIdentity`, `abi_version`), `ToolOutput`.
- Strict CellRef/audit-key layering: `Cell::reference()` emits a trust-free 3-segment `ctx:ext:scope`; only the host appends `:in|ex` via `audit_key(trust)` after descriptor scope lookup; `parse_cell_ref` rejects a guest-supplied 4-segment trust suffix as `SegmentCount(4)` (guests cannot self-claim `in`).
- `ToolDescriptor::validate()` registration-time consistency (duplicate scope → duplicate capability → undeclared scope); `required_channels()` derived only from `scopes` (no independent channels field, spec §11.3); `trust_of(undeclared) == Trust::Ex` (fail-closed).
- 8 inline tests covering the plan's test table (default-trust, undeclared scope, cartesian CellRef round-trip, guest-trust rejection, host-derived audit trust, undeclared-scope/duplicate-name rejection, multi-cell `session.todo_write` representation per spec §5.1).
- `lib.rs` gains a single `pub mod cell;` (+ module-doc bullet); `effect_types.rs` and existing re-exports untouched (add-only).

**Verification:**
- `cargo fmt --all --check` — clean.
- `cargo test -p lancet-protocol` — 44 passed / 0 failed (8 new `cell::tests`).
- `cargo clippy -p lancet-protocol --all-targets -- -D warnings` — clean.

**Changed files:**
- `crates/lancet-protocol/src/cell.rs` (new), `src/lib.rs`
- `crates/lancet-protocol/AGENTS.md`, `FINDINGS.md`, `FIX.md`

---

## FINDINGS T-23-03: 158 条 lattice-workspace keep 行补记 delete（owner 决议选项 a） — 2026-07-19

Severity: TRIAGE-DECISION (FINDINGS.md T-23-03 option a; owner-directed re-decision
of the signed-off count baseline).

**Problem:** The 158 `lattice-workspace` ledger rows still carried
decision=keep even though #62 deleted the crate — inconsistent with slice
3's 11 OBSOLETE rows, which were re-decided to delete for the same reason
(product surface deleted in #59–#62; tests no longer exist). Their reason
strings also retained retired KVM-lane vocabulary (fixed separately in the
review-fix round below).

**Fix:**
- Re-decide all 158 `crate=lattice-workspace` rows in
  `docs/test-triage-decisions.json`: keep → delete, replacement → `n/a`,
  reason → "#62 已删除 lattice-workspace crate（Upper/MSB 产品面），对应测试
  已不存在；补记 delete（FINDINGS.md T-23-03），不向 #46 虚构 replacement。"
  identity fields and `target_layer=local_invariant` unchanged.
- Sync `docs/TEST-TRIAGE.md`: §1 汇总 (保留 1337→1179 / 删除 26→184),
  per-crate table (lattice-workspace 158/0/0 → 0/0/158), §1.5 决定分布
  (final), section header, and all 158 per-test rows (决定 保留→删除,
  替代场景 —→n/a, 理由 synced). Layer distribution unchanged (1358/45/46).
- Move FINDINGS.md T-23-03 to OBSOLETE / CLOSED with the resolution record.

**Verification:**
- Ledger recount: keep 1179 / elevate 86 / delete 184, total 1449 (only the
  158 re-decided rows changed; diff confined to 474 JSON lines = 158 × 3 fields).
- Full 1449-row prose↔ledger consistency check (decision / layer /
  replacement / reason): zero mismatches; same checker against HEAD also
  passes, so no new drift introduced. All 19 per-crate count rows match.

**Changed files:**
- `docs/test-triage-decisions.json`
- `docs/TEST-TRIAGE.md`
- `FINDINGS.md` (T-23-03 → closed)

---

## Codeberg #23 review-fix: two-axis review findings closed (FIX.md backfill, KVM residue, harness dedup) — 2026-07-19

Severity: REVIEW-FIX (post-landing two-axis review of df62ec84..HEAD: Standards 2 hard
violations + 5 judgement calls, Spec 1 medium + 1 low + 1 info).

**Problem:** (1) FIX.md had no entries for slices 2–4 and TEST-TRIAGE.md
pointed at a nonexistent FIX.md migration record (dangling pointer). (2) 157
`lattice-workspace` ledger rows still claimed "KVM live 仍为可选车道" —
retired vocabulary contradicting the crate-local AGENTS.md and the range's
own removal claim. (3) `driver()`/`run()`/`ok_or_artifacts()` were
byte-duplicated across fast CLI test files. (4) 5 prose replacement cells
kept stale 延期/冗余保留 prefixes after the scenarios had landed. (5)
`release_smoke.rs` carried an orphan doc comment, empty section banners, and
a stale module coverage line. (6) Repeated scenario-name string literals.

**Fix:**
- Backfill FIX.md entries for slice 2, slice 3, slice 4, and the 9f238b79
  vocabulary closeout (making the TEST-TRIAGE pointer valid); the closeout
  entry records the identity-rename deviation with its sign-off (commit
  message: "退役词汇清零优先于 identity 冻结", user-directed) and notes the
  plan anchors were stale predictions — ledger identity lines verified
  unchanged against df62ec84.
- Rewrite the 157 stale reason strings in JSON + prose to "保留（历史决定；
  lattice-workspace 已随 #62 删除，行留档待重审，见 FINDINGS.md T-23-03）";
  identity/decision/counts untouched (keep 1337 / elevate 86 / delete 26 /
  total 1449; layers 1358/46/45). The keep-vs-deleted-crate re-decision
  itself changes the signed-off baseline, so it is filed as FINDINGS.md
  T-23-03 for human triage.
- Fix the 5 stale replacement cells to the sibling-row convention
  "已删除原测试，由 `planned_*` 接管" (1× planned_lancet_cli, 4×
  planned_session_cli).
- Extract `driver()`/`run()`/`ok_or_artifacts()` into a new shared
  `lattice-system-harness::cli` module; convert `fast_lancet_cli.rs` and
  `fast_session_cli.rs`. Other fast/direct/pty test files keep local copies
  (outside this change's scope; follow-up noted).
- `fast_session_cli.rs`: `assert_session_new_contract` now delegates JSON
  parsing to `parse_json_or_artifacts`; new `session_new_args(branch)` helper
  removes the duplicated `session new` argv in `create_session` /
  `create_session_on_branch`; scenario-name literals replaced with
  `&spec.name` (same file: `fast_lancet_cli.rs` too).
- `release_smoke.rs`: drop the orphan doc comment and the empty
  models/resolve banners (the session-status banner stays as the header of
  the slice-2 tombstone); module coverage line now describes the actual
  remaining tests.

**Verification:**
- `cargo xtask ci` → All checks passed (fmt / clippy -D warnings / build /
  harness-boundary / workspace tests incl. fast_lancet_cli 2 +
  fast_session_cli 15 + PTY smoke).
- `cargo xtask lancet-boundary` → passed.
- `cargo test -p lattice-cli --test release_smoke` → 3 passed.
- Ledger recount: keep 1337 / elevate 86 / delete 26 / total 1449; layers
  1358/46/45; zero retired-token rows; prose §1 汇总 matches.

**Changed files:**
- `crates/lattice-system-harness/src/cli.rs` (new), `src/lib.rs`
- `crates/lattice-system-harness/tests/{fast_lancet_cli,fast_session_cli}.rs`
- `crates/lattice-cli/tests/release_smoke.rs` (comments only)
- `docs/TEST-TRIAGE.md`, `docs/test-triage-decisions.json` (reason/replacement cells)
- `FIX.md`, `FINDINGS.md`

---

## Codeberg #23 vocabulary closeout: retired KVM-lane vocabulary removed from code, ledger, and TEST-TRIAGE — 2026-07-19

Severity: REFACTOR (#45 closeout follow-up; commit 9f238b79; applies a
stricter user-directed standard than the §4.7 grep gate; historical-document
rewrite tracked by Codeberg issue #65).

**Problem:** Retired KVM-lane field values (`real_microsandbox`,
`msb_full_turn`, `planned_*_msb`, `LATTICE_HARNESS_MSB`, and identity
names/file paths containing `msb`/`microsandbox`) were still present in the
machine ledger, `docs/TEST-TRIAGE.md`, and code comments even though #62
deleted the lane and slice 3 retired the layers.

**Fix:**
- Zero retired field values across code, `docs/test-triage-decisions.json`,
  and `docs/TEST-TRIAGE.md`; meta text describing the retirement itself
  ("no KVM, no MSB daemon") retained.
- Rename 8 JSON identity names and correct 4 file paths containing retired
  tokens (all already-deleted tests).
- Tag 6 historical ADR/plan/spec documents with an outdated-vocabulary
  warning blockquote pointing at Codeberg issue #65 for the rewrite.
- **Deviation record (sign-off):** this intentionally relaxes slice 3 §3.5's
  identity-snapshot invariant ("`crate/file/line/name/async/ignored` identity
  字段不变") for those 8 names + 4 files — an explicit user-directed
  trade-off recorded in commit 9f238b79's message ("退役词汇清零优先于
  identity 冻结"). Total 1449 and decision counts (keep 1337 / elevate 86 /
  delete 26) unchanged. The line anchors cited in
  `docs/plans/2026-07-19-issue45-fast-residual-ledger-closeout.md`
  (lancet.rs:289, session.rs:1284/1345) were stale plan predictions; the
  ledger identity line fields did not drift in-range (verified against
  df62ec84).

**Verification:**
- rg for retired field values in JSON/TEST-TRIAGE.md/code: zero hits; meta
  uppercase MSB retained (17 occurrences, describing the retirement).
- JSON 1449 rows, decision counts unchanged.
- `cargo xtask ci` all green.

**Changed files:**
- `docs/test-triage-decisions.json`, `docs/TEST-TRIAGE.md`
- `crates/lattice-system-harness/{AGENTS.md,src/artifacts.rs,tests/direct_local_turn.rs}`
- 6 historical docs tagged with the outdated-vocabulary warning

---

## Codeberg #23 slice 4: TEST-TRIAGE prose synced with post-slice-3 machine ledger — 2026-07-19

Severity: FEATURE (测试重构 4/6, slice 4 of #45 closeout;
docs/plans/2026-07-19-issue45-slice4-ledger-prose-consistency.md; commit 4c2c861d).

**Problem:** After slices 1–3 rewrote the machine ledger, the prose in
`docs/TEST-TRIAGE.md` (scenario catalog, summary counts, §1.5 execution
status, crate section headers, per-test rows) no longer matched it.

**Fix:**
- §1 catalog: register the 44 assertion-level `planned_direct_*` #46 IDs and
  the landed `direct_local_plain_text_turn_commits_and_exit_zero`; move
  `msb_full_turn` + 11 `planned_*_msb` IDs to a dedicated historical/obsolete
  subsection; mark `planned_lancet_cli` 已落地.
- Counts: keep 1338→1337 / elevate 96→86 / delete 15→26 (total 1449);
  per-crate headers resynced (lattice-api 223/25/12,
  lattice-system-harness 56/1/0).
- §1.5 rewritten: fast residual zero table, #62 skeleton takeover record, 11
  OBSOLETE delete identities, 44-row #46 handoff table, MSB vocabulary
  retirement note, final layer/decision distribution.
- 90 per-test rows synced to the JSON (layer names, replacement atoms,
  reasons); rich landed-takeover cells preserved where the atom matched.

**Verification:**
- §4.4 crate count headers: all 19 in sync; §4.5 replacement coverage: all
  50 atoms present; §4.7 forbidden-token grep: zero in JSON and §2 table.
- Full 1442-row consistency check (1449 entries, 7 duplicate names across
  crates): zero decision/layer/replacement/reason mismatches.

**Changed files:**
- `docs/TEST-TRIAGE.md`

---

## Codeberg #23 slice 3: 57 stale-layer rows retired/mapped to assertion-level #46 scenarios — 2026-07-19

Severity: FEATURE (测试重构 4/6, slice 3 of #45 closeout;
docs/plans/2026-07-19-issue45-slice3-msb-vocabulary-retirement.md; commit ce8880c0).

**Problem:** 57 ledger rows still carried `target_layer=real_microsandbox`
(and `*_msb` replacement names) after #62 deleted the lane — dead vocabulary
with no executable target.

**Fix:**
- Redistribute all 57 stale-layer rows across `direct_local_binary` (46) and
  `local_invariant` (11 obsolete), with exactly 12 decision deltas: 11
  elevate→delete for identities whose product contract was deleted in
  #59–#62 (no fabricated #46 replacements), and 1 keep→elevate for the
  historical harness identity taken over by the landed #62
  `direct_local_plain_text_turn_commits_and_exit_zero`.
- Map 44 internal tests to 44 distinct assertion-level `planned_direct_*`
  #46 scenario IDs — exact per-row mapping, no regex family fallback.
- PTY smoke row: layer→`direct_local_binary`, decision stays keep.
- 26 `local_invariant` keep rows: reason rewritten to swap retired MSB
  scenario references for live equivalents (decision/layer/replacement
  unchanged).
- Identity snapshot: crate/name/async/ignored unchanged for all 1449 rows;
  file/line unchanged for 1448; the single exception (historical harness
  identity whose file path contained the forbidden `msb_full_turn` token) is
  justified in commit ce8880c0's message.

**Verification:**
- Mechanical transformation script: all assertions pass (57 rows; 12 deltas;
  45 exact + 11 obsolete + 1 PTY; final keep 1337 / elevate 86 / delete 26).
- rg `real_microsandbox|planned_[a-z_]+_msb|msb_full_turn` in JSON: zero hits.
- 44 `planned_direct_*` IDs all distinct; re-dump byte-identical (no format drift).

**Changed files:**
- `docs/test-triage-decisions.json`

---

## Codeberg #23 slice 2: fast_session_cli branch scenarios + 4 redundant tests deleted — 2026-07-19

Severity: FEATURE (测试重构 4/6, slice 2 of #45 closeout;
docs/plans/2026-07-19-issue45-slice2-branch-fast-scenarios.md; commit 55128410).

**Problem:** The project-branch create/reopen contract was guarded only by
crate-internal `lattice-cli` tests elevated to `fast_binary`
(`planned_session_cli`) without landed external scenarios, and two
`release_smoke` session-status tests were fully covered yet still present.

**Fix:**
- Two ordinary-lane fast-binary scenarios in
  `crates/lattice-system-harness/tests/fast_session_cli.rs`:
  `fast_session_new_json_reports_requested_project_branch` (non-default
  `project_branch` echoed by `session new --json`) and
  `fast_session_project_branch_persists_across_cli_invocations` (a second,
  independent CLI process reopens via `session status --json`, asserting
  `project_target_branch` — strictly stronger than the in-crate reopen
  round-trip it supersedes). New `create_session_on_branch` helper; no
  product crate dependency (harness-boundary green).
- Delete the 4 superseded tests: `session.rs`
  `project_branch_creation_json_output_includes_branch` /
  `project_branch_persists_through_reopen`; `release_smoke.rs`
  `smoke_session_status_list_shows_seeded_session` /
  `smoke_session_status_specific_session`. `seed_session` retained (still
  used by `smoke_session_log_shows_seeded_turn`).
- Ledger: the 4 rows' `reason` records the black-box takeover;
  decision/target_layer/replacement/identity unchanged (prose sync deferred
  to slice 4).

**Verification:**
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness --test fast_session_cli` → 15 passed (2 new + 13 existing).
- `cargo test -p lattice-cli` → 129 passed.

**Changed files:**
- `crates/lattice-system-harness/tests/fast_session_cli.rs`
- `crates/lattice-cli/src/commands/session.rs` (test deletions only)
- `crates/lattice-cli/tests/release_smoke.rs` (test deletions + tombstone)
- `docs/test-triage-decisions.json` (reason strings)

---

## Codeberg #23 slice 1: `fast_lancet_cli` black-box replaces internal `cep_status_missing_db` test — 2026-07-19

Severity: FEATURE (测试重构 4/6, slice 1 of #45 closeout;
docs/plans/2026-07-19-issue45-slice1-fast-lancet-cli.md).

**Problem:** The `lattice lancet status` empty-state contract (no audit DB →
exit 0, `recent_calls: []`, `last_guard_verdict: null` in JSON; `Recent CEP
Calls:` / `none` / `Last Guard Verdict` in text) was only guarded by the
crate-internal unit test `cep_status_missing_db_returns_empty_state` in
`lattice-cli`, which triage elevated to `fast_binary` under replacement
`planned_lancet_cli` but the external black-box scenario had not landed.

**Fix:**
- New `crates/lattice-system-harness/tests/fast_lancet_cli.rs` with two
  ordinary-lane fast-binary scenarios:
  `fast_lancet_status_missing_audit_store_reports_empty_state_json` (exit 0
  + JSON shape) and
  `fast_lancet_status_missing_audit_store_text_mode_smoke` (exit 0 + three
  text substrings). Harness pattern mirrors `fast_audit_cli.rs`; no product
  crate dependency (`cargo xtask harness-boundary` passes).
- Delete the superseded internal test
  `cep_status_missing_db_returns_empty_state` from
  `crates/lattice-cli/src/commands/lancet.rs`. The seeded-state
  `cep_status_seeded_audit_json` test and its `ScopedXdgStateHome` /
  `seed_audit_db` fixtures are retained (deferred to `planned_audit_ops_*`,
  not in this slice's scope).
- Update `docs/test-triage-decisions.json` `reason` for
  `cep_status_missing_db_returns_empty_state` to record the black-box
  takeover; `decision` / `target_layer` / `replacement` / identity fields
  unchanged (prose↔ledger reconciliation is slice 4).

**Verification:**
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness --test fast_lancet_cli` → 2 passed.
- `cargo test -p lattice-cli` → 134 passed (seeded test + fixtures intact).
- `cargo xtask ci` → all green (fmt / clippy -D warnings / build / harness-boundary / workspace tests incl. new `fast_lancet_cli` in ordinary lane + pty smoke).
- `cargo xtask lancet-boundary` → LANCET boundary check passed.
- Full harness suite `cargo test -p lattice-system-harness` → 90 passed.

**Changed files:**
- `crates/lattice-system-harness/tests/fast_lancet_cli.rs` (new)
- `crates/lattice-cli/src/commands/lancet.rs` (test deletion only)
- `docs/test-triage-decisions.json` (single `reason` string)

---


## gix-only cleanup: session graph + fixtures off git2/shell-git — 2026-07-17

Severity: REFACTOR (docs/plans/2026-07-17-gix-only-cleanup.md).

**Problem:** Production and test code still depended on `git2` and
`Command::new("git")` for the session graph store and fixtures, blocking the
gix-only Direct-Local target shared with #41/#42 rewind.

**Fix:**
- Rewrite `lattice-session::git_graph` over gix 0.85 (`tree-editor` feature):
  blobs, tree editor, commits, ref CAS, revwalk, merge_base.
- Delete retired SQLite→Git `src/migrate/` and CLI/API `session migrate` /
  `check_migration_guard` call sites.
- Add `lattice-session` `test-utils` fixture helpers (`init_empty_repo`,
  `init_fixture_repo`, `commit_paths`/`commit_all`) and convert API/CLI/
  harness/lancet-native fixtures off shell-git and git2.
- Fix hanging `fetch_effect_contract_negative` (network-policy preflight
  instead of real DNS/HTTP to evil.example.com).

**Verification:**
- `rg -n "git2" crates xtask --type rust` → zero
- `rg -n 'Command::new\("git"\)' crates xtask --type rust` → zero
- `cargo tree -i git2` → no package
- `cargo test -p lattice-session --lib` → 54 ok
- `cargo test -p lattice-api --lib --tests` → all ok
- `cargo test -p lattice-cli` → all ok
- `cargo test -p lattice-system-harness` → all ok
- `cargo test -p lancet-native --lib` → 118 ok (1.55s)

**Changed:** workspace `Cargo.toml`/`Cargo.lock`, `lattice-session` (git_graph,
test_utils, migrate removed), `lattice-api`/`lattice-cli`/`lattice-tui`,
`lancet-native` fixtures + effect contract test, `lattice-system-harness`.

## Codeberg #29: official git-ready Upper OCI image inputs + probe + publish path — 2026-07-14

Severity: FEATURE (Upper rootfs 1/8; parent #28 commits 1–3, publish plumbing for 4).

**Problem:** Production Upper still depends on guest git for baseline delivery,
OID checks, dirty status, and candidate export, but the repo had no project-owned
git-ready OCI image, no black-box capability contract, and no Codeberg publish /
digest-lock path. Runtime still relied on caller-chosen images and per-turn
apt-get (see M-33-06).

**Fix summary:**
- Added `images/upper/` with pinned Debian bookworm-slim digest
  (`base-reference.txt` + matching `Containerfile` `FROM` digest), minimal
  packages (`git`, `ca-certificates`, `dash`, `coreutils`), OCI
  source/revision/version labels, and **no** `ENTRYPOINT`.
- Added black-box `probe.sh` exercised via `build-and-probe.sh` with entrypoint
  cleared: PATH tools, `/workspace` + base64 write, git pack ingest /
  `update-ref` / `symbolic-ref` / checkout / `rev-parse`, bundle `stat` +
  `dd|base64` binary round-trip.
- Added `verify-inputs.sh` for pin/label/lock-shape static checks (lock optional
  until maintainer checkpoint; when present only `schema_version` + canonical
  `codeberg.org/astrin/lattice-upper@sha256:<64-hex>`).
- Added `.github/workflows/upper-image.yml`: PR/push path builds+probes with
  read-only permissions and no registry push; manual `workflow_dispatch` runs
  `publish.sh` (candidate push → manifest digests → anonymous pull/probe →
  optional non-`latest` version promote → provenance + suggested lock draft).
  Publish never commits the repo lock.
- Docs: `images/upper/README.md`, pointers in `docs/DEVELOPMENT.md` and
  `docs/TESTING.md`.

**Not done in this change (maintainer checkpoint residual):** first public
publish of `codeberg.org/astrin/lattice-upper` and committing
`images/upper/lock.json`. Per #29, the lock must stay **absent** until
anonymous verify of a real manifest digest succeeds; no placeholder digest is
committed. Runtime descriptor embedding is #30.

**Verification:**
- `images/upper/verify-inputs.sh` — PASS (lock absent)
- lock schema negative tests (extra key / tag-only ref rejected; good lock
  accepted) — PASS
- `images/upper/build-and-probe.sh` — PASS (`lattice-upper:local`, linux/amd64,
  probe PASS with entrypoint cleared)

**Changed files:** `images/upper/**`, `.github/workflows/upper-image.yml`,
`docs/DEVELOPMENT.md`, `docs/TESTING.md`, `FIX.md`.

---

## Codeberg #22: full test triage inventory (keep/elevate/delete) — 2026-07-09

Severity: FEATURE (测试重构 3/6).

**What:** Enumerated all `#[test]` / `#[tokio::test]` under `crates/**` and
`xtask/**` plus the `cep_family_test!` macro expansions (1449 entries) and
produced a reviewable triage list for issue #40 step 3 / issue #44. Decisions
follow RFC #41 decision two (layers) and decision six (no migration quota):
keep local invariants, elevate product paths that are tested via internal
APIs, delete source-scan / weak construction checks.

**Deliverables (docs only — no test code changed):**

- `docs/TEST-TRIAGE.md` — human-readable per-test table + scenario catalog +
  4/6 batch hints
- `docs/test-triage-decisions.json` — machine-readable copy for 4/6 scripts
- `docs/TESTING.md` — pointer to the triage list

**Snapshot counts:** keep 1298 / elevate 140 / delete 11.

**Verification:**

- JSON invariants: 1449 rows; every elevate/delete has `replacement` or `n/a`;
  every row has `reason` + `target_layer`
- Completeness cross-check: a script enumerates every `#[test]` /
  `#[tokio::test]` and `cep_family_test!` call in `crates/**` + `xtask/**` and
  diffs against the JSON by `(file, name)`; result is 1449 = 1449 with zero
  omissions / zero stale / zero line drift, and `async` / `ignored` / `line`
  fields all match source. The initial 1443-row snapshot missed six
  `cep_family_test!` macro expansions (validation/permission/execution/state/
  guard/effect payload families); these were added and five `#[ignore]` MSB/KVM
  integration tests had their `ignored` flag corrected false -> true.
- `git diff --stat` shows only documentation files (no `crates/**/*.rs` test edits)

## Codeberg #21: `init_git_project` left unborn HEAD on `master` — KVM scenarios failed at baseline — 2026-07-06

Severity: HIGH (both `#[ignore]` KVM scenarios failed at Upper baseline resolution on any host whose git defaults to `master`).

**Problem:** `ScenarioWorld::init_git_project` (`world.rs:116-121`) ran only
`git init` + `git config user.email/name` — it neither set the branch name
nor created an initial commit. On hosts whose git defaults to `master`
(the historical default; the git hint the owner saw confirmed this), the
repo's HEAD pointed at `refs/heads/master` with no commits (unborn). The
scenarios pass `project_branch = "main"` to `session new`, but the repo
was on `master` with nothing committed.

When `session send` ran, the product's `resolve_baseline_head`
(`crates/lattice-api/src/turn/upper.rs:175-184`) called `repo.head()` →
`git2::Error` `UnbornBranch (-9)` (`reference 'refs/heads/master' not
found`). The turn never started: zero JSONL events, empty stdout, and
`fake_llm_requests.txt` was never written (the LLM was never called). The
failure surfaced as `required event subsequence not satisfied: missing
'turn_started' after position 0; observed types = []`.

The failure bundle (`target/harness-failures/msb-plain-text-turn/`)
captured the root cause in `stderr.txt`:
`error: git baseline resolution failed: reference 'refs/heads/master' not
found; class=Reference (4); code=UnbornBranch (-9)`.

**Fix:** `init_git_project` now:

1. `git symbolic-ref HEAD refs/heads/main` — points HEAD at `main` before
   the first commit so the initial commit lands on `main` (matching the
   `project_branch = "main"` the scenarios pass to `session new`). Portable
   across all git versions (unlike `git init -b main` which requires
   git 2.28+).
2. `git add -A` + `git commit --allow-empty -m "harness init"` — creates an
   initial commit so HEAD is born and `repo.head()` peels to a commit.
   `--allow-empty` makes this robust against call ordering (even if no
   project file has been written yet, the commit succeeds); `git add -A`
   picks up whatever the scenario already wrote (e.g. the agent profile)
   so the baseline reflects the real pre-turn project state.

**Verification:**

- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — clean.
- `cargo fmt -p lattice-system-harness --check` — clean.
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`
  — 50 unit + 4 fast black-box tests pass (fast lane unaffected: those tests
  do not call `init_git_project`). The 2 KVM scenarios remain `#[ignore]`-d.
- Live KVM re-run by owner pending (L-21-01); the failure bundle from the
  prior run confirmed the diagnosis and is now stale (the fix prevents the
  UnbornBranch error from recurring).

**Changed files:** `crates/lattice-system-harness/src/world.rs`, `FIX.md`.

---

## Codeberg #21 review follow-up: wire `LATTICE_MSB_IMAGE` into ignored scenarios — 2026-07-05

Severity: HIGH (the two `#[ignore]` KVM scenarios could not run as committed).

**Problem:** `tests/msb_full_turn.rs` and `tests/pty_tui_smoke.rs` both
declared an `AGENT_TOML` with an empty `[workspace]` section (no `image`
field). The `msb_full_turn.rs` doc comment claimed "the image is supplied
via the `LATTICE_MSB_IMAGE` env var at run time", but a full-repo grep found
**zero code reading `LATTICE_MSB_IMAGE`** — the env var existed only in that
one comment. The product reads `[workspace].image` from `agent.toml` via
`resolve_upper_config` (`crates/lattice-api/src/turn/upper.rs:225-232`),
which returns `UpperPrepareError::MissingWorkspaceSource` ("workspace.image
required for Upper-backed tools") when the field is absent or empty.

Consequence: on a real KVM host, `--include-ignored` would fail both
scenarios at Upper preparation, before any turn ran — so the owner-side
full-gate verification tracked by `FINDINGS.md` **L-21-01** could not pass
as-is. The comment was aspirational and had never been wired up.

**Fix:**

- `crates/lattice-system-harness/tests/msb_full_turn.rs`: replaced the
  `AGENT_TOML` const with an `AGENT_TOML_TEMPLATE` carrying an `{image}`
  placeholder, plus a `render_agent_toml()` helper that reads `LATTICE_MSB_IMAGE`
  and substitutes it into `[workspace].image`. **`LATTICE_MSB_IMAGE` is
  required** — the harness bakes no default (an earlier draft defaulted to
  the workspace contract tests' `docker.m.daocloud.io/library/python:3.12-slim`,
  but that was redundant with the env var and coupled a black-box harness to
  a product-test-specific China mirror that could go stale or be unreachable
  from the owner's host; requiring the var keeps a single source of truth and
  is consistent with the KVM lane already being explicit opt-in). Unset or
  empty panics with a clear message naming the env var and an example ref,
  same precondition pattern as `LatticeDriver::from_env().expect(...)`. The
  helper is a hand-written template render, **not** serde — the harness
  boundary forbids `lattice-profile`, so `AgentProfile::serialize` is
  unavailable. The test body now writes `&render_agent_toml()`.
- `crates/lattice-system-harness/tests/pty_tui_smoke.rs`: same fix (same
  required-var, same helper).
- `crates/lattice-system-harness/AGENTS.md`: the full-gate verification
  bullet now shows `LATTICE_MSB_IMAGE=<OCI ref>` in the command and states
  it is required (no default), with an example ref.

The substituted TOML is accepted by `resolve_explicit_image_or_template`
(`upper.rs:152-154`): a fully-qualified OCI ref contains both `/` and `:`,
classifying as `ImageOrTemplate::Image`; a bare alphanumeric template name
classifies as `Template`. The owner can pass either.

**Verification:**

- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — clean.
- `cargo fmt -p lattice-system-harness --check` — clean.
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`
  — 50 unit + 4 fast black-box tests pass; the 2 MSB/PTY scenarios are still
  `#[ignore]`-d (now compiling with the required-env-var image injection)
  and skipped on this non-KVM host. Live KVM execution remains owner-side
  (L-21-01).
- The `render_agent_toml()` substitution + required-var panic was reasoned
  through against `resolve_explicit_image_or_template`'s accept rule; it
  cannot be exercised live without KVM.

**Residual:** `FINDINGS.md` **L-21-01** still stands — the real MSB/PTY live
run is owner-side full-gate verification. This fix removes the
`MissingWorkspaceSource` blocker that would have prevented that run from
passing; it does not constitute the live run itself.

**Changed files:** `crates/lattice-system-harness/tests/{msb_full_turn,
pty_tui_smoke}.rs`, `crates/lattice-system-harness/AGENTS.md`, `FIX.md`.

---

## Codeberg #21 review follow-up: harness probe/pgid/shutdown/sentinel-scan/dedup/dead-code — 2026-07-05

Severity: MEDIUM (two implementation errors + one RFC #41 decision-eight spec gap; remainder standards).

**Problem:** A two-axis (Standards + Spec) review of the #43 commit
(`186d675d`) found no hard standards violations but several baseline smells
and three implementation errors plus one spec gap:

- **(c#2, implementation error)** `layer_guard::require_msb_env` spawned its
  `lattice lancet status` probe WITHOUT `set_new_process_group`, so
  `child.id()` was the child PID, not a PGID. The timeout path then ran
  `kill -KILL -<pid>`, but the child was not a group leader → ESRCH or
  hitting an unrelated group that happened to have that id. The driver's own
  scenario children did set the group; only the probe had drifted.
- **(c#1, implementation error)** `tests/pty_tui_smoke.rs` matched the prompt
  character `>` directly in the scenario (`wait_for(..., |s| s.contains('>'))`),
  violating RFC #41 decision three: "prompt recognition and ANSI cleanup only
  exist inside the driver; scenarios assert no display details".
- **(c#3 / Standards #27, implementation error + dead branch)** `FakeLlmServer`
  set the listener back to blocking (`set_nonblocking(false)`), making the
  `serve_loop` `WouldBlock` branch unreachable; `Drop` set a `shutdown` flag
  the blocking `accept` could never observe, then bound a throwaway listener
  (no wake). The serve thread and bound port survived for the whole test
  process; `_handle` was never joined. The `set_script` method and `script`
  field were also dead (no callers).
- **(a#2, spec gap)** RFC #41 decision eight requires "redact failure
  artifacts before writing, then scan after writing". Pre-write redaction was
  implemented, but there was no post-write scan of the on-disk artifacts;
  `assert_no_secret` only scanned the raw `Output`, not the written files.
- **(Standards #23)** The `world.txt` context block was duplicated across
  `assert_with_failure_artifacts`, `assert_with_full_artifacts`, and the MSB
  test's `write_full_bundle` (already diverging in how they sourced the
  command summary).
- **(Standards #24)** The `kill -KILL -{pgid}` shape was duplicated across
  `driver.rs`, `layer_guard.rs`, and `pty_tui.rs`.
- **(Standards #28)** pgid type drift: `driver.rs` used `u32`, `pty_tui` returned
  `i32`, and `pty_tui_smoke.rs` cast `pgid as u32`.
- **(Standards #25/#26 + Spec b#1)** Four public APIs with no callers:
  `assert_with_full_artifacts`, `SessionSendDriver::for_existing`,
  `SessionSendDriver::run_json`, `FakeLlmServer::set_script` — speculative
  generality / out-of-scope general test DSL (RFC #40).

**Fix:**

- `crates/lattice-system-harness/src/driver.rs`: promoted
  `set_new_process_group` and `kill_process_group` to `pub(crate)` so the
  layer-gate probe and PTY driver share the one implementation (documented
  why the probe in particular needs its own group).
- `crates/lattice-system-harness/src/layer_guard.rs`: the
  `require_msb_env` probe now calls `set_new_process_group(&mut cmd)` before
  spawn and `kill_process_group(pgid)` on timeout — fixing (c#2). The
  inlined `kill -KILL` block is gone.
- `crates/lattice-system-harness/src/pty_tui.rs`: added
  `PtyTuiDriver::wait_for_prompt` as the prompt-recognition seam (the `>`
  shape now lives inside the driver, never in a scenario). `pgid` is now
  `Option<u32>` (filtered for positive leaders, cast at the portable-pty
  boundary), `pgid()` returns `Option<u32>`, and `kill_and_reap` delegates to
  the shared `kill_process_group` — fixing (c#1), Standards #24, and #28. The
  now-unused `Stdio` import was removed.
- `crates/lattice-system-harness/tests/pty_tui_smoke.rs`: the scenario calls
  `tui.wait_for_prompt(...)` instead of matching `>`, and the `pgid as u32`
  cast is gone (`processes_in_pgid(pgid)` takes the `u32` directly).
- `crates/lattice-system-harness/src/fake_llm.rs`: the listener is now
  non-blocking, so the `serve_loop` `WouldBlock` branch polls `shutdown` every
  ~5ms and is reachable; `Drop` sets `shutdown` and joins the serve thread
  (renamed `_handle` → `serve_handle`), so the thread exits and the bound port
  releases before `Drop` returns. Removed the dead `set_script` method and the
  now-unused `script` field — fixing (c#3) / Standards #27.
- `crates/lattice-system-harness/src/artifacts.rs`: added
  `scan_artifacts_for_secrets` (walks the failure dir, redacts any file that
  still contains a secret sentinel in place, returns the patched paths; skips
  non-UTF8 files) and `write_world_context` (the shared, redacted
  scenario/command/project/home/port/env context writer). Both have unit
  tests (no-leak, patch-in-place, skip-non-utf8, empty-list-noop, context
  writes+redacts).
- `crates/lattice-system-harness/src/assert.rs`: `assert_with_failure_artifacts`
  now uses `write_world_context` and runs `scan_artifacts_for_secrets` after
  writing, recording any catch in `sentinel_scan.txt` (the scan never masks
  the original assertion error). Removed the dead
  `assert_with_full_artifacts` and trimmed its now-unused imports
  (`CapturedRequest`, `write_fake_llm_log`, `write_workspace_listing`,
  `redact_env_pair`).
- `crates/lattice-system-harness/src/session_send.rs`: removed the dead
  `SessionSendDriver::for_existing` and `::run_json` (and the now-unused
  `Output` / `Duration` imports).
- `crates/lattice-system-harness/src/lib.rs`: updated re-exports — removed
  `assert_with_full_artifacts`; added `scan_artifacts_for_secrets` and
  `write_world_context`.
- `crates/lattice-system-harness/tests/msb_full_turn.rs`: `write_full_bundle`
  now uses the shared `write_world_context`, redacts the cross-validation
  (`session log/status --json`) outputs **before** writing (they come
  straight from the product and could echo prompt text), and runs the
  post-write `scan_artifacts_for_secrets`. The duplicated world-context block
  and `redact_env_pair` import are gone; the "Mirrors assert_with_full_artifacts"
  doc was rewritten.

**Verification:**

- `cargo xtask harness-boundary` — PASS (3/3 groups; no boundary regression
  from the `pub(crate)` promotions or new helpers).
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` — clean.
- `cargo fmt -p lattice-system-harness --check` — clean.
- `cargo build -p lattice-cli --bin lattice` — ok.
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`
  — 50 unit tests + 4 fast black-box tests pass; the 2 MSB/PTY scenarios are
  correctly `#[ignore]`-d. New tests included: `write_world_context_writes…`,
  `write_world_context_redacts_secret_env_value`,
  `scan_artifacts_for_secrets_no_leak_returns_empty`,
  `scan_artifacts_for_secrets_patches_leaked_file_in_place`,
  `scan_artifacts_for_secrets_skips_non_utf8_files`,
  `scan_artifacts_for_secrets_empty_secret_list_is_noop`.
- The `FakeLlmServer` shutdown fix is exercised by the existing
  `server_captures_request_and_returns_plain_text_sse` test (start → request
  → drop no longer leaks the thread/port).

**Residual:** `FINDINGS.md` **L-21-01** is unchanged — the real Microsandbox
full-turn + PTY smoke live runs remain owner-side full-gate verification
(this environment has no KVM/MSB daemon). The fixes here harden the scenario
code (probe process-group, prompt encapsulation, post-write sentinel scan)
but do not change the deferral of the live run.

**Changed files:** `crates/lattice-system-harness/src/{driver,layer_guard,
pty_tui,fake_llm,artifacts,assert,session_send,lib}.rs`,
`crates/lattice-system-harness/tests/{pty_tui_smoke,msb_full_turn}.rs`,
`FIX.md`.

---

## Codeberg #26 audit: `lancet-boundary` parity with retired shell script — 2026-07-05

Severity: LOW (audit / no code change).

**Audit:** Verified `cargo xtask lancet-boundary` (6 groups, 4 allowlists, scan
ranges and exclusion rules) is item-by-item equivalent to the retired
`scripts/lancet-boundary-check.sh` (deleted in `bdf8204c`), with no silent loss.

**Method:** Restored the retired shell script from `bdf8204c~1` and built two
hand fixtures under `/tmp` — a dirty one with one violation per group plus
allowlisted/excluded canaries (G3 `usage_turn.rs`, G5 `registry.rs`, G6
`run_tests.rs`/`foo_tests.rs`/`tests/`, `target/`, `.planning/`), and a clean
one. Ran the old script and the Rust checks (`lattice_agent_forbidden_dependency_graph`,
`scan_with_allowlist` for G2/G3/G5, `tool_executor_reexport_hits`,
`cli_production_bypass_hits`) against both, then diffed the hit sets.

**Result:**

- Dirty fixture: **8 hit lines, byte-identical** between old and new (sorted
  diff empty). Group→hit mapping identical for all 6 groups.
- Clean fixture: old script all-6-PASS (exit 0); Rust tooling 0 hits (exit 0).
  No false positives.
- All 4 allowlists identical, same prefix-string semantics
  (`case "$file" in "$prefix"*` ⇄ `file.starts_with(prefix)`).
- Scan ranges identical: G2/G3/G5 scan `crates/*/src` + `crates/*/tests`;
  G4 scans `crates/lattice-agent`; G6 scans `crates/lattice-cli/src` only.
- Exclusion rules identical: `target/` and `.planning/` pruned at any depth
  (`--exclude-dir` ⇄ `is_excluded_walk_dir` + `filter_entry`); G6 additionally
  skips `*_tests.rs` filenames and any `tests/` dir under `lattice-cli/src`
  (`--exclude='*_tests.rs' --exclude-dir=tests` ⇄ `skip_cli_test_artifact`).
- `run_tests.rs` is flagged by G2 (DefaultToolExecutor scan) on BOTH sides —
  it lives only in `CLI_PRODUCTION_ALLOWLIST` (G6), not
  `DEFAULT_TOOL_EXECUTOR_ALLOWLIST` (G2). This is consistent legacy behavior,
  not a regression or silent loss.

**No code change.** The throwaway parity probe used during the audit was
removed; `cargo test -p xtask` (24), clippy `-D warnings`, and
`cargo fmt --check` remain green.

---

## Codeberg #26 follow-up: per-group pos/neg unit tests for xtask boundary checks — 2026-07-05

Severity: LOW (test coverage / regression guard).

**Problem:** `cargo xtask harness-boundary` (3 groups) and `cargo xtask
lancet-boundary` (6 groups) landed with unit tests, but the "each group has
both a positive (flags a violation) and a negative (clean input → empty) test"
invariant did not hold uniformly. 5 groups lacked a dedicated
negative/clean-case test (harness G2 `manifest_workspace_inheritance`, harness
G3 `metadata_transitive_workspace_members`, lancet G1
`lattice_agent_forbidden_dependency_graph`, lancet G3 `default_tool_definitions`
scan, lancet G4 `tool_executor_reexport_hits`), and lancet G2
(`DefaultToolExecutor` scan) had only a helper-level positive
(`filter_allowlist_drops_matching_prefix`), no direct positive through
`scan_with_allowlist` with that needle+allowlist.

**Fix:**

- harness G2: `manifest_workspace_inheritance_clean_manifest_empty` — clean
  manifest (no `workspace = true`) asserts empty.
- harness G3: `transitive_workspace_members_clean_workspace_empty` — workspace
  where `lattice-system-harness` has no workspace-member deps asserts empty.
- lancet G1: `lattice_agent_clean_manifest_no_hit` — `lattice-agent` with no
  `lancet-native` dep/string asserts empty.
- lancet G2: `default_tool_executor_scan_flags_non_allowlisted` — direct
  `scan_with_allowlist("DefaultToolExecutor", DEFAULT_TOOL_EXECUTOR_ALLOWLIST)`
  retains a non-allowlisted `lattice-api/src/lib.rs` hit.
- lancet G3: `default_tool_definitions_allowlisted_fixture_dropped` — the
  allowlisted `crates/lattice-api/tests/usage_turn.rs` fixture is dropped
  (asserts empty).
- lancet G4: `tool_executor_reexport_clean_agent_empty` —
  `lattice-agent/src/lib.rs` with no `ToolExecutor` re-export asserts empty.

**Verified:** `cargo test -p xtask` — 24 passed (was 18; +6).
`cargo clippy -p xtask --all-targets -- -D warnings` — clean.
`cargo fmt --all --check` — clean. `cargo xtask harness-boundary` ✓,
`cargo xtask lancet-boundary` ✓.

**Changed files:** `xtask/src/harness_boundary/check.rs`,
`xtask/src/lancet_boundary/check.rs`, `FIX.md`.

---

## Codeberg #26 follow-up: track `.cargo/config.toml`, standardize `cargo xtask` — 2026-03-14

Severity: LOW (developer ergonomics / clone parity).

**Problem:** After #26, CI and docs used `cargo run --package xtask --` because the
whole `.cargo/` tree was gitignored, so fresh clones could not rely on a tracked
`cargo xtask` alias.

**Fix:**

- `.gitignore`: keep `.cargo/*`, un-ignore `!.cargo/config.toml` only.
- Track `.cargo/config.toml` with `[alias] xtask = "run --package xtask --"`.
- User-facing automation strings now use `cargo xtask <subcommand>` (CI, docs,
  release gates, harness AGENTS, release_smoke).

**Verified:** `cargo xtask harness-boundary` ✓,
`release_smoke_release_gate_v1_4_includes_lancet_boundary_check` ✓.

**Changed files:** `.gitignore`, `.cargo/config.toml`, `.github/workflows/ci.yml`,
`CONTRIBUTING.md`, `docs/TESTING.md`, `docs/DEVELOPMENT.md`,
`docs/v1.3-alpha-walkthrough.md`, `crates/lattice-system-harness/AGENTS.md`,
`crates/lattice-system-harness/src/assert.rs`,
`crates/lattice-cli/tests/release_smoke.rs`, `scripts/release-gate-v1.3.sh`,
`scripts/release-gate-v1.4.sh`, `FIX.md`.

---

## Codeberg #26: Rust xtask replaces shell CI and boundary scripts — 2026-07-05

Severity: MEDIUM (developer workflow / CI maintainability).

**Problem:** Local CI and architecture boundary enforcement lived in
`scripts/ci-local.sh`, `scripts/system-harness-boundary-check.sh`, and
`scripts/lancet-boundary-check.sh`. Shell + inline Python/grep duplicated
logic, was harder to unit-test, and drifted from documented crate counts.
GitHub Actions could not rely on a `cargo xtask` alias because `.cargo/` is
gitignored.

**Fix:**

- Added root `xtask` workspace member with `ci`, `harness-boundary`, and
  `lancet-boundary` subcommands (Rust ports of the three shell checks, with
  per-group adversarial unit tests).
- Documented and wired automation entrypoints as
  `cargo run --package xtask -- <subcommand>` so fresh clones work without a
  gitignored `.cargo/` alias (local dev may still use an optional `xtask` alias).
- Rewired `.github/workflows/ci.yml` to
  `cargo run --package xtask -- harness-boundary`, an explicit
  `dtolnay/rust-toolchain` install for `1.88`, and `cargo +1.88 check -p xtask`.
- Rewired `scripts/release-gate-v1.3.sh` / `v1.4.sh` and `release_smoke` to
  `cargo run --package xtask -- lancet-boundary`.
- `cargo run --package xtask -- ci` preserves `LATTICE_BIN` /
  `LATTICE_HARNESS_FAILURE_DIR` when already set (matches former `ci-local.sh`
  `:-` defaults).
- Removed retired shell scripts with no wrapper.

**Verified:** `cargo test -p xtask` ✓ (18 tests),
`cargo run --package xtask -- harness-boundary` ✓,
`cargo run --package xtask -- lancet-boundary` ✓,
`cargo run --package xtask -- ci` ✓, restored shell scripts parity ✓ on clean
tree, `release_smoke_release_gate_v1_4_includes_lancet_boundary_check` ✓.

**Changed files:** `xtask/**`, `Cargo.toml`, `Cargo.lock`,
`.github/workflows/ci.yml`, `CONTRIBUTING.md`, `docs/TESTING.md`,
`docs/DEVELOPMENT.md`, `docs/v1.3-alpha-walkthrough.md`,
`crates/lattice-system-harness/AGENTS.md`,
`crates/lattice-system-harness/src/assert.rs`,
`crates/lattice-cli/tests/release_smoke.rs`,
`scripts/release-gate-v1.3.sh`, `scripts/release-gate-v1.4.sh`;
deleted `scripts/ci-local.sh`, `scripts/lancet-boundary-check.sh`,
`scripts/system-harness-boundary-check.sh`.

---

## Codeberg #20: lattice-system-harness + boundary check + docs/port/artifact/path hardening — 2026-07-05

Severity: MEDIUM (standards + spec review of the new `lattice-system-harness` crate).

**Problem:** A standards/spec review of the new `lattice-system-harness` crate
found multiple standards violations and spec defects across the harness, the
boundary check script, and the docs:

- Standards: boundary check did not truly traverse the dep graph (read a
  non-existent `pkg` field), then was bypassable by valid TOML forms
  (single-quoted `path`, inline-table `package =` renames, string-form
  version deps like `lattice-api = "0.1"`, neutral-named product path deps
  like `guard = { package = "net-guard", path = '../net-guard' }`).
- Standards: `docs/TESTING.md` and `CONTRIBUTING.md` contradicted themselves
  (claimed tests need no prior build while the harness requires a built
  binary; described `ci-local.sh` as fmt/clippy/test only; miscounted crates
  at 16/17; the GitHub Actions step list omitted the build and boundary
  steps).
- Standards: `LATTICE_BIN` docs gave the wrong failure mechanism (claimed
  relative paths resolved from the scenario child cwd; actually resolved in
  the test process before spawn) and called "must be absolute" a code-enforced
  contract when it is a robustness recommendation.
- Standards: `LatticeCommand.timeout` and `ScenarioSpec`'s
  `step_timeout_secs`/`total_timeout_secs` were exposed but ignored at
  execution (`let _ = command.timeout;`) — speculative generality with no
  semantics.
- Spec: documented relative `LATTICE_BIN=target/debug/lattice` failed because
  `cargo test -p lattice-system-harness` runs with the crate dir as cwd,
  where `target/debug/lattice` does not exist.
- Spec: `world.rs` allocated ports via a process-local `AtomicU16` from
  18000 without ever asking the OS, then swallowed the bind error and fell
  back to 0 — parallel test processes could collide, and a port-0 world
  violated the isolation contract.
- Spec: `assert_with_failure_artifacts` wrote the bundle into `world.root()`
  (a `tempfile::TempDir`), deleted the instant the world drops — nothing
  survived for post-mortem; the semantic public-commands assertion was also
  not wired to artifact capture, and the command/env values were written
  raw (secret leakage).

**Fix:**

- Boundary check (`scripts/system-harness-boundary-check.sh`): parse the
  manifest with `tomllib` (Python 3.11+ stdlib) instead of regex. One parse
  runs (a) a forbidden-name check using the actual package name (resolved
  through `package =` renames, falling back to the dep key for string-form
  version deps like `lattice-api = "0.1"`) and (b) a location-based path
  check that flags any `path` value (in any `*-dependencies` table) resolving
  under `repo_root/crates/`, regardless of crate name or quote style. The
  cargo-metadata backstop walks the full transitive closure via
  `resolve.nodes` and flags ANY workspace-member dep (name-agnostic — catches
  neutral-named product crates like `net-guard` and indirect deps regardless
  of declaration form).
- `crates/lattice-system-harness/src/world.rs`: OS-assigned ephemeral port
  via `TcpListener::bind("127.0.0.1:0")`; bind error propagated as
  `WorldError` (no silent port-0 fallback).
- `crates/lattice-system-harness/src/driver.rs`: `from_env` canonicalizes the
  binary to an absolute path (resolved in the test process); `run` enforces
  `LatticeCommand.timeout` via `try_wait` polling with concurrent
  stdout/stderr drain threads and `DriverError::Timeout`; `help`/`bare_invoke`
  take `&ScenarioSpec` and use `spec.step_timeout()`.
- `crates/lattice-system-harness/src/scenario.rs`: removed the speculative
  `total_timeout_secs`/`total_timeout()` (no consumer); kept
  `step_timeout_secs`/`step_timeout()`.
- `crates/lattice-system-harness/src/artifacts.rs` + `assert.rs`: `command_summary`
  redacted; `redact_env_pair` masks secret-name env vars as `[REDACTED-ENV]`
  and substring-redacts others; `assert_with_failure_artifacts` persists
  `stdout.txt`/`stderr.txt`/`command.txt`/`world.txt` under
  `$LATTICE_HARNESS_FAILURE_DIR/<scenario>/` (default anchored to
  `CARGO_MANIFEST_DIR`, cwd-independent). Added `assert_stdout_contains_all`
  and `assert_exit_failure`; wired the semantic assertions in
  `fast_cli_help.rs` to artifact capture.
- `crates/lattice-system-harness/tests/fast_cli_help.rs`: 4 black-box tests
  updated to the new APIs; port assertion relaxed to `!= 0` (OS-assigned).
- `crates/lattice-system-harness/tests/scenarios/cli_help_surface.json`:
  dropped the removed `total_timeout_secs` field.
- `crates/lattice-system-harness/AGENTS.md`: verification section uses an
  absolute `LATTICE_BIN` and documents the real resolution mechanism.
- `scripts/ci-local.sh`: added `LATTICE_HARNESS_FAILURE_DIR` defaulting to
  `$(pwd)/target/harness-failures` (workspace-level collection).
- `docs/TESTING.md`: corrected crate count to 18; rewrote the `ci-local.sh`
  description to the actual 5 steps; documented the build-required harness
  exception, the `LATTICE_BIN` resolution mechanism, the GitHub Actions
  build+boundary steps, and the failure-artifact location.
- `CONTRIBUTING.md`: corrected crate count to 18; replaced the
  fmt/clippy/test "full local gate" with `scripts/ci-local.sh` (5 steps);
  updated the summary checklist.
- `.github/workflows/ci.yml`: added the binary build, boundary check, and
  `LATTICE_BIN` env steps.
- `Cargo.toml` / `Cargo.lock`: registered `lattice-system-harness` as a
  workspace member.

**Verification:**

- `scripts/system-harness-boundary-check.sh` — 3/3 groups PASS (tomllib
  manifest check; workspace-true; name-agnostic transitive metadata check).
- `cargo fmt --all --check` — clean.
- `cargo clippy -p lattice-system-harness --all-targets -- -D warnings` —
  clean.
- `cargo build -p lattice-cli --bin lattice` — ok.
- `LATTICE_BIN=$(pwd)/target/debug/lattice cargo test -p lattice-system-harness`
  — 4/4 harness tests pass.
- Synthetic probes confirmed the boundary check catches the previously
  bypassable forms: `guard = { package = "net-guard", path = '../net-guard' }`
  (single-quoted + neutral-name rename), `lattice-api = "0.1"` (string-form
  version dep), and transitive `net-guard` via the metadata backstop.
- `ci-local.sh` full run is green except for a pre-existing, unrelated
  `lattice-api` SQLite "disk I/O error" flake
  (`turn_runtime_constructs_one_adapter_per_turn`) that passes in isolation;
  no `lattice-api`/audit/SQLite files were touched in this change.

**Changed files:**

- `scripts/system-harness-boundary-check.sh` (new)
- `crates/lattice-system-harness/**` (new crate: AGENTS.md, Cargo.toml,
  src/{artifacts,assert,driver,lib,scenario,world}.rs,
  tests/fast_cli_help.rs, tests/scenarios/cli_help_surface.json)
- `scripts/ci-local.sh`
- `docs/TESTING.md`
- `CONTRIBUTING.md`
- `.github/workflows/ci.yml`
- `Cargo.toml` / `Cargo.lock`

---

## Codeberg #15: permanently retire `mutate.batch` — 2026-07-04

Severity: MEDIUM (product surface reduction; removes multi-operation partial-failure API).

**Problem:** `mutate.batch` applied create/edit operations in two non-transactional
phases. Mid-batch failures could leave prior edits and creates on disk while the
agent lost a single atomic multi-file mutation contract.

**Fix:**

- Removed `mutate.batch` from `lancet-catalog::CATALOG` (6→5 agent methods);
  deleted `Batch` / `BatchParams` / `BatchOperation` / `BatchResult` from
  `lancet-catalog::methods::workspace`.
- Removed native `handle_batch` and batch-only helpers from
  `lancet-native/src/handlers/mutation.rs`; `mutate.create` and `mutate.edit`
  remain registered.
- Removed authority/policy/audit branches for `mutate.batch` in
  `lancet-authority` (`policy/default.rs`, `policy/resolved.rs`) and
  `lancet-audit` provenance projector; updated native mutation preflight.
- Removed batch from `lattice-api` `CepExecutorAdapter::dispatch_typed`,
  profile catalog-method validation, effect-contract fixtures, warn harness, and
  streaming-delta tests.
- Dropped `FailureSemantics::PartialEffectsAudited` from `lancet-protocol`
  after confirming no live `EffectDecl` still referenced it (production
  catalog used `NoBackendEffectOnFailure` and `RetainDiffOnVerifyFailure`
  only; `mutate.batch` had used `RetainDiffOnVerifyFailure`, not
  `PartialEffectsAudited`).
- Updated docs (`AGENTS.md`, `README.md`, `docs/LANCET.md`, `docs/ARCHITECTURE.md`,
  `docs/CONFIGURATION.md`, `docs/lancet-warn-report.md`) and default agent
  enabled lists.

**Verification:**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `rg 'mutate\.batch|BatchOperation' --glob '*.{rs,toml,md}'` (excluding historical
  #27 spec notes and regression comments that assert absence)

---

## Codeberg #4: permanently retire command execution + post-mutation verify — 2026-06-30

Severity: HIGH (security surface reduction; permanent retirement of agent-visible command execution).

**Problem:** The CEP layer exposed `exec.command_run` as an agent-visible tool
and ran a post-mutation verify step after writes. This kept 1646 lines of shell
AST parsing and command classification in `lancet-authority`, tied verify to
command policy, and left an any-command-execution capability on the agent face.

**Fix:**

- Removed `exec.command_run` from `lancet-catalog::CATALOG` (9→8 methods);
  deleted `crates/lancet-catalog/src/methods/command.rs` and its registration.
- Deleted `crates/lancet-authority/src/policy/command.rs` (822 lines) and
  `policy/shell_ast.rs` (824 lines); removed `CommandPolicy`, `EffectiveImpact`,
  `classify_command`, `classify_command_line`, `build_effective_env`,
  `validate_timeout`, `CommandLine`, `ShellExpansion`, `resolve_command_line`
  and all re-exports from `lancet-authority`.
- Removed the `command` field from `CepPolicyConfig`, `PolicyConfig`,
  `CapabilitySourcePolicy`, `ExternalPolicyFile`, `ExternalCapabilitySourcePolicy`;
  removed the `exec.command_run` branch from `authorized_region_for` (now 2 args);
  removed `ResolvedCommand`, `resolved_command`, `command_error`,
  `PolicyEvidence.command_classification`, `PolicyEvidence.shell_expansion`,
  `PolicyResolver.command`.
- Deleted `crates/lancet-native/src/handlers/command.rs` (717 lines),
  `handlers/command_tests.rs` (1093 lines), `enforcement/verify.rs` (885 lines),
  and `executor/process.rs`; removed `register_native_command`.
- Removed `ExecutionBackend::exec` from the trait (10→9 methods); deleted
  `ExecRequest`, `ExecOutput`, `ExecRequestError`, `VerifyConfig`,
  `VerifyConfigError`, and `PathIntent::ExecCwd` from
  `lancet-native::execution_backend`; removed all `exec` impls from
  Null/Test/support backends.
- Removed `verify_config` from `HandlerContext`, `ServerRunConfig`,
  `LancetRuntimeConfig`; removed `verify_config_from_profile` and
  `command_policy_from_profile` from `lattice-api`; removed the `cep.verify_failed`
  event mapping and `AgentTurnEvent::CepVerifyFailed` / `TurnEvent::CepVerifyFailed`;
  removed `LifecycleFailureKind::{MissingVerifyConfig, VerifyFailed, VerifyTimeout}`.
- Removed `CepErrorCode::VerifyFailed` from `lancet-protocol`.
- Removed `LancetCommandSection` and `LancetVerifySection` from `lattice-profile`;
  removed `[lancet.command]` / `[lancet.verify]` from the profile schema
  (old profiles with these sections are now rejected by `deny_unknown_fields`).
- Removed `exec.command_run` from `lattice-profile::defaults::is_catalog_method`.
- Removed `CepVerifyFailed` rendering from `lattice-tui` and `lattice-cli`.
- Cleaned provenance projector + audit test fixtures that referenced
  `exec.command_run` as a method name.
- Removed the `exec` impl and `convert_intent` ExecCwd branch from
  `lattice-api/src/turn/workspace_backend.rs` (CEP-side bridge); the
  `lattice-workspace` `Backend::exec_upper`, `Workspace::exec`, and
  `PathIntent::ExecCwd` (workspace-side) were preserved as trusted-host plugin.
- Added regression tests: `catalog_retires_exec_command_run`,
  `resolve_command_run_errors_as_unknown_method`,
  `default_hook_denies_command_run_as_unknown_method`,
  `denies_retired_command_run_as_unknown_method`.

**Retention boundary (preserved, not deleted):**
`crates/lattice-workspace/src/promotion/**`, `git.rs`,
`Backend::exec_upper`, `Workspace::exec`, and the workspace-side
`PathIntent::ExecCwd`/`ExecRequest`/`ExecOutput` are trusted-host plugin
command capabilities below the CEP layer and were explicitly preserved.

**Verification:**

- `cargo check --workspace --all-targets` — pass (0 errors, 0 warnings).
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test -p lancet-catalog -p lancet-authority -p lancet-native -p lancet-api -p lattice-api -p lattice-profile` — pass.
- `rg "command_run|CommandPolicy|EffectiveImpact|classify_command|shell_ast|VerifyConfig|VerifyFailed|verify_config|emit_verify_failed|Run::NAME|PathIntent::ExecCwd|ExecRequest|ExecOutput|ExecRequestError" crates/lancet-* crates/lattice-api crates/lattice-tui crates/lattice-cli crates/lattice-profile crates/lattice-agent --type rust` — only intentional regression-test string references and `lattice_workspace::Exec*` (retention boundary) remain.
- `lattice-workspace::Backend::exec_upper` and `promotion/**` compile unaffected.

**Changed files:** `crates/lancet-catalog/` (catalog, methods, AGENTS.md),
`crates/lancet-authority/` (policy/command.rs, shell_ast.rs, default.rs,
resolved.rs, mod.rs, lib.rs, region.rs, tests), `crates/lancet-native/`
(execution_backend.rs, handlers/{mutation,mod}.rs, enforcement/{mod,preflight}.rs,
executor/mod.rs, server/{native_registry,registry,transport}.rs, lib.rs,
effect_contract_tests/*, tests/*), `crates/lancet-protocol/` (error.rs,
provenance.rs), `crates/lancet-audit/` (provenance/projector.rs, audit_tests.rs),
`crates/lancet-api/` (config.rs, runtime.rs, lib.rs), `crates/lattice-api/`
(turn/{cep,workspace_backend,types,lifecycle,live,collector,execute,cep_tests}.rs,
tests/*), `crates/lattice-profile/` (schema.rs, defaults.rs, lib.rs),
`crates/lattice-agent/` (agent_loop/types.rs), `crates/lattice-tui/` (runtime.rs),
`crates/lattice-cli/` (commands/run.rs), `AGENTS.md`, `FIX.md`.

---

## Codeberg #4 (follow-up): review cleanup — release-gate false-green, dead `brush-parser` dep, raw CEP -32601 regression, `PolicyReasonCode::CommandDenied` — 2026-06-30

Severity: MEDIUM (closes the open review items blocking issue #26; no behavior
change to retired methods — these are cleanup, regression-proof, and
dead-code removal).

**Problem:** The 2026-06-30 post-implementation review of issue #26 left three
P2 items plus one residual dead-code variant open before #26 could be closed:

1. **Release-gate false-green.** `scripts/release-gate-v1.4.sh` still ran the
   deleted `v1_4_command_artifact_dogfood` lane; Cargo exits 0 when a filter
   matches zero tests, so the gate falsely reported the dogfood proof as
   executed.
2. **Dead `brush-parser` dependency.** `shell_ast.rs` was the only consumer;
   after its deletion the dependency remained in the workspace and
   `lancet-authority` manifests, still compiling a retired shell parser and
   its transitive tree.
3. **Missing raw CEP `-32601` regression.** Existing tests proved
   `exec.command_run` is absent from the catalog and that the policy resolver
   returns `NonDeterminable`, but no test proved the *production* CEP registry
   rejects a raw `exec.command_run` call with JSON-RPC `Method not found`
   (-32601) rather than a policy/verify/execution result.
4. **`PolicyReasonCode::CommandDenied` dead code.** No production policy
   constructs it; it survived only via exhaustive `match` arms and tests. The
   review decided to delete it with **no wire-value compatibility** (the goal
   of #26 is permanent contraction of the CEP command interface; the
   `"command_denied"` wire value has no legitimate producer).

**Fix:**

- **Release gate (P2-1):** removed the `v1_4_command_artifact_dogfood` lane
  from `scripts/release-gate-v1.4.sh` (the following `cargo test -p lattice-api`
  lane already covers all remaining tests); changed
  `crates/lattice-cli/tests/release_smoke.rs` to assert the gate no longer
  references the retired lane; updated `docs/LANCET.md` drift notice, catalog
  table (9→8 methods, removed `exec.command_run` row), effect-contract table,
  and dogfood description.
- **`brush-parser` (P2-2):** removed the dependency from workspace `Cargo.toml`
  and `crates/lancet-authority/Cargo.toml`; `Cargo.lock` updated (dropped
  `brush-parser` plus its transitive `bon`/`cached`/`insta`/`pest`/`similar`
  tree — confirmed no other workspace member uses them via
  `cargo check --workspace --all-targets`).
- **Raw CEP -32601 regression (P2-3):** added
  `production_runtime_rejects_retired_exec_command_run_as_method_not_found` in
  `crates/lancet-api/src/runtime.rs`. It builds a production `LancetRuntime`
  (all 8 catalog methods enabled, so the call is not filtered before
  dispatch), sends a raw `exec.command_run` call via `CepClient::call_raw`,
  and asserts `wire_code() == -32601`, message starts with
  `Method not found:`, and `Retriability::Permanent`. This exercises the full
  production path: `LancetRuntime::build` → `LancetServer::spawn` → production
  registry → transport server loop → `handle_request` → `registry.dispatch`
  → client.
- **`PolicyReasonCode::CommandDenied` (P2-4, no compat):** deleted the variant
  from `lancet-protocol/src/policy.rs`; the variant-count test now asserts 8;
  the `guard.rs` CEP-error mapping branch now matches only `NetworkDenied`;
  deleted the `map_command_denied_to_guard_denied` unit test; corrected the
  stale `CommandDenied` comment in `guard_decide_fuzz.rs` (the
  `exec.command_run` deny input now exercises `NonDeterminable`, which is
  already listed). The old `"command_denied"` wire value is no longer
  deserializable, consistent with the permanent retirement goal.
- **Doctor hardening:** the LANCET doctor (`lancet-api/src/doctor.rs`) had a
  hard-coded `expected 9 methods` check, which now failed (catalog is 8). Added
  `lancet_catalog::CATALOG_LEN` (a `pub const` mirroring the static
  `[MethodDescriptor; 8]` array bound) and made the doctor read it, so the
  check cannot drift from the catalog again. Added
  `catalog_len_matches_runtime_catalog` regression test in
  `lancet-catalog/src/catalog.rs` to assert `CATALOG_LEN == catalog().len()`.
- **Docs drift:** updated `docs/ARCHITECTURE.md` (catalog table 9→8, removed
  `exec.command_run` from the method list, added a note about its retirement
  and the production-path -32601 regression test), `README.md` (8 methods,
  retirement note), and `crates/lancet-authority/AGENTS.md` (removed the
  deleted `src/policy/command.rs` reference; added a do-not-reintroduce note).

**Pre-existing gate blockers fixed in the same pass (out of #26 scope, but
required to run `scripts/release-gate-v1.4.sh` per the review):**

- `cargo fmt --all --check` had 287 diff lines at commit `691c0b67`; ran
  `cargo fmt --all` so the gate's step 1 passes (formatting-only changes
  across many pre-existing files).
- `crates/lattice-api/tests/upper_routing.rs` `microsandbox_lane` had an
  unused `use super::*;` (clippy `--all-features` failure); removed it.
- `crates/lattice-api/tests/promotion_wiring.rs` imported
  `lattice_workspace::MicrosandboxWorkspace` from the crate root, but the type
  lives at `lattice_workspace::microsandbox::MicrosandboxWorkspace`
  (compile failure under `--all-features`); corrected the path.
- `crates/lancet-catalog/src/catalog.rs`
  `scaffold_result_schema_includes_status_semantic_stage_and_routed` used the
  pre-1.0 `schemars` `schema.schema.object` API; migrated to the
  `serde_json::to_value(schema)` access pattern used by the other schema tests.

**Verification:**

- `cargo test -p lancet-catalog -p lancet-authority -p lancet-native -p lancet-api -p lattice-api -p lattice-profile -p lattice-cli` — pass (all targeted crate suites green, including the new production-path regression and the doctor test now reporting `catalog: 8 methods (expected 8)`).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass.
- `scripts/release-gate-v1.4.sh` — **PASSED** (all 10 steps: fmt, clippy `--all-features`, LANCET crate tests, lattice-agent, lattice-api dogfood suite, lattice-cli, leaf regressions, boundary check + LOC advisory, doctor, live-canary lane skipped).
- `rg "CommandDenied|command_denied"` — no matches (variant fully removed).
- `rg "brush-parser|brush_parser"` — no matches in source or manifests; only historical `FIX.md` mentions remain.

**Changed files:** `scripts/release-gate-v1.4.sh`,
`crates/lattice-cli/tests/release_smoke.rs`, `docs/LANCET.md`,
`docs/ARCHITECTURE.md`, `README.md`, `Cargo.toml`,
`crates/lancet-authority/Cargo.toml`, `Cargo.lock`,
`crates/lancet-api/src/runtime.rs`, `crates/lancet-api/src/doctor.rs`,
`crates/lancet-catalog/src/catalog.rs`, `crates/lancet-catalog/src/lib.rs`,
`crates/lancet-protocol/src/policy.rs`, `crates/lancet-authority/src/guard.rs`,
`crates/lancet-authority/tests/guard_decide_fuzz.rs`,
`crates/lancet-authority/AGENTS.md`, `crates/lattice-api/tests/upper_routing.rs`,
`crates/lattice-api/tests/promotion_wiring.rs`, `FIX.md`, plus
`cargo fmt --all` formatting-only changes across pre-existing files.

---

## Codeberg #3 / L-llm-coupling: extract neutral `net-guard` leaf — 2026-06-29

Severity: LOW (`architecture` / `crate-boundary`).

**Problem:** LANCET authority and native web execution reused
`lattice_llm::security`, leaving the otherwise independent LANCET stack with a
dependency on the LATTICE provider crate.

**Fix:**

- Added the Layer-0 `net-guard` crate with a five-item public interface:
  `NetGuardError`, `host_is_dns_rebinding_domain`,
  `is_private_or_reserved_ip`, `resolve_public_socket_addrs`, and
  `validate_base_url`. Its only non-`std` dependency is `url`.
- Moved the existing URL, private/reserved-address, rebinding-domain, and DNS
  guard implementation plus all 15 security regression tests into `net-guard`.
- Updated `lattice-llm` to map neutral config/DNS errors back to `LlmError`;
  updated `lancet-authority` and `lancet-native` to depend directly on
  `net-guard`; deleted `lattice-llm/src/security.rs`.
- Removed the open `L-llm-coupling` finding and updated workspace/crate
  architecture guidance.
- The required workspace clippy gate exposed pre-existing lint failures in
  SQLite-to-Git migration helpers, native test backends, and a candidate-ref
  integration test. Applied only clippy's behavior-preserving control-flow and
  type-alias rewrites so the mandated gate is green.

**Verification:**

- `cargo test -p net-guard -p lattice-llm -p lancet-authority -p lancet-native`
  — pass.
- `cargo test --workspace` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo tree -p net-guard --depth 1` — only `url`.
- `rg lattice_llm crates/lancet-*` — no matches.
- `rg 'lattice-llm' crates/lancet-*/Cargo.toml` — no matches.

**Changed files:** workspace manifests/lockfile; `crates/net-guard/**`;
`lattice-llm` client/security ownership; LANCET authority/native imports and
manifests; root and crate-local architecture/review records; clippy-only
migration/native/API test helpers.

---

## Phase 38 code review fixes: git session backend hardening — 2026-06-23

Severity: 1 CRITICAL, 8 WARNING, 3 INFO (all fixed via `--fix --all`).

Review artifact: `.planning/phases/38-git-session-backend/38-REVIEW.md` (gitignored).

Verified: `cargo build -p lattice-session -p lattice-api` ✓; `cargo test -p lattice-session` ✓ (169 lib + 4 promotion = 173 passed; 0 failed); `cargo test -p lattice-api` ✓ (117 lib + all integration suites green; 0 failed).

| # | Severity | Problem | Fix |
| --- | ---------- | --------- | ----- |
| CR-01 | CRITICAL | `commit_prepared_turn_impl` performed a non-atomic CAS check (read HEAD OID) followed by a force-update ref write (`repo.reference(..., force=true)`). A concurrent process could move the ref between check and write, and the force update would silently overwrite the other writer's commit. | Replaced the force update with an atomic compare-and-set: `find_reference` → re-verify `target() == head_oid` → `set_target` (fails if ref moved). First-commit path uses `reference(..., force=false)` and errors if the ref unexpectedly exists. Returns `SessionError::Conflict` on any movement. |
| WR-01 | WARNING | `messages_to_jsonl` used `unwrap_or_else( | _ | String::new())`, silently emitting empty lines on serialization failure;`parse_messages_jsonl` would then fail on readback, losing the original message. | Changed signature to `Result<String>` and propagate `serde_json::to_string` errors as `SessionError::Git`. Updated the single caller in `commit_prepared_turn_impl`. |
| WR-02 | WARNING | `collect_unique_commits` did `let _ = revwalk.hide(oid);`, silently discarding hide failures. A failed hide would include merge-base ancestors in the diff, producing incorrect `only_in_from`/`only_in_to`. | Propagate `revwalk.hide(oid)` errors as `SessionError::Git`. |
| WR-03 | WARNING | `resolve_session_id_for_commit` did `let _ = revwalk.push(tip);`, silently discarding push failures. A failed push would yield an empty revwalk, returning `Ok(None)` for commits that exist. | Changed to `if revwalk.push(tip).is_err() { continue; }` — the scan is best-effort across sessions, so we skip the session on push failure rather than silently producing a wrong empty result. |
| WR-04 | WARNING | `session_ops_path` joined `commit_oid` directly into a filesystem path without validating it. Safety depended on upstream `git2::Oid::from_str` rejection, but `write_ops_state`/`read_ops_state` call it directly. | Added defense-in-depth validation: reject `commit_oid` that is empty or contains non-hex characters. |
| WR-05 | WARNING | `get_turn_commit_impl` took `_session_id: &str` but never used it. A commit from session A would be returned when queried with session B's ID, violating session isolation. | After parsing the manifest, compare `manifest.session_id != session_id` and return `Ok(None)` on mismatch. |
| WR-06 | WARNING | `diff_impl` used `repo.merge_base(...).ok()`, converting all git errors (not just "no common ancestor") to `None`. A transient git error would produce incorrect diff results instead of propagating. | Match on `git2::ErrorCode::NotFound` → `None`; all other errors propagate as `SessionError::Git`. |
| WR-07 | WARNING | `turn_commit_from_manifest_and_oid` set `profile_snapshot_json: String::new()`. The full profile JSON is stored as `turn/profile.json` in the commit tree but never read back, so consumers relying on the field (replay, audit) got an empty string. | `get_turn_commit_impl` now reads the `turn/profile.json` blob and populates `profile_snapshot_json` with the real payload. |
| WR-08 | WARNING | `atomic_write` used a deterministic temp path (`path.with_extension("tmp")`). Concurrent writers to the same target would collide on the same temp file, corrupting data. | Temp file name now includes PID + a per-process atomic counter: `.{file_name}.{pid}.{seq}.tmp`, making concurrent writes non-colliding while staying deterministic per-process. |
| IN-01 | INFO | `enumerate_session_branches` set `updated_at: Utc::now()` — the enumeration time, not the ref's last-update time. Misleading for callers expecting the actual last-update timestamp. | Documented in a comment that `updated_at` reflects enumeration time, not ref update time; callers needing the true last-update time should read the commit's committer timestamp via `commit_id`. |
| IN-02 | INFO | `SessionApi::commit_prepared_turn` did not call `resolve_id` while `commit_turn` did. An abbreviated session ID in a prepared commit would fail with `NotFound` from the store. | Documented in the doc comment that prepared commits must carry the canonical session ID; this is intentional because prepared commits are built internally after session resolution. |
| IN-03 | INFO | `read_session_meta` and `read_ops_state` mapped JSON parse errors to `SessionError::Git(...)`, a semantic mismatch — sidecar JSON corruption is not a git error. | Changed both to use `SessionError::InvalidPersistedValue { field, value }`. |

Files: `crates/lattice-session/src/git_graph/{commit,helpers,refs,session}.rs`, `crates/lattice-api/src/session.rs`.

---

## 37-05: Remove legacy overlay and envelope authority concepts — 2026-06-14

Severity: MEDIUM (architectural simplification; phase 37-overlay-and-envelope-removal).

**Problem:** The codebase carried legacy `AuthorityMode` (Direct/Envelope), `Capability` enums,
`SourcedCapability`, `CapabilitySource`, `ExecutionEnvelope`, `BaseExecutionEnvelope`,
`EffectiveEnvelopeSnapshot`, envelope composition, and `ActiveEnvelopeStore` — all of which
were no longer needed in the direct-authority-only runtime.

**Fix:**

- **Task 1** (`da1b174`): Deleted `AuthorityMode`, `Capability`, `SourcedCapability`,
  `CapabilitySource`, `EffectiveEnvelopeSnapshot`, `ExecutionEnvelope`, `BaseExecutionEnvelope`,
  `Constraints`, `Intent`, `Scope`, `Impact`, `ImpactConfidence`, `OnFailure`, `VerifyCommand`
  from `lancet-protocol/src/policy.rs`. Simplified `PolicyAuthorityInput` to a unit struct.
- **Task 2** (`da1b174`): Deleted `lancet-authority/src/envelope_store.rs`. Rewrote
  `lancet-authority/src/policy/capability.rs` to keep only `InvocationFacts` and
  `SessionStateOperation`. Simplified `policy/default.rs` (removed `decide_resolved_envelope`,
  `allowed_outcome_envelope`, `capability_failure_hint`, `map_cap_action`). Updated
  `guard.rs`, `effect/region.rs`, `effect/checker.rs`, `effect/mod.rs`, `policy/resolved.rs`,
  `lib.rs`. Updated test files (`direct_policy.rs`, `policy_coverage.rs`, `guard_decide_fuzz.rs`).
- **Task 3** (`0871770`): Removed `base_envelope`, `active_envelope`, `authority_mode` from
  `HandlerContext` and `ServerRunConfig`. Updated `transport.rs`, `preflight.rs`, `mutation.rs`
  (simplified `MutationAuthorization` to direct-only), `verify.rs` (removed
  `extract_verify_command`), `command.rs`. Updated effect_contract_tests fixtures and support.
- **Task 4** (`2a9feef`): Removed `LegacyEnvelopeGate` and `AuthorityModeRejection` from
  `RejectionStage` enum in `lancet-audit`.
- **Task 5** (`df48601`): Removed `authority_mode` from `LancetRuntimeConfig`. Removed
  `base_envelope`/`active_envelope` from `LancetRuntime::build`. Removed
  `from_catalog_enabled_for_authority_mode` from `ToolExposure`. Removed `AuthorityMode`
  re-exports. Updated `lattice-api/turn/cep.rs` to remove `AuthorityMode` usage.
- Integration test fixes (`104d15d`, `a3c55f`): Updated test imports and removed
  envelope-specific test code across `lattice-api` integration tests.

**Verification:**

- `cargo build --workspace` — passes (2 pre-existing warnings in lancet-native).
- `cargo test -p lancet-protocol -p lancet-cep -p lancet-audit -p lancet-authority -p lancet-api --lib` — all pass.
- `lancet-native` lib compiles; test compilation has pre-existing Wave 7 errors (supervisor/shell).
- `lattice-api` integration tests were already not compiling before this change (pre-existing).

**Changed files:** 25+ files across `lancet-protocol`, `lancet-authority`, `lancet-audit`,
`lancet-cep`, `lancet-api`, `lancet-native`, `lattice-api`.

---

## M-33-05: git-ready warm template replaces per-Upper apt-get install git — 2026-06-14

Severity: MEDIUM (performance + correctness; gap-closure Plan 33-06).

**Problem:** `MicrosandboxWorkspace::deliver_baseline` ran `apt-get install git`
inside every Upper because the base image (`python:3.12-slim`) lacks git. Two
consequences blocked WS-04 closure: (1) every Upper boot paid the apt-get cost,
making the production contract suite `test_microsandbox_contract_suite` time out
at >595s; (2) apt-get needs network, so under `EgressPolicy::Blocked` (EGRESS-01)
git could not be installed and the git-init sequence was silently skipped,
leaving Blocked-egress Uppers without a `/workspace` `.git` root.

**Fix:**

- Added `MicrosandboxWorkspace::build_git_ready_template(&self, base_image, name)`:
  bakes git into a reusable per-project snapshot once (create → `apt-get` via the
  fixed `APT_GET_GIT` literal through an `exec_with` argv slice → verify
  `git --version` → `stop()` →
  `Snapshot::builder(name).destination(SnapshotDestination::Name(name)).create()`).
  T-33-04: never calls `.env()` or `inject_file_credentials`, so no resolved
  secret is baked into the template. T-33-05: the apt-get command is a
  compile-time constant, never `sandbox.shell()`.
- Hardened `deliver_baseline`: `ImageOrTemplate::Template` + missing git now
  returns an error (`git-ready template is missing git; rebuild with
  build_git_ready_template`) instead of silently producing a git-less Upper.
  `ImageOrTemplate::Image` + Open/Allowlist still installs git once via apt-get
  (then errors if absent); `ImageOrTemplate::Image` + Blocked delivers files only
  (exec-only Upper). Removed the old unconditional swallow-apt-get-failure skip.
- Added `WarmTemplate::git_ready_snapshot_name_for_project` naming helper.
- Refactored the contract suite into `run_suite_with<W, F>(ws, make_spec)` +
  `minimal_upper_spec_with(...)`; `run_suite` stays a wrapper so the deterministic
  lane is unchanged. Rewrote `test_microsandbox_contract_suite` to build a
  git-ready template and run the suite from `ImageOrTemplate::Template` under
  `EgressPolicy::Blocked`; added `test_git_ready_template_has_git_under_blocked_egress`.

Scope note (D-33-02, Phase 34): automatic per-project template selection on the
`create_upper` `Image` path is still not wired — callers must pass
`ImageOrTemplate::Template` explicitly. WS-04 is satisfied for the explicit
git-ready-template path; auto-selection remains deferred.

Verification: `cargo check -p lattice-workspace` exits 0; `cargo test -p
lattice-workspace` (deterministic lane, no KVM, 23 tests) exits 0; `cargo clippy
-p lattice-workspace --features=integration_tests --all-targets -- -D warnings`
compiles + lints the KVM integration tests clean. KVM execution command (run on a
KVM host): `cargo test -p lattice-workspace --features=integration_tests
test_microsandbox_contract_suite test_git_ready_template_has_git_under_blocked_egress`.

Live KVM verification (2026-06-14, libkrun host) — both integration tests PASS:
`test_microsandbox_contract_suite` 222s (down from the prior >595s timeout; one
template build + 6 reuses, zero per-Upper apt-get). `test_git_ready_template_has_git_under_blocked_egress`
188s; `git rev-parse --show-toplevel` exits 0 under `EgressPolicy::Blocked` with
git sourced from the template (apt-get never runs). Verification found and fixed
one test-wiring bug (`6e96505`): the standalone test used `restore_upper` (a raw
CoW restore that delivers no baseline → leaves the Upper without `/workspace`,
making `exec_with(...).cwd("/workspace")` spawn ENOENT); changed to
`create_upper(ImageOrTemplate::Template)` (from_snapshot + deliver_baseline), the
correct per-turn path for a git-ready base template, matching the contract suite.

Files: `crates/lattice-workspace/src/microsandbox.rs`,
`crates/lattice-workspace/src/template.rs`,
`crates/lattice-workspace/src/testing/contract_tests.rs`,
`crates/lattice-workspace/tests/integration_microsandbox.rs`, `FINDINGS.md`,
`.planning/phases/33-workspace-adapter-production-hardening/33-VERIFICATION.md`.

---

## LANCET preflight/evidence audit metadata — 2026-06-07

Verified: `cargo test -p lancet-native guard_l1` ✓, `cargo test -p lancet-native command` ✓, `cargo test -p lancet-native mutation` ✓, `cargo test -p lancet-native audit_` ✓, `cargo test -p lancet-audit preflight_and_policy_metadata_project_to_activity_attributes` ✓, `cargo test -p lancet-authority effect` ✓, `scripts/lancet-boundary-check.sh` ✓, `scripts/release-gate-v1.4.sh` ✓ (`release-gate-v1.4.sh PASSED`).

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| LANCET-EVIDENCE-01 | MEDIUM | Policy, command, mutation, and approval evidence was split across events and ad hoc durable metadata. Some failure paths, especially policy-level replan and HumanReview, did not expose an approval-ready or preflight-shaped audit record before the backend stayed uninvoked. | Added native `cep_policy`, `cep_preflight`, and `approval_request` durable metadata. Policy evaluation now records capability-match/effect/authorized-region summaries; HumanReview emits `cep.approval_required` without auto-approval; command preflight records effective impact, effect kinds, envelope/allowlist/write-region evidence; mutation preflight records changed paths, out-of-scope paths, expected-hash status, verify configuration, and replan reason, including early policy denials. Audit provenance projection now exposes these fields in PROV activity attributes for `observe.audit` and `observe.audit_replay`. |

Files: `crates/lancet-native/src/enforcement/{guard_l1,preflight}.rs`, `crates/lancet-native/src/handlers/{command,mutation,audit}.rs`, `crates/lancet-audit/src/provenance/projector.rs`, `docs/LANCET.md`.

Deferred: MCP/dynamic external tool routing, interactive approval cache/UI, and lattice outer-loop orchestration remain outside this LANCET-core tranche.

---

## LANCET explicit-envelope documentation drift cleanup — 2026-06-07

Verified: `rg -n --glob '*.md' --glob 'AGENTS.md' --glob 'CLAUDE.md' --glob '!FIX.md' --glob '!docs/v1.3-alpha-walkthrough.md' "impact analysis|invariant checks|optional invariant|derived intent|lancet-invariants|tree-sitter|guard_l2|derive_intent|submit_intent|ImpactAnalyzer|AnalysisResult|9-method" .` ✓ (no current-doc hits).

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| DOC-24-01 | LOW | Current LANCET docs still described removed semantic layers and stale catalog shape: `session.envelope` mentioned derived intent, native ownership mentioned impact analysis layers, handler flow still mentioned invariant checks, and catalog guidance still called the production surface 9-method. | Updated current architecture guidance to match the explicit-envelope runtime: `session.envelope` is declare-only, `lancet-native` owns effect-contract enforcement instead of impact analysis, mutation/native execution checks are policy, Guard L1, effect, audit, and optional verify, and catalog docs now say 10-method. |

Files: `docs/LANCET.md`, `AGENTS.md`, `crates/lancet-native/AGENTS.md`, `crates/lancet-catalog/AGENTS.md`, `benchmarks/legacy-maintenance/README.md`.

---

## M-24-04: separate CEP exposure and base authority with `lancet-cep` — 2026-06-06

Verified: `cargo fmt --all --check` ✓, `cargo test -p lancet-catalog` ✓ (`25 passed`), `cargo test -p lancet-cep` ✓ (`10 passed`), `cargo test -p lancet-api` ✓ (`2 passed`), `cargo test -p lancet-native --lib` ✓ (`350 passed; 6 ignored`), `cargo test -p lattice-api` ✓ (`79 passed` lib + all integration suites green), `cargo clippy --workspace --all-targets -- -D warnings` ✓, `cargo tree -p lancet-cep --edges normal` ✓ (protocol/catalog/dispatch only), `cargo tree -p lancet-native --edges normal --depth 1` ✓ (no `lancet-cep` dependency), `git diff --check` ✓.

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| M-24-04 | MEDIUM | `tools.enabled` was doing two jobs: filtering catalog-derived LLM tool exposure and shaping parts of the CEP base envelope (`MutateSessionState`, `FetchUrl`, `ReadAudit`). That made tool exposure look like an authority boundary. | Added `lancet-cep` as the public CEP facade over protocol/catalog/dispatch. `ToolDefinition` and `ToolExposure` now live there. `MethodDescriptor::base_authority` declares method-local base requirements in `lancet-catalog`, `ToolExposure` aggregates them, and `BaseAuthorityPlan` materializes session/fetch/audit base capabilities while rejecting workspace write or command authority. `lancet-api` consumes the descriptor-derived plan instead of hard-coded method-name branches, `lancet-native` no longer carries a projection shim, and `lattice-api` converts CEP tool definitions to provider tool definitions only at the LATTICE↔LANCET seam. |

Notes: `lattice-llm` was intentionally not changed to depend on `lancet-cep`; that would make the Layer-0 provider crate depend upward on the CEP facade. The remaining LANCET→LATTICE coupling is URL/DNS security reuse and remains tracked in `FINDINGS.md`.

Files: `crates/lancet-cep/` (new), `Cargo.toml`, `Cargo.lock`, `crates/lancet-catalog/src/`, `crates/lancet-api/`, `crates/lancet-native/src/lib.rs`, `crates/lattice-api/src/turn/cep.rs`, `crates/lattice-api/src/turn/cep_tests.rs`, `crates/lattice-api/tests/cep_round_trip.rs`, `AGENTS.md`, crate-local `AGENTS.md`, `docs/LANCET.md`, `FINDINGS.md`.

---

## M-24-05: extract LANCET runtime composition into `lancet-api` crate — 2026-06-06

Verified: `cargo fmt --all --check` ✓, `cargo clippy --workspace --all-targets -- -D warnings` ✓, `cargo test --workspace` ✓ (46 suites, 0 failed), `cargo test -p lancet-api` ✓ (`2 passed`), `cargo test -p lattice-api` ✓ (`79 passed` lib + all integration suites green), `cargo test -p lattice-cli` ✓.

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| M-24-05 | MEDIUM | `lattice-api` claimed to be a thin facade but had absorbed the entire turn-scoped LANCET runtime composition (`turn/cep.rs` ~650 lines: native registry, dispatch server/client, profile→CEP policy lowering, base envelope derivation, audit store, invariant index, attestation, server lifecycle). The same registry/transport/server spawn was also hand-rolled in `lattice-cli` `lancet` and `audit` commands — three copies. There was no way to drive LANCET without pulling in `lattice-agent`/`lattice-session`. | Added a new `lancet-api` crate (Layer 3.5) that owns LANCET runtime composition independent of LATTICE: `LancetRuntime::build` (full turn runtime), `LancetServer::spawn` (low-level registry+transport+server launcher), `ServerLifecycle`, `LancetRuntimeConfig`/`AuditBacking`, `store_turn_attestation`, `base_observation_envelope`, and curated re-exports. It takes LANCET-native policy types only (`PolicyConfig`, `CommandIsolationRuntimeConfig`, `CepMode`) — no `lattice-profile` dependency. `lattice-api/turn/cep.rs` is now just the LATTICE↔LANCET seam (profile→policy translation, `CepExecutorAdapter`, CEP-event→`TurnEvent` mapping) and delegates composition to `LancetRuntime::build`. Both CLI spawn sites now route through `LancetServer::spawn`, collapsing the three duplicates to one. `lattice-api` dropped its direct `lancet-invariants` dependency. |

Notes: `CepExecutorAdapter`, the profile→policy lowering, and event mapping intentionally stay in `lattice-api` because they depend on `lattice-agent`/`lattice-profile` types — they are the seam, not LANCET composition. The M-24-05 boundary drift is corrected: `lattice-api` is again a facade + seam, not a runtime aggregator.

Files: `Cargo.toml`, `crates/lancet-api/` (new: `Cargo.toml`, `src/{lib,error,config,runtime,attestation,tools}.rs`, `AGENTS.md`, `CLAUDE.md`), `crates/lattice-api/Cargo.toml`, `crates/lattice-api/src/turn/{cep,cep_tests,live,live_tests,mod}.rs` (removed `enabled_tools.rs`), `crates/lattice-cli/Cargo.toml`, `crates/lattice-cli/src/commands/{lancet,audit}.rs`, `AGENTS.md`, `crates/lattice-api/AGENTS.md`, `FINDINGS.md`.

---

## FINDINGS/INFO actionable backlog fixes — 2026-06-05

Verified: `cargo fmt --all --check` ✓, `cargo test -p lancet-protocol` ✓ (`86 passed; 0 failed`), `cargo test -p lancet-native --lib` ✓ (`357 passed; 0 failed; 6 ignored`), `cargo test -p lancet-native commit_baseline` ✓, `cargo test -p lancet-native artifact_commit_blocks_cleanup_failures_before_lower_recheck` ✓, `cargo test -p lancet-native artifact_overlay_commit_flow_cleanup_failure_blocks_commit` ✓, `cargo clippy -p lancet-native --all-targets -- -D warnings` ✓, `scripts/lancet-boundary-check.sh` ✓, `scripts/release-gate-v1.4.sh` ✓ (`release-gate-v1.4.sh PASSED`)

| # | Severity | Problem | Fix |
| --- | ---------- | --------- | ----- |
| M-24-01 | MEDIUM | Review records and crate navigation still described `FetchUrl { host_pattern }` even though the current protocol already uses explicit `FetchUrl { host }`. | Kept the current wire shape, removed the stale open finding, and updated protocol/native navigation to describe the current `host` semantics without introducing a protocol rename. |
| M-24-02 | MEDIUM | Workspace snapshot and lower-baseline capture walked the tree synchronously on the async command path and had no explicit entry/depth ceiling. | Added crate-private `SnapshotLimits` with fail-closed entry/depth checks, preserved sync helpers for pure sync paths/tests, and moved command-path snapshot, lower-baseline, lower-fingerprint, diagnostic consistency, and artifact commit sections through `spawn_blocking`. |
| M-24-03 | MEDIUM | Hidden `ToolLimits` carried unrelated defaults for shell timeout and patch output size. | Removed `executor::limits::ToolLimits`, made shell `ExecuteConfig.timeout_ms` a required validated `u64`, and moved patch write-size enforcement into focused `PatchApplyLimits`. |
| L-30-01 | LOW | Overlay cleanup metadata preserved only the first cleanup failure, losing compact diagnostics when multiple cleanup stages failed. | Kept the first-failure metadata fields for compatibility and added `cleanup_failures` entries with `{ kind, path_label, reason }` for every recorded cleanup failure. |
| L-31-01 | LOW | Successful artifact commit baseline audits with hunk evidence still emitted empty `commit_baseline_details`, hiding positive anchor evidence. | Successful and partial artifact reports now include compact `commit_baseline_details.hunks` when hunk evidence exists, while no-evidence successful paths remain `{}`. |

Residual: `M-24-04`, `M-24-05`, `L-20`-`L-23`, `L-29-*`, and `L-31-02` remain tracked in `FINDINGS.md` for later architecture/performance windows. `INFO.md` items are explicitly reviewed and retained as informational, not upgraded to bugs.

Files: `FINDINGS.md`, `INFO.md`, `crates/lancet-protocol/AGENTS.md`, `crates/lancet-native/AGENTS.md`, `crates/lancet-native/src/edit/apply.rs`, `crates/lancet-native/src/enforcement/verify.rs`, `crates/lancet-native/src/executor/mod.rs`, `crates/lancet-native/src/executor/shell.rs`, `crates/lancet-native/src/executor/supervisor/artifact.rs`, `crates/lancet-native/src/executor/supervisor/overlay.rs`, `crates/lancet-native/src/executor/supervisor/snapshot.rs`, `crates/lancet-native/src/handlers/command.rs`, `crates/lancet-native/src/handlers/command_tests.rs`, `crates/lancet-native/src/handlers/mutation.rs`

---

## Phase 31 code review fixes: artifact commit baseline boundary — 2026-06-05

Verified: `cargo fmt --all --check` ✓, `git diff --check` ✓, `cargo test -p lancet-native artifact_commit_delete_refuses_symlink_parent_and_preserves_outside_file -- --nocapture` ✓, `cargo test -p lancet-native commit_baseline_artifact_report_uses_captured_baseline_bytes_before_lower_recheck -- --nocapture` ✓, `cargo test -p lancet-native commit_baseline_artifact_report -- --nocapture` ✓, `cargo test -p lancet-native artifact_overlay_commit_flow -- --nocapture` ✓, `cargo test -p lancet-native command_boundary -- --nocapture` ✓, `cargo test -p lancet-native --lib -- --nocapture` ✓ (`351 passed; 0 failed; 6 ignored`)

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| P31-CR-01 | CRITICAL | Artifact commit delete paths rejected symlink final targets but did not reject existing symlink parents before deletion, allowing a delete manifest path to remove a file outside the project root. | `apply_delete_entry` now rejects existing symlink parents before deleting the target. Added a regression that preserves an outside file behind a symlinked parent. |
| P31-WR-02 | MEDIUM | Commit baseline report generation re-read anchor files from the live workspace after lower-baseline capture, so workspace edits before lower recheck could contaminate pre-verify anchor validation details. | `LowerBaseline` now stores captured regular file bytes, and commit-baseline report generation reads anchor files from that captured lower baseline. Added a regression that mutates the source before lower recheck and still blocks commit as lower drift. |

Files: `crates/lancet-native/src/executor/supervisor/artifact.rs`, `crates/lancet-native/src/executor/supervisor/snapshot.rs`, `crates/lancet-native/AGENTS.md`

---

## Phase 31 plan review fixes: validation mapping and artifact scope — 2026-06-05

Verified: validation matrix task/plan/wave check ✓ (`validation_rows=14 plan_tasks=14 status=ok`), `gsd-tools.cjs query check.decision-coverage-plan ...` ✓ (`25/25` decisions covered), Phase 31 requirement coverage check ✓ (`ANC-01..ANC-04`, `AUD-01..AUD-03` covered), artifact section noise scan ✓ (no phase-wide inventory wording remains)

| # | Severity | Problem | Fix |
| --- | ---------- | --------- | ----- |
| P31-PLAN-01 | MEDIUM | `31-VALIDATION.md` task IDs and plan/wave columns were shifted from the actual seven-plan structure, causing native HashLine tests, protocol observed tests, PROV tests, and CLI tests to appear under the wrong plans. | Rebuilt the validation matrix as 14 rows matching the real `31-01-01` through `31-07-02` task IDs, plan numbers, waves, requirements, and automated commands. |
| P31-PLAN-02 | LOW | `wave_0_complete=false` looked like missing test infrastructure even though Cargo test is already present. | Kept `wave_0_complete=false` because Phase 31's specific regression tests are still created during execution, but clarified that Cargo infrastructure exists and checked the infrastructure/sign-off items that are already true. |
| P31-PLAN-03 | LOW | Each PLAN.md repeated a phase-wide artifact inventory, making it unclear which symbols a given executor should create. | Narrowed each plan's artifact block to "Produced by this plan" plus explicit consumes/handoff notes for artifacts owned by earlier or later plans. |

Files: `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-VALIDATION.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-01-PLAN.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-02-PLAN.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-03-PLAN.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-04-PLAN.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-05-PLAN.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-06-PLAN.md`, `.planning/phases/31-hashline-commit-boundary-and-audit-integration/31-07-PLAN.md`

---

## Phase 30 Wave 6 gate fixes: artifact overlay terminal semantics — 2026-06-04

Verified: `cargo test -p lancet-native overlay_cleanup_report_is_returned_on_nonzero_exit -- --nocapture` ✓, `cargo test -p lancet-native artifact_overlay_cleanup_is_deferred_on_nonzero_exit -- --nocapture` ✓, `cargo test -p lancet-native workspace_mutation_timeout_checks_out_of_scope_changes -- --nocapture` ✓, `cargo test -p lancet-native command_boundary -- --nocapture` ✓, `cargo test -p lancet-native artifact_overlay_commit_flow -- --nocapture` ✓, `cargo test -p lancet-native verify_baseline_exclusion -- --nocapture` ✓, `cargo test -p lancet-native artifact_verify -- --nocapture` ✓, `cargo build` ✓, `cargo test` ✓

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| P30-W6-01 | HIGH | WorkspaceMutation mode-off fallback returned `CommandTimeout` before doing the post-failure workspace diff, so out-of-scope writes made by a timed-out command were not promoted to `ReplanRequired`. | `artifact_fallback_without_overlay` now records and checks the workspace mutation diff on execution errors before returning the original error, preserving out-of-scope `ReplanRequired` behavior and ledger effects. |
| P30-W6-02 | MEDIUM | An executor test still expected Artifact overlay cleanup to happen on the executor terminal path, but artifact overlays now defer cleanup to the command handler so upper content can be scanned and adjudicated first. | Updated the nonzero cleanup test to exercise Diagnostic cleanup and added explicit coverage that Artifact cleanup is deferred on nonzero exit. |

Files: `crates/lancet-native/src/handlers/command.rs`, `crates/lancet-native/src/executor/shell.rs`

---

## Phase 29 hardening: unprivileged userns preflight — 2026-06-04

Verified: `cargo test -p lancet-native unprivileged_userns -- --nocapture` ✓, `cargo test -p lancet-native user_namespace -- --nocapture` ✓, `cargo test -p lancet-native supervisor -- --nocapture` ✓, `LATTICE_LINUX_ISOLATION_LIVE=1 cargo test -p lancet-native linux_isolation_live_canary_suite -- --ignored --nocapture` ✓ (`passed=5 skipped_unsupported=0 failed=0`), `cargo test -p lancet-native` ✓

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| P29-HARD-01 | MEDIUM | The unprivileged namespace path relied on the later `CLONE_NEWUSER` and uid/gid map operations to reveal disabled rootless user namespaces, so setup failed safely but diagnostics did not distinguish obvious sysctl-disabled hosts up front. | Added a `check_unprivileged_user_namespace` supervisor preflight in `privilege_check` for the rootless strategy. It reads optional sysctls, fails closed when `kernel.unprivileged_userns_clone=0` or `user.max_user_namespaces=0`, ignores absent distro-specific sysctls, and still treats real `unshare`, `setgroups`, `uid_map`, and `gid_map` operations as authoritative. |

Files: `crates/lancet-native/src/executor/supervisor/syscall.rs`, `crates/lancet-native/src/executor/supervisor/linux.rs`, `docs/LANCET.md`

---

## Phase 29 live UAT fix: netns topology reads namespace-aware procfs — 2026-06-04

Verified: `cargo test -p lancet-native proc_net_ -- --nocapture` ✓, `cargo test -p lancet-native supervisor -- --nocapture` ✓, `LATTICE_LINUX_ISOLATION_LIVE=1 cargo test -p lancet-native linux_isolation_live_canary_suite -- --ignored --nocapture` ✓ (`passed=5 skipped_unsupported=0 failed=0`), `cargo test -p lancet-native` ✓

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| P29-LIVE-01 | HIGH | The live netns topology check read `/sys/class/net`, which can still reflect the host sysfs mount after `unshare --user --map-root-user --net`; this made a fresh no-egress netns look like it contained host veth devices. | Production and live-canary topology checks now read `/proc/net/dev` and `/proc/net/route`, require only `lo` plus no default route, and include parser regressions for loopback-only, non-loopback, missing-loopback, and default-route cases. |

Files: `crates/lancet-native/src/executor/supervisor/syscall.rs`, `crates/lancet-native/src/executor/supervisor/live_tests.rs`, `.planning/phases/29-isolation-supervisor/29-VERIFICATION.md`, `.planning/phases/29-isolation-supervisor/29-HUMAN-UAT.md`, `.planning/phases/29-isolation-supervisor/29-06-SUMMARY.md`, `.planning/STATE.md`

---

## Phase 29 verifier gap fixes: production isolation supervisor wiring — 2026-06-04

Verified: `cargo test -p lancet-native command_boundary -- --nocapture` ✓, `cargo test -p lancet-native isolation_supervisor -- --nocapture` ✓, `cargo test -p lancet-native shell -- --nocapture` ✓, `cargo test -p lancet-native fd -- --nocapture` ✓, `cargo test -p lancet-native mount -- --nocapture` ✓, `cargo test -p lancet-native network -- --nocapture` ✓, `cargo test -p lancet-native landlock -- --nocapture` ✓, `cargo test -p lancet-native projection -- --nocapture` ✓, `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✓

| # | Severity | Problem | Fix |
| --- | ---------- | --------- | ----- |
| P29-VG-01 | CRITICAL | Production `exec.command_run` still did not execute the child through the Linux supervisor after the evidence-honesty fix. | `handle_run_as` now calls `execute_argv_supervised` for isolated command plans. The child runs live supervisor setup in pre-exec, reports the setup attempt through a close-on-exec pipe, and parent-side metadata is built from that real report. |
| P29-VG-02 | CRITICAL | Live mount-view and inherited-authority cleanup stages had no production implementation, so filesystem/fd requirements could not be satisfied. | `LiveSupervisorSyscalls` now builds a per-child tmpfs root, bind-mounts `/project`, declared read-only toolchain paths, scratch tmpfs, and minimal devices, pivots away from the old root, verifies old-root cleanup, and runs fd cleanup while preserving only stdio plus the setup-report pipe. |
| P29-VG-03 | HIGH | Audit fallback and setup failures could not distinguish setup failure from backend command execution. | Supervised execution now returns setup attempts on success and failure. Setup failures record `backend_invoked=false` for enforce, while audit fallback records the failed setup attempt and then runs the unbounded backend with honest `unbounded` / `not_applied` boundary metadata. |

Files: `crates/lancet-native/src/handlers/command.rs`, `crates/lancet-native/src/handlers/command_tests.rs`, `crates/lancet-native/src/executor/shell.rs`, `crates/lancet-native/src/executor/supervisor/mod.rs`, `crates/lancet-native/src/executor/supervisor/syscall.rs`, `crates/lancet-native/src/executor/supervisor/live_tests.rs`, `docs/LANCET.md`

---

## Phase 29 review fixes: isolation supervisor evidence honesty — 2026-06-04

Verified: `cargo test -p lancet-native command_boundary` ✓, `cargo test -p lancet-native isolation_supervisor` ✓, `cargo test -p lancet-native shell` ✓, `cargo test -p lancet-native landlock_live_canary_requires_applied_status_to_pass` ✓, `cargo test -p lancet-native live_supervisor_unimplemented_evidence_stages_fail_closed` ✓, `cargo test -p lancet-authority boundary_credit` ✓, `cargo test -p lancet-native env` ✓, `cargo test -p lancet-native fd` ✓, `cargo test -p lancet-native mount` ✓, `cargo test -p lancet-native network` ✓, `cargo test -p lancet-native landlock` ✓, `cargo test -p lattice-cli lancet` ✓, `cargo test -p lattice-api cep_mode_integration` ✓, `cargo test -p lancet-audit effective_boundary` ✓, `cargo test -p lancet-catalog command` ✓, `cargo test -p lancet-native projection` ✓, `cargo fmt --all --check` ✓, `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✓, `cargo test --workspace` ✓, `LATTICE_LINUX_ISOLATION_LIVE=1 cargo test -p lancet-native -- --ignored --nocapture` ✓ (`passed=2 skipped_unsupported=3 failed=0`)

| # | Severity | Problem | Fix |
| --- | ---------- | --------- | ----- |
| P29-CR-01 | CRITICAL | Production `exec.command_run` used the `Ready` supervised handoff even though the hook only wrote `ready\n` and applied no Linux setup. | Production no longer calls the fake-ready handoff. Pending child-side setup is reported as `native_linux_pre_exec_pending`; required `enforce` boundaries fail closed before backend execution, while `audit` fallback runs with `unbounded` / `not_applied` metadata. Docs and phase summaries now avoid claiming applied production isolation without a real setup report. |
| P29-CR-02 | CRITICAL | `LiveSupervisorSyscalls` returned `Ok(())` for evidence-bearing no-op stages, allowing future wiring to mint applied boundary evidence without doing the work. | Unimplemented live stages now return errors, and the netns topology assertion performs a real no-veth/no-default-route check. Added regression coverage so those stages cannot silently report success before implementation. |
| P29-WR-01 | WARNING | The Landlock live canary passed when the helper printed `landlock_status=not_applied` and exited successfully. | The canary now passes only on `landlock_status=applied`; `not_applied` is classified as failure unless the case is explicitly unsupported. |

Files: `crates/lancet-native/src/handlers/command.rs`, `crates/lancet-native/src/handlers/command_tests.rs`, `crates/lancet-native/src/executor/shell.rs`, `crates/lancet-native/src/executor/supervisor/syscall.rs`, `crates/lancet-native/src/executor/supervisor/live_tests.rs`, `docs/LANCET.md`, `.planning/phases/29-isolation-supervisor/29-05-SUMMARY.md`, `.planning/phases/29-isolation-supervisor/29-06-SUMMARY.md`

---

## 技术债扫描修复 — 2026-05-27

Verified: `cargo fmt --all --check` ✓, `cargo clippy --workspace --all-targets -- -D warnings` ✓, `cargo test --workspace` ✓ (731 tests), `cargo test -p cep-executor` ✓ (121 tests; default sandbox blocked localhost bind, unrestricted local test passed), `cargo test -p lattice-hooks` ✓

| # | 严重度 | 问题 | 修复 |
| --- | -------- | ------ | ------ |
| TD-01 | MEDIUM | Workspace MSRV 声明 1.85，但锁定依赖需要 1.88+ | `rust-version` 升到 1.88；AGENTS 的 Rust 版本说明同步 |
| TD-02 | MEDIUM | `lattice-hooks` 存在于 `crates/` 但不是 workspace member，CI 不覆盖 | 将 `crates/lattice-hooks` 加回 workspace；`Cargo.lock` 纳入该 package |
| TD-03 | MEDIUM | CI clippy 未启用 `--all-targets` | CI 与 AGENTS 命令改为 `cargo clippy --workspace --all-targets -- -D warnings` |
| TD-04 | HIGH | `workspace.write_file` 只在写入前检查 symlink，存在检查到写入之间的 TOCTOU 窗口 | Unix 写入路径改为 `openat` + `O_NOFOLLOW`，逐级打开父目录并拒绝 symlink parent/leaf；补 leaf/parent symlink 回归测试 |
| TD-05 | HIGH | `validate_agents_dir` 在目标目录不存在时只做词法规范化，不解析已存在前缀中的 symlink | 改为 canonicalize 最深存在前缀后再拼接缺失后缀；拒绝 `..` components；补 symlink-prefix 回归测试 |
| TD-06 | LOW | `command.run` 安全扫描指出 shell 执行验证过薄 | 保持 argv 直传、无 shell interpolation；新增空 `argv[0]`、NUL byte、非法 env key/value 拒绝和回归测试 |

Residual: SQLite 单连接串行化与 `ChatRequest` 每轮完整 `Vec<Message>` clone 属架构/性能项，已留在 `FINDINGS.md` 继续跟踪；扫描里的 “command injection” 在当前 argv-only 设计下未复现为 shell 注入。

文件: `Cargo.toml`, `.github/workflows/ci.yml`, `AGENTS.md`, `crates/cep-executor/src/executor/fs.rs`, `crates/cep-executor/src/executor/shell.rs`, `crates/lattice-cli/src/commands/run.rs`

---

## Phase 19 review fixes: CEP core / JSON-RPC wire — 2026-05-24

Verified: `env TMPDIR=/home/astrin/Dev/lattice/target/tmp cargo test -p cep-executor` ✓ (44 tests + 1 doctest), `env TMPDIR=/home/astrin/Dev/lattice/target/tmp cargo test -p cep-protocol` ✓ (46 tests), `env TMPDIR=/home/astrin/Dev/lattice/target/tmp cargo test -p lattice-profile cep` ✓ (10 tests), `env TMPDIR=/home/astrin/Dev/lattice/target/tmp cargo clippy -p cep-executor -p cep-protocol -p lattice-profile --all-targets -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 |
| --- | -------- | ------ | ------ |
| P19-CR-01 | CRITICAL | `CepServer::run` 用 vector 截断追踪 cancellation sender，乱序完成时会丢掉仍在运行 handler 的 cancel handle | 改为按 call id 存储 `HashMap<u64, watch::Sender<bool>>`，task join 后只移除对应 id；新增乱序完成后 shutdown 仍能 cancel 剩余 handler 的回归测试 |
| P19-CR-02 | HIGH | `EventSink::emit` 在 bounded event channel 上 `send().await`，普通 `call()` 不 drain events 时会死锁 | progress event 改为 best-effort `try_send` drop-on-full；新增 buffer=1 且不消费 events 的 call 完成测试 |
| P19-CR-03 | MEDIUM | JSON-RPC helper / `wire_code()` / `to_jsonrpc_error()` 对标准错误码映射不一致 | `CepError` inherent helpers 统一复用 public constructors；新增 parse/method/params 标准 code 映射，`wire_code()` 与 `to_jsonrpc_error()` 共用同一映射函数 |
| P19-CR-04 | MEDIUM | compile-fail doctest 因无效 `ImpactKind::ReadOnly` 失败，未证明 catalog sealing | doctest 改用有效 `ImpactKind::WorkspaceObservation`，现在失败点落在缺失 `CatalogMethod` bound |
| P19-CR-05 | LOW | `[lancet]` flatten extension 静默接受标量 typo | `LancetSection` 改为自定义 deserialize，只保留 table-valued future sub-tables，拒绝 `cep.*` 标量 typo |

文件: `crates/cep-executor/src/transport.rs`, `crates/cep-executor/src/registry.rs`, `crates/cep-protocol/src/error.rs`, `crates/lattice-profile/src/schema.rs`

---

## M-21 修复: AgentLoop 增量维护 LLM messages — 2026-05-23

Verified: `cargo test -p lattice-agent --features test-utils` ✓, `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
| M-21 | MEDIUM | 每轮 tool loop 全量 `convert_to_llm_message` | turn 初建 `llm_messages`；tool round 仅 `append_llm_message` |

**Fixed:** repeated GraphMessage → `lattice_llm::Message` conversion on every LLM round.

**Residual:** `ChatRequest { messages: llm_messages.clone(), ... }` still clones the full provider message `Vec` each round because `ChatRequest` owns `Vec<Message>`. Eliminating that cost needs API-shape work (`Arc<[Message]>`, borrowed requests, builder reuse, etc.) — intentionally out of scope for M-21.

文件: `lattice-agent/src/agent_loop/mod.rs`, `messages.rs`, `tests/agent_loop.rs`

---

## M-11 修复: head_transcript 拓扑遍历 merge 父分支 — 2026-05-23

Verified: `cargo test -p lattice-session` ✓ (61 tests), `cargo clippy -p lattice-session -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
| M-11 | MEDIUM | `head_transcript_impl` 只走 first-parent，merge 侧消息丢失 | `collect_reachable_commits_topological`（迭代 postorder，`parents_json` 顺序）；linear / merge / diamond 测试 |

文件: `lattice-session/src/sqlite_graph/helpers.rs`, `commit.rs`, `tests/head_transcript.rs`

---

## H-06 修复: production `stream_turn` live streaming — 2026-05-23

Verified: `cargo test -p lattice-api` ✓, `cargo test -p lattice-api --test consistency` ✓, `cargo test -p lattice-cli run_integration` ✓

| # | 严重度 | 问题 | 修复 | 文件 |
|---|--------|------|------|------|
| H-06 | HIGH | production `stream_turn` 执行完整个 turn 后 replay buffered events，调用方无法获得渐进 token 推送 | Phase 16：`stream_turn` → `stream_turn_live`（spawn + bounded channel，边跑边 emit）；Phase 17 failure/consistency 覆盖；Phase 18 CLI `run` 切到 public `stream_turn`；Steps 1–4：`run_turn` 改为 drain live stream + `RunTurnCollector`，删除 `TurnDeps` / buffered replay seam | `lattice-api/src/turn/live.rs`, `execute.rs`, `collector.rs` |

Steps 1–4 明细见下方 **Turn execution unification** 节。

---

## M-19a/b 修复: live 背压不阻塞 agent / commit — 2026-05-23

Verified: `cargo test -p lattice-agent --features test-utils` ✓, `cargo test -p lattice-api stream_turn` ✓, `cargo clippy -p lattice-agent -p lattice-api --features test-utils --all-targets -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 | 文件 |
|---|--------|------|------|------|
| M-19a | MEDIUM | `emit_agent_event` 在满 channel 上 `send().await` 阻塞 `AgentLoop`，cancel/timeout 无法推进 | live mirror 改为 `try_send`；`buffered` 保持完整 `AgentTurnResult.events` | `lattice-agent/src/agent_loop/mod.rs` |
| M-19b | MEDIUM | `turn_tx.send().await` 阻塞 live task，无法 drain `agent_rx`，与 agent 侧背压双向僵局 | `ObserverSink`：delta `try_send`；非 delta 500ms 超时后放弃 observer，仍 drain 并 commit | `lattice-api/src/turn/live.rs` |

---

## M-20 修复: 批量 IN 查询分块 — 2026-05-23

Verified: `cargo test -p lattice-session` ✓ (58 tests), `cargo clippy -p lattice-session -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
| M-20 | MEDIUM | `IN (...)` 占位符数量随 message/commit ID 增长，可超出 SQLite `SQLITE_MAX_VARIABLE_NUMBER` 限制（默认 999） | 三处查询（`log_impl` tool_set、`commit_message_ids_batch`、`load_graph_messages_by_id`）均按 `SQLITE_BIND_CHUNK_SIZE`（900）分块查询后合并结果；新增 `chunked_queries` 测试模块覆盖 930 参数跨块场景 |

---

## v1.2 安全修复 — 2026-05-23

Verified: `cargo test -p lattice-llm -p lattice-tools` ✓, `cargo clippy -p lattice-llm -p lattice-tools -- -D warnings` ✓

### MEDIUM

| # | 问题 | 修复 | 文件 |
|---|------|------|------|
| M-04 | `validate_base_url` DNS 检查与 reqwest 请求之间存在 TOCTOU | 请求前钉 IP（**PARTIAL**，见 FINDINGS M-04） | `lattice-llm/src/security.rs`, `lattice-llm/src/client.rs` |
| M-05 | `web_fetch` 同 M-04；rebinding 黑名单不完整 | 钉 IP + 已知后缀黑名单（**PARTIAL**，见 FINDINGS M-05） | `lattice-tools/src/executor/web.rs` |

---

## v1.2 前瞻修复 — 2026-05-22

Verified: `cargo test -p lattice-session -p lattice-api -p lattice-cli -p lattice-profile -p lattice-llm` pass

### MEDIUM

| # | 问题 | 修复 | 文件 |
| --- | ------ | ------ | ------ |
| M-12 | `hash_json` 依赖 serde_json 序列化顺序 | 引入 `canonical_json` 递归排序 object keys | `lattice-session/src/graph.rs` |
| M-13 | profile `canonicalize` 失败时回退非规范路径 | 改为返回 `ProfileError::SystemPathNotFound` | `lattice-profile/src/load.rs` |
| M-15 | `map_session_error` 丢弃结构化错误信息 | 添加 Conflict/BadRequest/MissingCommit/Io/Database 映射 | `lattice-api/src/session.rs` |
| M-18-01 | release-gate.sh 缺 workspace crate | 添加 lattice-session/tools/profile/hooks | `scripts/release-gate.sh` |
| M-18-02 | `status_to_exit_code` 死代码 | 删除函数及 3 个遗留测试 | `lattice-cli/src/commands/run.rs` |

### LOW

| # | 问题 | 修复 | 文件 |
| --- | ------ | ------ | ------ |
| L-02 | `parse_json_data` 用 sentinel 做控制流 | 改为 `Result<Option<Value>, LlmEvent>`，语义清晰 | `lattice-llm/src/providers/mod.rs` + 调用方 |
| L-18-02 | TurnEvent 序列化 `.expect()` panic | 改为 `map_err` + `?` | `lattice-cli/src/commands/run.rs` |
| L-18-03 | 无 stream 超时机制 | CLI 加 5 分钟 idle timeout，超时写 stderr + exit 1；API/Agent durable deadline 留未来 | `lattice-cli/src/commands/run.rs` |

---

## v1.1 收口审查 — 2026-05-19

Verified: `cargo fmt --all --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` 427 pass ✓

### HIGH

| # | 问题 | 修复 | 分支 |
| --- | ------ | ------ | ------ |
| H-01 | thread-local SSE parser 跨请求状态泄漏 | 每次 `stream_sse_response` 创建独立 `ProviderParser` 实例 | llm-stream-sse-hardening |
| H-02 | `stream_sse_response` 整体下载（假流式 + OOM） | `response.bytes_stream()` 逐 chunk 解析，边读边检查累计大小 | llm-stream-sse-hardening |
| H-03 | `write_file` 跟随 symlink 绕过 project-root **(partial)** | 写入前 `symlink_metadata` 检测并拒绝；残留 TOCTOU 窗口留 v1.2 | tool-safety-hotfix |
| H-04 | `truncate_output` 非 UTF-8 字符边界 panic | `is_char_boundary` 循环回退到最近合法边界 | tool-safety-hotfix |
| H-05 | bash 命令不设 `current_dir` | Unix/非 Unix 分支均添加 `.current_dir(project_root)` | tool-safety-hotfix |
| H-07 | `tool_output_is_ok` 字符串前缀判断成功/失败 | 统一 `ToolExecutor` trait 为 `Result<String, String>`，删除前缀判定和 adapter | agent-loop-hardening |
| H-08 | agent loop 无迭代上限 | `MAX_TURN_ROUNDS = 200` hard cap + 测试 | agent-loop-hardening |
| H-09 | SQLite 每次新建连接 + TOCTOU 竞态 | `Mutex<Option<Connection>>` 持久化单连接 + lazy init + WAL | sqlite-consistency |
| H-10 | `validate_agents_dir` canonicalize 回退原始路径 **(partial)** | 手动按 `Component` 规范化，`..` 越根报错；symlink 残留留 v1.2 | tool-safety-hotfix |

### MEDIUM

| # | 问题 | 修复 | 分支 |
| --- | ------ | ------ | ------ |
| M-01 | OpenAI 硬编码 `stream: true` | 改为 `request.stream`，`stream_options` 条件附加 | llm-stream-sse-hardening |
| M-02 | Gemini 忽略 temperature/max_tokens | 添加 `generationConfig` 块 | llm-stream-sse-hardening |
| M-03 | `validate_base_url` 接受空字符串 | 改返回 `Err("base_url must not be empty")` | llm-stream-sse-hardening |
| M-06 | `grep` 不做前置 project-root 校验 | 入口处 canonicalize + `ensure_under_project_root` | tool-safety-hotfix |
| M-07 | `read_text_file` TOCTOU | 读取后 recheck 实际 content 长度 | tool-safety-hotfix |
| M-08 | blocking `is_file()`/`is_dir()` 在 async | 改用 `tokio::fs::metadata` | tool-safety-hotfix |
| M-09 | SQLite 无 WAL 模式 | `PRAGMA journal_mode = WAL; synchronous = NORMAL` | sqlite-consistency |
| M-10 | `unwrap_or_default()` 吞掉 corrupt JSON | `parse_json_column` helper 返回 `SessionError::Database` | sqlite-consistency |
| M-14 | `ForbiddenSection` 错误信息误导 | 只列出真正允许的 section | misc-fixes |
| M-16 | `TurnEvent` 死 variants | 删除 `Reasoning`/`ToolCallDelta`/`ToolCallEnd` | misc-fixes |
| M-17 | `topology.rs` 孤立死代码 | 删除文件 | misc-fixes |
| M-18 | `project_root` 回退空字符串 | 改回退 `"/"` | misc-fixes |

### LOW

| # | 问题 | 修复 | 分支 |
| --- | ------ | ------ | ------ |
| L-01 | `u32_field` 静默截断 | `u32::try_from(...).unwrap_or(u32::MAX)` | llm-stream-sse-hardening |
| L-03 | `ToolCallEnd` 顺序不定 | `BTreeMap<u32, String>` 按 index 排序 | llm-stream-sse-hardening |
| L-04 | `display_id` byte 切片 | `char_indices().nth(12)` 安全截断 | misc-fixes |
| L-05 | `config_dir()` 回退相对路径 | `HOME/.config` fallback + panic on missing | misc-fixes |

### 重构

| 改动 | 内容 | 分支 |
| ------ | ------ | ------ |
| trait 统一 | `lattice_tools::ToolExecutor` = `lattice_agent::ToolCallExecutor`，签名 `Result<String, String>`，12 个工具函数返回 `Err` | agent-loop-hardening |
| 工具面简化 | 删除 `read_files`（并行 `read_file` 替代），`web_search` → `web_fetch` | tool-safety-hotfix |
| 测试去重 | `test_helpers` module 提取共享 `user_msg`/`assistant_msg`/`tool_msg` | misc-fixes |
| hooks 移除 | `lattice-hooks` 从 workspace members 移除（文件保留，不编译） | misc-fixes |

### Phase 11 遗留项

| # | 问题 | 修复 |
| --- | ------ | ------ |
| WR-02 | env var 测试同步 | Router tests 用 local env mutex |
| WR-03 | SQLite per-method connection | 合并到 H-09 |
| WR-04 | tool_output_is_ok 前缀 | 合并到 H-07 |
| WR-06 | turn parent_commit_id TOCTOU | 初始 transcript 读取时 capture parent |
| IN-01 | 重复测试 helper | 提取到 `lattice_session::test_helpers` |
| IN-02 | lattice-hooks 未使用 | 从 workspace members 移除 |
| IN-03 | CLI 依赖 hooks 未 import | 删除依赖 |

### PR #28 遗留项

| # | 问题 | 修复 |
| --- | ------ | ------ |
| P2-01 | 模型覆盖未持久化 | turn commit metadata 正确记录 override model |
| P2-02 | NULL-tip branch checkout 失败 | 区分 branch exists + NULL commit_id vs missing |
| P2-03 | detached HEAD 无法 branch | 从 head_commit_id 取 detached commit |
| P2-05 | branch move 不同步 HEAD | branch upsert 同步 `graph_sessions.head_commit_id` |

---

## Phase 11 — 2026-05-18

Verified: `cargo fmt --all --check` ✓, `cargo clippy -- -D warnings` ✓, `cargo test` 504 pass ✓

### CR-01: Unknown message roles silently mapped to User

- **File:** `crates/lattice-agent/src/agent_loop.rs:474-501`
- **Before:** `_` wildcard branch mapped unknown roles to `Role::User`, corrupting conversation
- **After:** `convert_to_llm_message` returns `Result<Message, String>`, errors on unknown roles; caller returns `AgentTurnStatus::Failed`(Deprecated)

### CR-02: Provider hardcoded "unknown" in TurnCommitInput

- **File:** `crates/lattice-api/src/turn.rs:501`
- **Before:** `provider: "unknown".to_string()` — audit trail destroyed
- **After:** `resolve_model_and_prompt` returns `(model, prompt, provider)`; provider resolved via `ModelRouter::resolve(&ModelTarget::new(&model))`

### CR-03: ${VAR} env var expansion not blocked in check_command

- **File:** `crates/lattice-tools/src/executor.rs:877`
- **Before:** Metacharacter list blocked `$(` but not `$` or `${`, allowing credential leakage via `echo ${ANTHROPIC_API_KEY}`
- **After:** `"${"` and `"$"` added to blocked metacharacter list

### CR-04: Literal API key stored as both HashMap key and value

- **File:** `crates/lattice-cli/src/credentials.rs:53`
- **Before:** `values.insert(key_spec.clone(), key_spec.clone())` — same secret as key and value
- **After:** `values.insert(format!("config:{}", provider_name), key_spec.clone())` — secret appears only as value

### CR-05: aliases() returns empty Vec when no credentials

- **File:** `crates/lattice-llm/src/router.rs:103-119`
- **Before:** Doc claimed aliases() filtered by credential status; code already returned all known aliases
- **After:** Doc updated — `resolve()` returns `Ok(ResolvedModel)` for all known aliases regardless of `CredentialStatus`

### WR-01: Blanket #![allow] suppresses all lint warnings

- **File:** `crates/lattice-cli/src/lib.rs:1`
- **Before:** `#![allow(dead_code, unused_imports, private_interfaces)]` — masked unused deps, dead code
- **After:** Removed; per-module `#![allow(dead_code)]` for staged command modules only

### WR-05: ProviderConfig api_key serializable without protection

- **File:** `crates/lattice-cli/src/config.rs:26`
- **Before:** `pub api_key: Option<String>` with `#[derive(Serialize)]` — literal keys leak on serialization
- **After:** `#[serde(skip)]` added to `api_key` field; Deserialize reads, Serialize omits

### WR-07: write_file uses string concatenation for path construction

- **File:** `crates/lattice-tools/src/executor.rs:491-495`
- **Before:** `format!("{}/{}", self.project_root, path.trim_start_matches('/'))` — fragile, platform-specific
- **After:** `PathBuf::from(&self.project_root).join(path.trim_start_matches('/'))`

---

## Phase 12 repair — 2026-05-21

Verified: `rustfmt --edition 2021 --check` on touched Rust files ✓, `cargo test -p lattice-api` ✓, `cargo test -p lattice-llm provider_kind_as_str_is_stable_snake_case` ✓, `cargo clippy -p lattice-api -p lattice-llm --tests -- -D warnings` ✓

| # | 问题 | 修复 |
| --- | ------ | ------ |
| WR-01 | Resolved provider dropped before `AgentTurnRequest` | API now passes routed provider into `AgentTurnRequest.provider`, so `ModelStarted.provider` is populated on the API path |
| WR-04 | Provider string used `Debug` lowercase | Added `ProviderKind::as_str()` stable identifiers, including `openai_compatible` |
| WR-05 | `RealTurnDeps` cloned `AgentTurnResult.events` | Uses `std::mem::take` to move events out of the result without cloning |
| Planning | `STATE.md` said milestone complete; `VER-07` pending; `EVT-05` overbroad | Local `.planning/` state updated to Phase 13 ready, `VER-07` complete, and `EVT-05` narrowed to vocabulary representation |
| Deferred | WR-02/WR-03 need later semantic work | Local Phase 12 deferred-items file logs stage honesty for Phase 17 and timeout partial preservation for Phase 14/17 |

---

## Phase 14 follow-up — 2026-05-21

Verified: `cargo test -p lattice-agent --features test-utils --test streaming_delta` ✓, `cargo test -p lattice-agent --features test-utils` ✓, `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` ✓, `cargo clippy --workspace --all-targets -- -D warnings` ✓, `cargo fmt --all --check` ✓

| # | 问题 | 修复 |
|---|------|------|
| P1-14-01 | `ToolCallEnd` 后同 id 的 `ToolCallDelta` 仍可修改最终 arguments | `consume_model_stream` rejects delta-after-end and duplicate-end as invalid tool-call stream states; regression tests assert Error status, no execution, no `ModelFinished`, and no malformed committed tool calls |
| P1-14-02 | invalid tool-call-only response commits an empty assistant message | Invalid tool-call path now commits assistant content/reasoning only when either exists; regression test asserts malformed tool-only responses leave `messages` empty |
| Deferred | Phase 14 cancel checkpoints incomplete by design | `.planning/STATE.md` records Phase 15 cooperative cancellation checkpoints |

---

## Phase 17 review closeout — 2026-05-22

Verified: `cargo fmt --all --check` ✓, `cargo test -p lattice-agent --features test-utils timeout` ✓, `cargo test -p lattice-api stream_turn` ✓, `cargo test -p lattice-api --test consistency` ✓, `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` ✓, `cargo clippy -p lattice-api --all-targets -- -D warnings` ✓, `cargo test --workspace` ✓

| # | 问题 | 修复 |
| --- | ------ | ------ |
| P1-17-01 | Timeout cooperative deadline did not cover tool execution | `ToolCallExecutor::execute` now awaits through the deadline helper; timeout during a tool preserves state, emits no fake `ToolCallFinished`, and commits `Error(timeout)` metadata |
| P1-17-02 | Expired deadline could lose to ready stream events | Deadline expiry is checked before awaiting and deadline sleeps use `sleep_until`, so timeout wins once expired |
| P2-17-03 | Run/stream consistency did not compare committed terminal metadata | Both observation paths now read `terminal_reason` from committed log metadata; error stream events assert public `TurnFailed.reason` matches committed metadata |
| P2-17-04 | Run/stream consistency did not compare committed usage metadata | Both observation paths now read usage from committed log metadata and normalize zero usage |
| P2-17-05 | Receiver-drop test used fixed sleep | Test now polls for background commit completion with an outer timeout |
| Docs | Live stream and timeout contracts were stale | Rustdoc, Phase 17 validation/verification, summaries, roadmap wording, FINDINGS, and codebase docs now match the alpha evented-runtime design |
| Deferred | Live channel backpressure is outside cooperative timeout | Accepted alpha gap recorded in `.planning/STATE.md`; deadline-aware event emission is deferred until live/result event reliability semantics are specified |

---

## Turn execution unification (Steps 1–4) — 2026-05-23

Verified: `cargo test -p lattice-api` ✓, `cargo test -p lattice-api --test consistency` ✓, `cargo test -p lattice-api --test usage_turn` ✓, `cargo test -p lattice-cli run_integration` ✓, `cargo clippy -p lattice-api --all-targets -- -D warnings` ✓

| # | 问题 | 修复 |
| --- | ------ | ------ |
| Step 1 | Production `run_turn` used buffered `TurnDeps` path parallel to live `stream_turn` | `run_turn` now drains live `stream_turn` via `RunTurnCollector`; round-aware `assistant_output`, usage sum, synthetic stages |
| Step 2 | `turn/tests.rs` depended on `FakeTurnDeps` buffered seam | Migrated to public `run_turn` / `stream_turn` with `SessionApi` + `FakeStreamingLlmClient`; added optional `RunTurnRequest.cancel_flag` for cooperative-cancel E2E |
| Step 3 | Legacy `TurnDeps` / `execute_turn_inner` / `run_turn_with_deps` / `stream_turn_with_deps` duplicated orchestration | Deleted `deps.rs`, buffered execute path, and `map_tool_results`; single core is `stream_turn_live` |
| Step 4 | Consistency tests compared two full execution paths with dual tempdirs | Rewrote to assert `run_turn` == `collect_turn_result_from_events(drain stream_turn)` plus durable session-store anchor |

---

## Phase 28 review fixes: capability probe and boundary contract — 2026-06-03

Verified: `cargo fmt --all --check` ✓, `git diff --check` ✓, `cargo build` ✓, `cargo test` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test -p lancet-native` ✓, `cargo test -p lattice-cli` ✓, `cargo test -p lattice-cli --bin lattice lancet -- --nocapture` ✓

| # | 严重度 | 问题 | 修复 |
| --- | -------- | ------ | ------ |
| P28-CR-01 | CRITICAL | Verify-failed `exec.command_run` workspace mutations could leave durable metadata at `backend_invoked=false` after the backend had already run | Command execution metadata is now recorded and emitted immediately after `execute_argv` succeeds, before diff/verify return paths; added a verify-failure regression asserting durable metadata and events report `backend_invoked=true` |
| P28-WR-01 | WARNING | Native and CLI host-probe output used a separate Vec-shaped report instead of the protocol primitive-keyed `HostProbeReport` | Native now reuses `lancet_protocol::HostProbeReport`/`HostProbeEntry`; CLI JSON exposes the canonical `host_probe.primitives.<primitive>` shape and serializes gate-missing primitive reasons through the protocol type |
| P28-WR-02 | WARNING | Live host capability probes resolved `unshare`, `sh`, and child helpers through ambient `PATH` | Probes now use trusted absolute helper candidates, sanitize child `PATH`, and record helper paths in diagnostics; missing trusted helpers are reported as unknown instead of falling back to ambient `PATH` |

## Phase 34: CJK double-width character display fixes — 2026-06-07

User reported TUI output label calculation issues with Chinese double-width characters. Root cause: the entire codebase used `.len()` (byte count) or `.chars().count()` (Unicode scalar count) for display-relevant string lengths, with zero awareness of terminal column width (CJK = 2 columns, ASCII = 1 column).

Severity: LOW — display-only, no behavioral impact.

**Changes:**

| File | Change |
| ------ | -------- |
| `Cargo.toml` | Added `unicode-width = "0.1"` workspace dependency |
| `crates/lattice-agent/Cargo.toml` | Added `unicode-width` dependency |
| `crates/lattice-cli/Cargo.toml` | Added `unicode-width` dependency |
| `lattice-agent/src/agent_loop/mod.rs` | Added `truncate_to_width()` using `UnicodeWidthStr`/`UnicodeWidthChar`; error summary now truncates to 200 display columns instead of 200 chars; `output_len` changed from `.len()` (bytes) to `.chars().count()` |
| `lattice-tui/src/runtime.rs` | `{output_len}B` → `{output_len} chars` |
| `lattice-cli/src/commands/run.rs` | Same `{output_len}B` → `{output_len} chars` |
| `lattice-cli/src/commands/session.rs` | `summarize()` switched from `.chars().count()` to `.width()` for display-column-aware truncation |
| `lattice-cli/src/commands/run_tests.rs` | Updated test assertions from `"412B"`/`"120B"` to `"412 chars"`/`"120 chars"` |

Verification: pending `cargo check`, `cargo clippy`, `cargo test` (blocked by stale overlay directory permissions).

---

## Maintenance: Rust 2024 edition and dependency refresh — 2026-06-13

Severity: MAINTENANCE — workspace/toolchain compatibility and dependency currency.

**Problem:** The workspace still targeted Rust 2021 with Cargo resolver 2 and
several direct dependencies were pinned to older major versions.

**Fix:**

- Migrated all workspace crates to Rust 2024 and Cargo resolver 3 while keeping
  the project MSRV at Rust 1.88 (required by the exact-pinned
  `brush-parser 0.4.0`).
- Applied the Rust 2024 compatibility migration, including match ergonomics,
  macro fragment specifiers, environment mutation safety, and let-chain lint
  updates.
- Upgraded direct dependencies and regenerated `Cargo.lock`, including
  `reqwest 0.13.4`, `rusqlite 0.40.1`, `schemars 1.2.1`, `sha2 0.11.0`,
  `toml 1.1.2`, `unicode-width 0.2.2`, `colored 3.1.1`, and `dirs 6.0.0`.
- Kept `brush-parser` exact-pinned after confirming 0.4.0 remains the latest
  stable release, and configured reqwest 0.13 explicitly for rustls without
  default proxy behavior.
- Preserved canonical SHA-256 IDs by replacing the removed digest `LowerHex`
  formatting path with explicit two-digit lowercase byte encoding.

**Verification:**

- `cargo check --workspace --all-targets` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo test --workspace` — passed.
- `scripts/ci-local.sh` — passed (`fmt`, `clippy`, and workspace tests).

**Changed files:** workspace/crate manifests, `Cargo.lock`, Rust 2024 migration
sites across workspace crates, and this changelog.

---

## 34-07: Direct command write-scope enforcement (CR-01/02/03, WR-01) — 2026-06-20

Severity: CRITICAL (gap-closure Plan 34-07; 34-REVIEW CR-01, CR-02, CR-03, WR-01).

**Problem:** Direct `exec.command_run` for workspace-mutating commands passed
`AuthorizedEffectRegion::empty()` into cwd and verify preflight, skipped
post-execution changed-path scope checks, and recorded `WorkspaceMutation` effects
as `ObservedScope::Root` — which `AuthorizedEffectRegion::contains` rejects under
default `CepMode::Enforce`. Together these allowed commands outside
`allowed_write_paths` with no replan gate and broke the effect ledger in production.

**Fix:**

- Pass `ctx.authorized_region` into `validate_direct_command_cwd` and
  `run_verify_with_backend`; exempt workspace-root cwd (`"."`) at preflight
  (actual write scope enforced post-exec).
- Add `ExecutionBackend::changed_paths()` with `WorkspaceExecutionBackend`
  implementation via `git status --porcelain` parsing.
- After Direct workspace-mutating exec, diff changed paths against
  `authorized_region`, record per-path `ObservedScope::Path` effects, and deny
  out-of-scope paths with `ReplanRequired` before verify.
- Remove blanket `WorkspaceMutation` + `ObservedScope::Root` effect from
  `record_direct_command_effects`.

**Verification:**

- `cargo test -p lancet-native phase34_direct --lib` — 24 passed
- `cargo test -p lancet-native phase34_direct_command --lib` — 14 passed
- `cargo test -p lattice-api --lib workspace_execution_backend` — 4 passed

**Changed files:** `crates/lancet-native/src/handlers/command.rs`,
`crates/lancet-native/src/enforcement/verify.rs`,
`crates/lancet-native/src/execution_backend.rs`,
`crates/lancet-native/src/handlers/command_tests.rs`,
`crates/lattice-api/src/turn/workspace_backend.rs`, spy backends in
`mutation.rs`, `observation.rs`, `registry.rs`, `verify.rs`.

---

## 34-08: CLI audit path unification (D-08 gap closure) — 2026-06-20

Severity: HIGH (gap-closure Plan 34-08; 34-VERIFICATION Gap 2, D-08).

**Problem:** `lancet-api` production readers (`collect_status`, `query_audit`,
`tail_audit`) resolve the external project-keyed audit path via
`audit_db_path_for_project`, but `lattice-cli` audit and lancet status tests
still seeded the legacy `.lattice/cep_audit.db` path. Seeded rows landed in the
wrong database, causing 14 CLI test failures and split-brain audit visibility.

**Fix:**

- Replace legacy test helpers in `audit.rs` and `lancet.rs` with
  `lancet_api::audit_db_path_for_project`.
- Add `ScopedXdgStateHome` in `test_env.rs` to set isolated `XDG_STATE_HOME`
  under the crate-wide env mutex for parallel-safe test execution.
- Update module doc comments to reference the external audit path.
- Align `tool_executor_uses_workspace_root_not_profile_package_dir` with Direct
  Upper routing (workspace section, git baseline, `DeterministicWorkspace`
  override).

**Verification:**

- `cargo test -p lattice-cli audit -- --nocapture` — passed
- `cargo test -p lattice-cli` — 149 passed, 0 failed
- `grep -rn '\.lattice.*cep_audit' crates/lattice-cli/src/commands/` — no matches

**Changed files:** `crates/lattice-cli/src/commands/audit.rs`,
`crates/lattice-cli/src/commands/lancet.rs`,
`crates/lattice-cli/src/commands/test_env.rs`,
`crates/lattice-cli/src/commands/run_tests.rs`,
`crates/lattice-cli/Cargo.toml`, `Cargo.lock`.

---

## 34-09: Direct-mode release E2E gap closure (34-VERIFICATION Gap 3) — 2026-06-20

Severity: HIGH (gap-closure Plan 34-09; EXEC-02, EXEC-03).

**Problem:** `release_e2e_criterion_8` and `v1_4_command_artifact_dogfood` were
`#[ignore]`d because they targeted envelope-centric declare → exec flows. Under
Direct Upper routing (D-07), `session.envelope` is rejected and release gates
lacked Direct-mode integration evidence.

**Fix:**

- Rewrote `release_e2e_criterion_8` for Direct mode: removed envelope dead
  code and `#[ignore]`, chained `expected_hash` across sequential Upper edits,
  kept external audit assertions via `audit_db_path_for_project_in`.
- Rewrote `v1_4_command_artifact_dogfood` for Direct mode: exec-only LLM
  script, `[lancet.workspace] allowed_write_paths`, `XDG_STATE_HOME` isolation,
  external audit path, Direct metadata assertions, no Lower host artifact leak.
- Fixed Direct `exec.command_run` project-root cwd preflight when adapter
  reports normalized path `"."` but guard expects `""`.

**Verification:**

- `cargo test -p lattice-api --test release_e2e` — 1 passed, 0 ignored
- `cargo test -p lattice-api --test v1_4_command_artifact_dogfood` — 1 passed, 0 ignored
- `cargo test -p lattice-api --test phase34_upper_routing` — 6 passed
- `grep '#\[ignore' crates/lattice-api/tests/release_e2e.rs crates/lattice-api/tests/v1_4_command_artifact_dogfood.rs` — no matches

**Changed files:** `crates/lattice-api/tests/release_e2e.rs`,
`crates/lattice-api/tests/v1_4_command_artifact_dogfood.rs`,
`crates/lancet-native/src/handlers/command.rs`.

---

## 36-UAT: TUI target branch, disposition rendering, and prune coverage — 2026-06-22

Severity: MAJOR (TUI target branch and CLI disposition visibility) / MINOR
(pending snapshot prune coverage).

**Problem:**

- Starting a new TUI session without `--project-branch` silently targeted
  `main`, bypassing the explicit project-target requirement enforced by
  `session new`.
- Human-readable `session log` output showed terminal `status` but omitted the
  independent workspace `disposition`, even though storage and JSON projection
  already carried it.
- Phase 36 prune tests manipulated retained snapshots directly through the
  workspace adapter and never exercised `prune_session_snapshots` with a
  pending turn, leaving the CR-01 `skipped` path without integration coverage.

**Fix:**

- Made `--project-branch` conditionally required for `lattice tui` when no
  existing session ID is supplied, and added a runtime guard that rejects
  missing or blank branches before new-session creation.
- Extracted the human turn-log renderer and added a `disposition:` row beside
  `status:`, with a focused rendering regression test.
- Added an integration test that creates a `PendingWorktree` turn with retained
  snapshot evidence, invokes `prune_session_snapshots`, asserts the snapshot is
  returned in `skipped`, and confirms it remains available for resume.

**Verification:**

- `cargo test -p lattice-tui` — 3 passed.
- `cargo test -p lattice-cli` — 167 unit + 9 cutover + 16 smoke passed.
- `cargo test -p lattice-api --test phase36_resume` — 19 passed.
- `cargo clippy -p lattice-cli -p lattice-tui --all-targets` — passed; existing
  warnings remain in dependency crates and pre-existing CLI test code.
- `git diff --check -- crates/lattice-cli/src/main.rs
  crates/lattice-cli/src/commands/session.rs crates/lattice-tui/src/runtime.rs
  crates/lattice-api/tests/phase36_resume.rs` — passed.
- `scripts/ci-local.sh` — stopped at the repository-wide `fmt` gate because of
  existing rustfmt drift across unrelated `lancet-*`, `lattice-agent`, and
  `lattice-workspace` files; clippy and workspace tests were therefore not
  reached by that script.

**Changed files:** `crates/lattice-cli/src/main.rs`,
`crates/lattice-cli/src/commands/session.rs`,
`crates/lattice-tui/src/runtime.rs`,
`crates/lattice-api/tests/phase36_resume.rs`,
`.planning/phases/36-turn-lifecycle-and-disposition/36-UAT.md`, and this
changelog.

---

## Doc-sync: documentation drift correction — 2026-06-27

Severity: INFO (documentation-only; no behavior change).

**Problem:** Several living-reference docs had drifted from the codebase after
recent refactors:

- Root `AGENTS.md` and `README.md` claimed a 10-method CEP catalog including
  `session.envelope` (actual: 9 methods; `session.envelope` removed).
- Root `AGENTS.md` and `crates/lattice-session/AGENTS.md` described the session
  graph as SQLite-backed (actual: git-backed `GitSessionGraphStore`;
  `sqlite_graph/` retired by commit `687dd5c5`).
- Root `AGENTS.md` `lattice-tui` role said "Ratatui/crossterm terminal UI shell"
  (actual: rustyline inline REPL; no alternate screen, no ratatui).
- Root `AGENTS.md` crate table and layer graph omitted `lattice-workspace`
  (Layer 0 leaf; production MSB/Upper workspace adapter).
- Root `AGENTS.md` CEP/LANCET contracts described Direct-mode write-scope
  enforcement and `[lancet.isolation]` (actual: Direct-mode retired by commits
  `c160d3eb`/`e939ae02`/`dc507d15`; landlock/netns isolation removed by
  `a29072e2`; `[lancet.isolation]` removed from the profile schema).
- `docs/LANCET.md` claimed a 10-method catalog and documented the removed
  landlock/netns/isolation system as current behavior.
- `docs/lancet-warn-report.md` (v1.3.1-alpha point-in-time baseline) listed
  `session.envelope` as current.
- `FINDINGS.md` carried OPEN entries (L-20, L-29-01/02/03/04, L-31-02, L-34-01)
  referencing removed files (`sqlite_graph/`, `executor/supervisor/`).
- `crates/lancet-audit/AGENTS.md` and `crates/lattice-workspace/AGENTS.md`
  carried stale "SQLite session" / "Direct-mode" wording.

**Fix:**

- Regenerated canonical docs from the live codebase (each grounded by a
  doc-writer subagent that verified facts against source):
  `README.md` (144 lines), `docs/ARCHITECTURE.md` (332 lines),
  `docs/CONFIGURATION.md` (511 lines), `docs/GETTING-STARTED.md` (255 lines),
  `docs/DEVELOPMENT.md` (206 lines), `docs/TESTING.md` (308 lines),
  `CONTRIBUTING.md` (238 lines). All carry the GSD marker and use
  `<!-- VERIFY: ... -->` markers for claims not discoverable from the repo.
- Surgically corrected living refs:
  - `AGENTS.md`: added `lattice-workspace` to Layer 0 and the crate table;
    fixed `lattice-session` (git-backed) and `lattice-tui` (rustyline REPL)
    roles; replaced Direct-mode/isolation/SQLite claims with MSB-Upper,
    removed-isolation, and git-backed statements.
  - `crates/lattice-session/AGENTS.md`: rewrote Role/Sub-Architecture/Boundaries
    for the git-backed store (`git_graph/`, `migrate/`, `promotion.rs`);
    closed L-20 as moot.
  - `crates/lancet-audit/AGENTS.md`: session graph is git-backed, not SQLite.
  - `crates/lattice-workspace/AGENTS.md`: dropped retired "Direct-mode" naming
    from the changed-path WATCH.
  - `docs/LANCET.md`: added a drift notice; corrected the catalog to 9 methods
    (no `session.envelope`); marked the removed isolation/overlay/canary
    sections as historical design context.
  - `docs/lancet-warn-report.md`: added a point-in-time notice that
    `session.envelope` was removed.
- `FINDINGS.md`: moved L-20, L-29-01/02/03/04, L-31-02, L-34-01 from OPEN to
  OBSOLETE/CLOSED with dated close notes; re-scoped L-34-02 (changed-path
  porcelain gap) to the MSB/Upper boundary.

**Verification:**

- All 7 canonical docs confirmed on disk with GSD markers and line counts.
- Spot-checked generated docs for stale claims — every `session.envelope` /
  `sqlite_graph` / `ratatui` / `landlock` / `isolation.command` hit is a
  deliberate corrective statement ("no `session.envelope`", "sqlite_graph was
  removed", "no ratatui/alternate screen", "landlock/netns removed"), not a
  stale assertion.
- `FINDINGS.md` OPEN section no longer references removed `sqlite_graph/` or
  `executor/supervisor/` paths.

Files: `README.md`, `docs/ARCHITECTURE.md`, `docs/CONFIGURATION.md`,
`docs/GETTING-STARTED.md`, `docs/DEVELOPMENT.md`, `docs/TESTING.md`,
`CONTRIBUTING.md`, `AGENTS.md`, `crates/lattice-session/AGENTS.md`,
`crates/lancet-audit/AGENTS.md`, `crates/lattice-workspace/AGENTS.md`,
`docs/LANCET.md`, `docs/lancet-warn-report.md`, `FINDINGS.md`, `FIX.md`.

---

## Codeberg #11: retire agent-visible audit CEP methods — 2026-07-03

Severity: MEDIUM (security / agent surface reduction).

**Problem:** `observe.audit` and `observe.audit_replay` were registered in the
production CEP catalog and executed through the native server, granting agents
`ReadAudit` capability and a policy-gated path to read the full audit store.

**Fix:**

- Removed `observe.audit` / `observe.audit_replay` from `lancet-catalog::CATALOG`
  (8→6 agent methods). Kept query/tail/replay DTOs in
  `lancet-catalog::methods::audit` for operator APIs only.
- Deleted native audit handlers and registry wiring; no audit methods in
  `register_production_method_registry`.
- Implemented `lancet-api::query_audit`, `tail_audit`, `replay_audit` as direct
  `CepAuditStore` reads (project-keyed path via `audit_db_path_for_project`).
- `lattice-cli` `audit` command and integration tests call `lancet-api` only
  (no CEP server for audit reads). Regression tests assert operator
  `audit.rs` sources do not reference `LancetServer` / `CepClient` and that
  pre-`model_id` SQLite layouts remain queryable (no versioned migration table;
  `init_schema` uses `CREATE IF NOT EXISTS` + best-effort `ALTER TABLE`).
- `CepAuditStore::query_rows` returns only finalized rows (`ended_at` and
  `result_or_error` present).
- `lattice-api` CEP adapter returns `METHOD_NOT_FOUND` for retired audit method
  names; turn provenance tests use `replay_audit`.

**Verification:**

- `cargo test -p lancet-catalog -p lancet-audit -p lancet-api`
- `cargo test -p lattice-api --test cep_tests`
- `cargo test -p lattice-cli audit`

**Files:** `crates/lancet-catalog/`, `crates/lancet-native/`, `crates/lancet-api/`,
`crates/lancet-audit/`, `crates/lattice-api/`, `crates/lattice-cli/`, `FIX.md`,
`crates/lancet-api/AGENTS.md`, `README.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`,
`docs/LANCET.md`, `docs/CONFIGURATION.md`, `docs/GETTING-STARTED.md`,
`docs/v1.3-alpha-walkthrough.md`, `docs/lancet-warn-report.md`, and related
`AGENTS.md` / design-note cross-references.

---

## Codeberg #11 follow-up: operator replay CLI + audit read-path verification — 2026-07-04

Severity: LOW (operator surface completeness / verification).

**Problem:** The #33 retirement left `replay_audit` as a `lancet-api` library
API with no operator CLI entry point — documentation promised `replay_audit`
for provenance reconstruction, but `lattice audit` only exposed query/tail.
Separately, the audit read-path needed verification across four axes: direct
store read (no CEP server), finalize-only rows + WAL snapshot, pagination, and
concurrent-read safety.

**Fix:**

- Added `lattice audit replay --turn <id> [--session <s>] [--json]` subcommand
  to `lattice-cli::commands::audit`. `AuditCmd` now takes an optional
  `AuditAction` subcommand (`Query` / `Replay`); `lattice audit` with no
  subcommand defaults to the existing query/tail path (flatten
  `AuditQueryArgs`), preserving backward compatibility. `render_replay_output`
  renders trace/statement/`stored_statement_match`/`source_audit_range_or_hash`
  in both JSON and human-readable modes.
- Re-exported `AuditReplayResult` from `lancet-api::lib` (was missing —
  `replay_audit`'s return type was not nameable by consumers).
- Added `replay_audit` doc comment documenting the wire-superset contract
  (`stored_statement_match` / `source_audit_range_or_hash` are on the wire,
  not just durable metadata as in the retired handler) and the no-self-audit
  contract (operator read, not agent capability → no `record_durable_metadata`
  / `record_effect`).
- Added `audit.rs` module doc documenting the concurrency invariant: SQL
  finalize-only predicate + WAL statement-level snapshot, covered by
  `operator_audit_reads_concurrent_with_active_writer` (synthetic writer + 4
  reader tasks + 1 tail task on multi-thread runtime).
- Added `tail_audit` doc comment documenting the cancellation contract:
  `tokio::task::yield_now().await` between batches (drop-cancellation at batch
  boundaries), bounded by `AUDIT_TAIL_MAX_BATCHES * AUDIT_TAIL_MAX_ROWS`.
- Rewrote `tail_audit_paginates_large_result_without_cep_server` to seed 1001
  rows (`AUDIT_TAIL_MAX_ROWS + 1`), exercising real cross-batch pagination
  (capped → `done=false` → cursor advance → second batch → `done=true`).
  Asserts `rows.len() == 1001`, `batches == 2`, `done`, `last_audit_id == 1001`,
  no duplicate `audit_id` across batches.
- Added `replay_audit_matches_stored_attestation` test: seed rows +
  `store_turn_attestation` → `replay_audit` asserts
  `stored_statement_match == Some(true)` (previously uncovered path).
- Added CLI tests: `audit_replay_reconstructs_turn_trace_for_seeded_db`,
  `audit_replay_reports_stored_attestation_match`,
  `audit_replay_errors_for_unknown_turn`,
  `cli_surface_audit_replay_subcommand`, `cli_surface_audit_query_subcommand`.
- Recorded L-11-01 (tail yield_now downgrade, no CancellationToken + CLI
  wiring) and L-11-02 (concurrent-read covered by synthetic writer, no
  live-turn integration test) as ACCEPTED RESIDUAL in `FINDINGS.md`, with
  escalation conditions.

**Verification:**

- `cargo test --workspace` (TMPDIR=/tmp) — all green.
- `cargo test -p lancet-api --lib audit` — 15 passed.
- `cargo test -p lattice-cli audit` — 13 audit + 3 surface tests passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.

**Files:** `crates/lancet-api/src/audit.rs`, `crates/lancet-api/src/lib.rs`,
`crates/lattice-cli/src/commands/audit.rs`, `crates/lattice-cli/src/main.rs`,
`crates/lancet-api/AGENTS.md`, `crates/lattice-cli/AGENTS.md`, `FINDINGS.md`,
`FIX.md`, `docs/LANCET.md`.

## Codeberg #21: harness 假 LLM、SessionSendDriver 完整 turn、PTY smoke — 2026-07-05

Severity: FEATURE (测试重构 2/6).

**Problem:** `lattice-system-harness` (#42) 只能驱动 `lattice --help` 等
fast-binary 黑盒；无法驱动确定性的完整 turn，后续产品级场景 (#46/#27–#30)
没有载体。#43 要求四块交付：localhost 假 LLM、`SessionSendDriver`、`PtyTuiDriver`、
场景超时 + 进程组清理 + 失败产物 + 一个最小完整 turn 场景。

**Fix summary:**

- `src/fake_llm.rs` — localhost OpenAI-compatible HTTP/SSE 假服务（std
  `TcpListener` 手写，零新依赖）。通过正式 `OPENAI_COMPATIBLE_*` 配置接入；
  捕获真实请求（model / message_roles / wire tool_names / raw body）；四种
  SSE 脚本（`PlainText` / `ToolCall` / `Disconnect` / `IllegalResponse`）按
  `OpenAiSseParser` 期望的 chunk 形状渲染（`data: {json}\n\n` + `data: [DONE]`）。
  7 个 fast 单测（含真实 HTTP round-trip tracer）。
- `src/session_send.rs` — `SessionSendDriver` 驱动公开
  `lattice --json session send`，逐行解析 JSONL；`TurnEventLine` 解析器
  **容忍未知变体**（公开契约是增量的，未知 `type` → `Unknown{type_name, raw}`，
  绝非解析错误）；`SendExitCode` 分类 0/1/2 契约；`derive_send_exit_code`
  仅从公开 JSONL 信号推导退出码（镜像产品 `exit_code_from_state`，但不导入
  产品 enum）；`assert_event_subsequence` 断言必现事件子序列（RFC 决定五：禁止
  整段 golden）。15 个 fast 单测。
- `src/driver.rs` — 升级：子进程在独立进程组 spawn（Unix
  `process_group(0)`），超时 `kill -KILL -<pgid>` 杀整组（PRD user story 20）；
  新增 `processes_in_pgid` 公开 helper（Linux /proc 扫描）供测试与 PTY smoke
  机检“整组已死”。3 个 fast 单测，含 **grandchild-kill** 测试（`sh -c 'sleep 30
  & wait'` + 1s 超时 → 验证 /proc 无同 pgid 幸存者），这是单 `kill()` 不满足、
  `killpg` 满足的核心 #43 验收点。
- `src/artifacts.rs` + `src/assert.rs` — 失败产物扩容：`write_fake_llm_log`
  写假 LLM 请求日志；`WorkspaceSnapshot::capture` 捕获 turn 前工作区树清单，
  `write_workspace_diff` 写 turn 前/后清单 + 统一 diff（`workspace_before.txt` /
  `workspace_after.txt` / `workspace_diff.txt`，PRD user story 21：工作区差异），
  均对 `secrets` 子串脱敏；`assert_with_full_artifacts` 协调完整 bundle
  （stdout/stderr/command/world env + 假 LLM 日志 + 工作区清单）。
  `tests/msb_full_turn.rs` 的 `write_full_bundle` 接 turn 前 snapshot，失败时写
  workspace before/after diff。7 个 fast 单测（含 snapshot 排序、diff 增删改、
  脱敏）。
- `src/layer_guard.rs` — 层级 gating：`#[ignore]` + `LATTICE_HARNESS_MSB=1`
  env guard。置位而 MSB 不可用时 `require_msb_env` 经 `lattice lancet status`
  探测后 **panic**（RFC 不可回退边界 7：必须失败不静默）；未置位时 `#[ignore]`
  退出普通 CI，保持 fast lane 干净。2 个 fast 单测。
- `src/scenario.rs` — 新增可选 `total_timeout_secs` + `ScenarioBudget`
  （PRD user story 19：步骤级 + 总量超时）。多步场景每步前 `budget.check`，
  超时 panic。`None` 默认保留 #42 单步行为。5 个 fast 单测。
- `src/pty_tui.rs` — `PtyTuiDriver`（`portable-pty`，Unix-only target dep，
  harness-boundary 通过）。`wait_for` 用 ANSI 剥离后的文本做语义匹配，提示符/
  颜色/排版只存在于 driver 内，场景零展示细节断言（RFC 决定三）。5 个 fast 单测
  （含 `cat` PTY round-trip tracer）。
- `tests/msb_full_turn.rs` — 最小完整 turn 场景（layer=microsandbox，
  `#[ignore]`）：纯文本 turn → JSONL 子序列（turn_started → model_started →
  token_delta → model_finished → turn_committed）→ exit 0 与流推导一致 →
  `session log/status --json` 反映新 commit → 假 LLM 收到 model=msb-plain 的
  user 请求。失败写完整 bundle（含假 LLM 日志 + 工作区清单 + cross-validation
  输出）。
- `tests/pty_tui_smoke.rs` — PTY smoke（layer=pty_smoke，`#[ignore]`）：启动
  TUI → 等 `>` 提示符 → 发 ping → 等 `pong` token → `/quit` → Drop 杀进程组 →
  Linux /proc 扫描验证无幸存者。不断言任何展示细节。
- `crates/lattice-system-harness/AGENTS.md` — 补 Boundaries（假 LLM 正式配置
  接入、SessionSendDriver 容忍未知变体、进程组 spawn+killpg）、Test Layers
  （fast lane / MSB+PTY `#[ignore]` + `LATTICE_HARNESS_MSB=1`）、Verification
  （full gate `--include-ignored` 必须 fail-not-skip）。

**Design decisions (拍板，理由见上):**

1. 层级 gating 用 `#[ignore]` + `LATTICE_HARNESS_MSB=1` env guard（非
   env-gated 编译方案——后者有“意外 skip”风险）。符合 PRD user story 17 与
   RFC 不可回退边界 7。
2. 假 LLM 用 std `TcpListener` 手写 SSE（零新依赖）；PTY 用 `portable-pty`
   （通用测试库，harness-boundary 通过；Unix-only target dep；依赖面从轻）。
3. JSONL 断言粒度 = 必现事件**子序列**，非整段 golden（RFC 决定五）。

**Verification:**

- `cargo fmt --all --check` — clean。
- `cargo clippy --workspace --all-targets -- -D warnings` — clean。
- `cargo run -q --package xtask -- harness-boundary` — PASS（3/3 groups）。
- `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness`
  — **44 lib + 4 fast_cli_help = 48 passed, 0 failed, 2 ignored** (MSB/PTY
  smoke, gated by `LATTICE_HARNESS_MSB=1`).
- `cargo test --workspace --exclude lancet-native` — **1307 passed, 0 failed**.
  - **L-26-01 已知坑**：`lancet-native` 的 `fetch_effect_contract_negative` 在
    本 DNS 黑洞环境挂起（远早于 #43 的 ACCEPTED RESIDUAL，见 FINDINGS.md:42）；
    正常网络无此问题。`cargo xtask ci` 的 test step 在受限环境需
    `--exclude lancet-native` 绕过，已在 FINDINGS.md 注明，非 #43 回归。
- **MSB 实跑约束**：本执行环境 `microsandbox` daemon 未运行、`msb` CLI 不在
  PATH；`tests/msb_full_turn.rs` 与 `tests/pty_tui_smoke.rs` 的 fast 层
  （编译 + `#[ignore]` 跳过 + layer_guard/parse/budget 单测）已全绿，**真实
  MSB turn 与 PTY smoke 的实跑由 owner 在本机 KVM 完整门验证**
  （`LATTICE_HARNESS_MSB=1 ... --include-ignored`）。

**Files:** `crates/lattice-system-harness/{Cargo.toml,AGENTS.md,
src/{lib.rs,fake_llm.rs,session_send.rs,driver.rs,artifacts.rs,assert.rs,
layer_guard.rs,scenario.rs,pty_tui.rs},tests/{msb_full_turn.rs,pty_tui_smoke.rs}}`,
`FIX.md`, `FINDINGS.md`。

---

## Codeberg #27: `xtask ci` test step ran `cargo test --workspace` and triggered dev-machine compile-phase OOM — 2026-07-07

Severity: HIGH (twice forced a hard reboot of the owner's dev machine during
`cargo test --workspace`; the local CI gate was unusable on the 24-core
oversubscribed dev host).

**Problem:** `xtask/src/ci.rs:44` ran a single `cargo test --workspace`. On
the dev machine (`nproc`=24, 31 GB RAM + 14 GB swap, heavy desktop with many
Electron apps over-reserving ~7478 GB of virtual address space), cargo's
default of one rustc per core (24 parallel) plus codegen allocation spikes
hit global OOM during the **compile phase** — before any test binary ran. The
kernel OOM killer selected `adj=300` Electron processes (highest `oom_score`
from the giant `total_vm`), crashing the desktop and forcing reboots. The
compilation toolchain itself was small (~2.41 GB RSS across 24 rustc + 19
rust-lld + 1 cargo + 2 rust-analyzer); the root cause was host oversubscription
plus swap near-empty, not a lattice code or test bug. See FINDINGS.md
"ENV-OOM-01" for the full root-cause ledger.

A secondary failure mode: the old gate also hung indefinitely when
`lancet-native::fetch_effect_contract_negative` hit the DNS-blackhole hang
(L-26-01), because there was no per-crate timeout.

**Fix:** Rewrote the `xtask ci` test step to run one `cargo test -p <crate>`
per workspace member (deterministic dependency order via `cargo_metadata`,
excluding `xtask`), each under a hard per-crate timeout. Compile parallelism
is capped via `CARGO_BUILD_JOBS` (default 4) across **all** gate steps (fmt
inherits the env var but is single-threaded; clippy/build/test all capped) —
this bounds the compile footprint on oversubscribed hosts without slowing
down resource-rich CI, where `CARGO_BUILD_JOBS` can be raised. The timeout
uses `process_group(0)` + `kill -KILL -<pgid>` so a hung crate's whole tree
(cargo + rustc + test binaries) is reaped, no orphans. `LATTICE_SKIP_KNOWN_HANGS=1`
makes the gate skip `fetch_effect_contract_negative` (L-26-01) so the gate is
usable on DNS-blackhole hosts; `LATTICE_TEST_TIMEOUT_SECS` overrides the
300 s/crate default.

This does **not** change release-gate scripts: `release-gate-v1.3.sh` and
`release-gate-v1.4.sh` already run per-crate `cargo test -p <crate>` (the
handoff's claim that they "depend on full test" was inaccurate). `release-gate.sh`
is already retired (exits 1). The only `cargo test --workspace` in the repo was
`xtask/src/ci.rs:44`.

**Verification (all on the same 24-core dev host, no reboot):**

- `cargo fmt -p xtask --check` — clean.
- `cargo clippy -p xtask --all-targets -- -D warnings` — clean.
- `cargo test -p xtask -- --test-threads=1` — **26 passed** (incl. 2 new
  `ordered_test_crates` tests asserting determinism, `xtask` exclusion, and
  dependency-before-dependent ordering).
- `CARGO_BUILD_JOBS=4 cargo build --workspace --tests` — **PASS in 14.89 s**
  (incremental; swap stayed at 0 B). Confirms the jobs=4 cap is the high-
  leverage mitigation.
- Per-crate `timeout 300 cargo test -p <crate> -- --test-threads=1` over all
  17 non-`lancet-native` crates — **17/17 PASS, no timeouts, no runaways**
  (spawn-subprocess crates clean: `lattice-workspace` 2 s, `lattice-cli` 9 s,
  `lattice-system-harness` 4 s; biggest `lattice-api` 34 s).
- `lancet-native` `fetch_effect_contract_negative` reproduced the L-26-01 hang
  (90 s hard timeout, no `ok`/`FAILED`) — confirms it is environment-
  independent and separate from the OOM (already ACCEPTED RESIDUAL).
- `LATTICE_SKIP_KNOWN_HANGS=1 LATTICE_TEST_TIMEOUT_SECS=180 CARGO_BUILD_JOBS=4
  ./target/debug/xtask ci` — **EXIT=0, "All checks passed"**, all 18 crates
  pass, skip notice printed for `lancet-native`, no OOM, no orphans.
- Timeout+kill mechanism: `LATTICE_TEST_TIMEOUT_SECS=15 ./target/debug/xtask ci`
  (no skip flag) — gate **EXIT=1** at 24 s, `lancet-native` killed at 15 s with
  `timed out after 15s`, **zero orphan rustc/cargo/test processes** after exit.

**Residual (not introduced by this fix, surfaced by per-crate isolation):**
running tests at default thread count (parallel) can flake two env-var races —
`lattice-api::turn_runtime_constructs_one_adapter_per_turn` and
`lattice-system-harness::msb_requested_is_false_when_unset` — both pass under
`--test-threads=1`. These are pre-existing `unsafe env::set_var` test races
that the old `cargo test --workspace` gate masked by OOM-ing in the compile
phase. Tracked in FINDINGS.md ENV-OOM-01.

**Changed files:** `xtask/src/ci.rs`, `FIX.md`, `FINDINGS.md`.

---

## Codeberg #23: test refactor 4/6 - batch-execute triage (fast-binary lane) - 2026-07-11

Severity: FEATURE (测试重构 4/6). Parent #40; triage #44.

**What:** Executed the #44 triage for the lanes verifiable without KVM:
landed the fast-binary black-box scenarios for the no-turn CLI surface,
deleted the source-scan/weak tests, and consolidated fixtures. Batch 3
(turn/Upper, 39 MSB-bound elevates) and the turn-bearing CLI subset (70
elevates) are deferred - recorded as deviations, not silently re-judged
(see `docs/TEST-TRIAGE.md` §1.5 and FINDINGS.md `T-23-01`).

**Deliverables:**

- New fast-binary harness scenarios (no product-crate deps; boundary-clean):
  - `crates/lattice-system-harness/tests/fast_config_cli.rs` (13 tests) -
    `models` / `resolve` / `doctor` + secret redaction + HOME/XDG isolation
    (replacement `planned_config_cli`).
  - `crates/lattice-system-harness/tests/fast_session_cli.rs` (8 tests) -
    `session new/list/status/log/branch` no-turn subset (replacement
    `planned_session_cli`).
  - `crates/lattice-system-harness/tests/fast_audit_cli.rs` (4 tests) -
    `audit query/replay/tail` missing-store error states (replacement
    `planned_audit_cli`, partial - empty/seeded states need MSB).
  - `fast_cli_help.rs` gained `fast_retired_run_command_rejected`.
  - `ScenarioWorld::write_agent_profile` consolidates the agent-profile
    fixture (hand-written TOML; harness boundary forbids `lattice-profile`).
- Blocking product bug fixed: `lattice-cli/src/commands/audit.rs` `audit::run`
  nested a current-thread runtime inside `#[tokio::main]` and panicked
  (`Cannot start a runtime from within a runtime`, exit 101) on a
  missing/empty audit store. Made `audit::run` async (reuses the top-level
  runtime); `run_at` kept sync under `#[cfg(test)]` for the crate-internal
  audit unit tests. This unblocked `planned_audit_cli` and is in scope per
  PRD #40 ("不顺手修复独立产品问题，除非直接阻塞本测试体系").
- Deleted 11 source-scan tests (all `n/a` - no replacement; boundary covered
  by `cargo xtask harness-boundary` / `lancet-boundary` + dependency graph):
  `lancet-api::operator_audit_module_does_not_reference_cep_server_types`,
  `lattice-api::production_code_does_not_fabricate_passing_verification`,
  `lattice-cli::{audit_command_source_does_not_reference_cep_server,
  run_source_has_no_old_crate_imports,
  session_prune_snapshots_variant_exists_in_enum,
  project_branch_send_source_has_no_branch_flag,
  boundary_no_legacy_session_store_patterns,
  focused_tests_import_no_removed_crate_names}`,
  `lattice-profile::source_free_of_forbidden_symbols`,
  `lattice-session::{promotion_crate_has_no_workspace_or_api_dependency,
  promotion_module_is_object_only}`.
- Elevated (deleted) 31 internal tests now superseded by the harness fast
  scenarios: `lattice-cli` `models.rs` / `resolve.rs` / `doctor.rs` test
  modules (20), `audit_query_errors_when_store_missing` (1), and the no-turn
  `smoke_*` / `cli_cutover_*` in `release_smoke.rs` / `cli_cutover.rs` (10).
- Removed now-dead helpers `with_env_var` / `without_env_vars` from
  `lattice-cli/src/commands/test_env.rs` (fixture consolidation).

**Counts:** deletes executed 11/11; elevates executed 31/140; elevates
deferred 109/140 (39 MSB-bound + 70 turn-bearing CLI). Triage verdicts
unchanged.

**Verification:**

- `cargo fmt --all --check` - clean.
- `cargo clippy --workspace --all-targets -- -D warnings` - clean.
- `cargo xtask harness-boundary` - PASS (harness still has no product-crate
  deps; `write_agent_profile` is hand-written TOML).
- `cargo xtask lancet-boundary` - PASS.
- `LATTICE_BIN=… cargo test -p lattice-system-harness` - 51 lib + 30 fast
  binary + 2 `#[ignore]` MSB/PTY, all green.
- Affected crates green: `lattice-cli` 148, `lancet-api` 17, `lattice-profile`
  96, `lattice-session` 69, `lattice-api` (full).
- `cargo xtask ci` - **EXIT=0, "All checks passed"** (fmt / clippy / build /
  both boundary gates / all workspace tests; MSB+PTY scenarios correctly
  `#[ignore]` without KVM).

**Changed files:** `crates/lattice-cli/src/commands/audit.rs`,
`crates/lattice-cli/src/main.rs`,
`crates/lattice-cli/src/commands/{models,resolve,doctor,run_tests,session,test_env}.rs`,
`crates/lattice-cli/tests/{release_smoke,cli_cutover}.rs`,
`crates/lattice-api/tests/promotion_wiring.rs`,
`crates/lattice-profile/src/schema.rs`,
`crates/lattice-session/tests/promotion_commit.rs`,
`crates/lancet-api/src/audit.rs`,
`crates/lattice-system-harness/src/world.rs`,
`crates/lattice-system-harness/tests/{fast_config_cli,fast_session_cli,fast_audit_cli,fast_cli_help}.rs`,
`docs/TEST-TRIAGE.md`, `FIX.md`, `FINDINGS.md`.

**Note:** Issue #45 left open and not pushed per request; 109 deferred
elevates tracked in FINDINGS.md `T-23-01` for the MSB/KVM lane.

## Codeberg #23 review-fix round 1+2: triage mapping precision + fast-binary JSON contracts - 2026-07-12

Severity: TEST (review feedback on #45 commit `0a365c89`). Parent #40; triage #44.

**What:** Closed all 6 review comments on #45's fast-binary test migration,
making the triage replacement mapping per-row auditable and re-securing the
public JSON / semantic text contracts. Two rounds of review-fix. No product
behavior changed; old crate-internal tests stay deleted (plus 3 more deleted
in round 2). Round 3 corrections are recorded in the next entry below.

**Feedback closure:**

1. *95 `lattice-cli` elevates used one generic 3-way replacement.* CONFIRMED.
   All 95 rows in `docs/test-triage-decisions.json` now map to the narrowest
   scenario by test behaviour; the generic
   `planned_session_cli|planned_config_cli|planned_audit_cli` value is gone.
   Round 2 corrected 9 additional imprecise mappings.
2. *`session status/log/branch` had no JSON contract test.* CONFIRMED.
   `fast_session_cli.rs` gained three `--json` contract scenarios asserting the
   stable object fields (`session_id`/`active_agent`/`commit_count`/
   `project_target_branch`/`branches`/`turns` per renderer). Text tests kept.
3. *`resolve --json` did not assert `provider_model`.* CONFIRMED.
   `fast_resolve_json_…` now asserts `provider_model` is a non-empty string and
   `api_key_env == "ANTHROPIC_API_KEY"` (no version pinning); secret sentinel
   scan kept.
4. *`doctor --json` did not assert credential object fields.* CONFIRMED.
   `fast_doctor_json_…` now asserts `credentials` is a non-empty array of
   objects with string `env_var`/`status` ∈ {Present, Missing} and the
   `ANTHROPIC_API_KEY` entry is `Present`; `alias_count` and sentinel scan kept.
5. *`models --auth` not line-guarded.* PARTIALLY CONFIRMED (reviewer overstated
   the old cutover test's line-counting; it only checked non-empty/secret/icon/
   URL). New assertion parses stdout per-line, set-compares the credentialed
   Anthropic aliases `{opus, sonnet, haiku}`, rejects `gpt-5.5`, and forbids
   `✓`/`✗`/`http://`/`https://`/sentinel on every line. Round 2 removed a
   redundant whole-stdout guard (per-line scan + `assert_no_secret` already
   cover it) and added `lines.len() == expected.len()` to catch duplicate
   alias lines that `HashSet` would mask.
6. *Acceptance count still 1443.* CONFIRMED, doc-only. `TEST-TRIAGE.md` §4 now
   reads 1449 (matches `jq length`); snapshot date/scope untouched.

**Round 2 corrections (9 mapping fixes + 2 test deletions + 1 new fast test):**

- `cep_status_missing_db_returns_empty_state`: `planned_audit_cli` ->
  `planned_lancet_cli` (new scenario ID; tests `lattice lancet status` empty-DB
  behavior, not `audit query`).
- `cep_doctor_passes_on_repo_root`: elevate -> `keep`/`local_invariant`
  (LANCET doctor catalog/registry invariant via `#[cfg(test)]` helper, not
  public black-box).
- `run_integration_missing_session_returns_not_found`: combo -> **takeover**;
  new `fast_session_send_nonexistent_session_errors` test covers `session send
  <missing-id>` (fails at `open_session` before workspace, exit 2 + stderr
  "not found"). Original test deleted from `run_tests.rs`.
- `smoke_session_status_list_shows_seeded_session` / `smoke_session_status_specific_session`:
  reason clarified -- `seed_session` committed turn is test fixture, not
  assertion; actual assertions covered by existing fast tests. Stays
  redundant-kept.
- `audit_query_prints_fragment_for_seeded_db` /
  `audit_query_empty_result_for_unknown_session`: `planned_audit_cli` ->
  `planned_audit_ops_msb` (seeded audit DB needs committed turn).
- `release_smoke_release_gate_v1_4_includes_lancet_boundary_check` /
  `release_smoke_release_gate_sh_is_retired`: elevate -> `keep`/`local_invariant`
  (script-file content scan / script-deprecation smoke, not CLI help surface).
- `project_branch_new_source_defines_field`: `keep` -> **delete** + code deleted
  (`include_str!` source-scan; RFC #41 decision six).
- `not_found_error_includes_object_id`: `keep` -> **delete** + code deleted
  (`#[cfg(test)]` helper; missing-id behavior covered by fast binary).

**Triage mapping detail after round 2** (JSON total/identity preserved at 1449):
32 already-deleted rows -> the landed scenario that took them over (31 from #45
batch + 1 `run_integration_missing_session` via new `session send` fast test);
47 true-deferred rows -> `msb_full_turn` / `planned_audit_ops_msb` /
`planned_resume_msb` / `planned_provider_fail_msb` / `planned_lancet_cli`;
4 redundant-kept rows (replacement lane landed but original test not deleted);
2 combo/precise rows; 15 rejudged `keep`/`local_invariant` with `null`
replacement (11 round-1 + 3 round-2 + 1 pre-existing); 2 round-2 `delete`
(source-scan + cfg-test helper).
`lattice-cli` counts: 保留 78 / 提升 101 / 删除 6 -> 92 / 85 / 8 (still 185).
Global: 1298 / 140 / 11 -> 1312 / 124 / 13 (still 1449).

**Note:** Round 3 corrections supersede some of these numbers; see the round-3
entry below for the final authoritative counts.

## Codeberg #23 review-fix round 3: target_layer correction + Upper-prior failures + failure-artifact contracts - 2026-07-13

Severity: TEST (review feedback on round-1+2). Parent #40; triage #44.

**What:** Corrected 10 mapping errors from round 1+2, added 4 new fast-binary
tests for Upper-prior failure paths, fixed `target_layer` for MSB-scenario
records, and hardened test failure-artifact contracts.

**Round 3 corrections:**

1. *`target_layer` for MSB items was `fast_binary` instead of
   `real_microsandbox`.* Fixed 43 `lattice-cli` elevate records whose
   replacement is `msb_full_turn` / `planned_*_msb` to `target_layer =
   "real_microsandbox"`. 39 `fast_binary` records remain (planned_*_cli +
   fast_cli_help + planned_lancet_cli).

2. *4 Upper-prior failures incorrectly deferred to MSB.* Moved to fast-binary
   takeover with new black-box tests:
   - `cep_status_missing_db_returns_empty_state` -> `planned_lancet_cli`
     (takeover; `lattice lancet status` empty-DB returns empty state, exit 0).
   - `smoke_session_resume_without_session_id_fails` -> `planned_session_cli`
     (takeover; clap rejects missing session_id, exit 2).
   - `smoke_session_resume_nonexistent_session_returns_error` ->
     `planned_session_cli` (takeover; `open_session` fails before workspace,
     exit 1).
   - `smoke_session_send_no_profile_fails_with_clear_message` ->
     `planned_session_cli` (takeover; `load_agent_checked` fails after deleting
     `.lattice/agents/`, exit 2 + no secret leak).
   Original internal tests deleted from `release_smoke.rs`.

3. *`audit_query_reads_legacy_schema_store` cannot be covered by MSB.* Changed
   elevate -> `keep`/`local_invariant`. Manually implanted old SQLite schema
   tests backward compat; RFC #41 harness boundary forbids opening/implanting
   internal audit DB, and real turns only generate current schema.

4. *`tool_executor_uses_workspace_root` needs observe tool call.* Changed
   `msb_full_turn` -> `planned_observe_msb` + `real_microsandbox`. The
   `msb_full_turn` profile only executes text turns (`tools.enabled = []`),
   cannot trigger `observe.workspace`.

5. *2 resume source-scan tests should be delete.* `session_resume_handler_function_exists`
   and `session_resume_calls_typed_api` both use `impl_source()` + string
   search. Changed keep -> `delete` (RFC #41 decision six). Code deleted from
   `session.rs`.

6. *`session send` exit code not pinned to 2.* `assert_exit_failure` only
   checks nonzero. Added `assert_exit_code(&out, 2)` to
   `fast_session_send_nonexistent_session_errors` and
   `fast_session_send_no_agent_profile_errors`. New `assert_exit_code` helper
   added to `crates/lattice-system-harness/src/assert.rs`.

7. *JSON parse panics lose failure artifacts.* All 4 `expect("... is a JSON
   object")` calls in `fast_session_cli.rs` (list/status/log/branch `--json`)
   replaced with `parse_json_or_artifacts` helper that routes parse errors
   through `assert_with_failure_artifacts`.

8. *`session-new` failure artifacts overwrite each other.* `assert_session_new_contract`
   now takes `scenario_name` parameter (uses `spec.name`) instead of hardcoded
   `"session-new"`, so parallel callers don't clobber each other's artifacts.

**Final triage mapping detail** (JSON total/identity preserved at 1449):
36 already-deleted takeover rows (31 #45 batch + 1 round-2 + 4 round-3);
41 true-deferred rows -> `msb_full_turn`(33) / `planned_audit_ops_msb`(7) /
`planned_provider_fail_msb`(1) / `planned_observe_msb`(1) / `planned_resume_msb`(1),
all with `target_layer = "real_microsandbox"`;
4 redundant-kept rows; 1 combo/precise row; 16 rejudged `keep`/`local_invariant`
(11 round-1 + 3 round-2 + 1 round-3 + 1 pre-existing); 4 deletes (2 round-2 +
2 round-3).
`lattice-cli` counts: 78 / 101 / 6 -> **93 / 82 / 10** (still 185).
Global: 1298 / 140 / 11 -> **1313 / 121 / 15** (still 1449).
Total deferred: 80 (41 lattice-cli + 36 lattice-api + 3 lattice-agent).

**Additive work:** `fast_session_cli.rs` grew from 8 to 15 tests (3 JSON
contract + 1 send missing-id + 2 resume + 1 send no-profile);
`fast_config_cli.rs` strengthened 3 existing scenarios; `assert.rs` gained
`assert_exit_code` helper; `lib.rs` exports it.

**Verification:**

- `cargo build -p lattice-cli --bin lattice` - OK.
- `LATTICE_BIN=… cargo test -p lattice-system-harness --test fast_session_cli
  --test fast_config_cli -- --test-threads=1` - **28 passed; 0 failed** (15
  session incl. 3 JSON + 4 Upper-prior, 13 config incl. 3 strengthened).
- `cargo test -p lattice-cli` - 129 unit + 4 cli_cutover + 7 release_smoke = 140 passed.
- Triage acceptance: `jq empty` OK; `jq length` == 1449; generic 3-way count
  == 0.
- `cargo fmt --all --check` - OK.
- `cargo clippy --workspace --all-targets -- -D warnings` - OK.
- `cargo xtask harness-boundary` - OK.
- `cargo xtask lancet-boundary` - OK.
- `LATTICE_SKIP_KNOWN_HANGS=1 cargo xtask ci` - **All checks passed.**
- `git diff --check` - OK.

**Changed files:**
`crates/lattice-system-harness/src/assert.rs` (new `assert_exit_code`),
`crates/lattice-system-harness/src/lib.rs` (export),
`crates/lattice-system-harness/tests/fast_session_cli.rs`,
`crates/lattice-system-harness/tests/fast_config_cli.rs`,
`crates/lattice-cli/src/commands/session.rs` (deleted 2 source-scan tests),
`crates/lattice-cli/src/commands/run_tests.rs` (deleted 1 test, round 2),
`crates/lattice-cli/tests/release_smoke.rs` (deleted 3 tests),
`docs/test-triage-decisions.json`, `docs/TEST-TRIAGE.md`, `FIX.md`,
`FINDINGS.md`.

**Note:** Residual (41 true-deferred lattice-cli + 4 redundant-kept = 45 rows
in lattice-cli; 39 lattice-api/agent deferred) stays under FINDINGS.md
`T-23-01`/`T-23-02` pending the MSB/KVM lane; 16 keep-deviations are rejudged,
not deferred; 4 deletes are landed and closed. No push, no issue closure, no
history rewrite.

## Codeberg #23 review-fix round 4: msb_full_turn coverage honesty + exit-code precision + secret scan - 2026-07-13

Severity: TEST (review feedback on round 1-3). Parent #40; triage #44.

**What:** Corrected 33 false `msb_full_turn` mappings, fixed the lancet status
false takeover, hardened resume/send exit-code and secret-scan assertions,
and fixed `assert_exit_code` diagnostics.

**Round 4 corrections:**

1. *33 `msb_full_turn` mappings were not honestly coverable.* The
   `msb_full_turn` scenario only executes a plain-text success turn with
   `--json` and checks the event subsequence + exit 0 + log/status
   cross-validation. It does NOT cover: human stdout/stderr rendering
   (`render_human_*`, 8 tests), JSON event serialization with synthetic
   events including failure variants (`render_json_*`, 3 tests), verbose
   epilogue rendering (`verbose_epilogue_*`, 2 tests), renderer state machine
   with failure paths (`seq_*`, 11 tests), human turn log rendering (1 test),
   session-graph scoping for branch/checkout/reset/diff (5 tests), seeded
   session log (1 test), missing-credentials path (1 test), or
   `run_integration_commits` (1 test). All 33 rejudged from
   elevate/`msb_full_turn` -> **keep**/`local_invariant` with `null`
   replacement.

2. *`cep_status_missing_db_returns_empty_state` false takeover.* No
   `fast_lancet_status` test exists and the original test remains in source.
   Reverted from takeover to **deferred** (`planned_lancet_cli`, fast test
   not yet added).

3. *Resume exit-code assertions imprecise.* `fast_session_resume_without_session_id_fails`
   now asserts `assert_exit_code(&out, 2)` (clap convention) + stderr contains
   `SESSION_ID`. `fast_session_resume_nonexistent_session_errors` now asserts
   `assert_exit_failure` + stderr contains `not found` + `nope-sid`.

4. *`send_no_profile` secret scan only checked stderr.* Replaced with
   `assert_no_secret(&out, sentinel)` which scans both stdout and stderr.

5. *`assert_exit_code` reused `ExitNotSuccess` error.* Added
   `ExitCodeMismatch { expected, actual }` variant to `AssertError`.

**Final counts:** lattice-cli **126 keep / 49 elevate / 10 delete** = 185.
Global **1346 / 88 / 15** = 1449. target_layer: 40 fast_binary + 9
real_microsandbox. Total deferred: 49 (10 lattice-cli + 36 lattice-api + 3
lattice-agent).

**Verification:** 28 fast tests pass; 140 lattice-cli tests pass; fmt/clippy/
boundary/xtask ci all green.

## Codeberg #23 review-fix round 5: restore invocation-error and honest residual contracts - 2026-07-13

Severity: TEST (independent follow-up review of the round-4 claim). Parent #40;
triage #44.

**Problem:** Round 4 still accepted exit 1 for `session resume <missing-id>`,
blanket-rejudged eight product-behavior tests as local invariants, and left the
JSON/Markdown/FINDINGS category arithmetic inconsistent. Config JSON tests also
parsed with `expect`, losing failure artifacts on malformed output.

**Fix:**

1. `cmd_resume_api` now classifies `ApiError::NotFound` as invocation error exit
   2; the fast black-box test pins exit 2 plus `not found` and the requested ID.
2. Split the 33 round-4 blanket keeps after source inspection: 25 genuine
   renderer/serializer invariants remain keep; `run_integration_commits` is an
   `msb_full_turn` redundant-kept elevate; five cross-session graph tests,
   human seeded log, and missing-credential redaction map to precise planned MSB
   scenarios and remain in source until those scenarios land.
3. Added `planned_session_graph_msb` and `planned_session_history_msb`; separated
   the one `planned_lancet_cli` fast residual from KVM-gated work.
4. All four config JSON parsing sites now convert parse failures to
   `AssertError` before `assert_with_failure_artifacts`, preserving stdout,
   stderr, and world context.
5. Resynchronised per-record Markdown with JSON and corrected the partition of
   the original 101 CLI elevate rows: 35 takeover + 17 deferred + 5
   redundant-kept + 40 keep rejudgments + 4 delete = 101.

**Final ledger:** lattice-cli **118 keep / 57 elevate / 10 delete** = 185;
global **1338 / 96 / 15** = 1449; elevated target layers **40 fast_binary + 17
real_microsandbox**. Deferred total **56** (55 MSB/KVM + 1 fast-binary); CLI
residual is 17 deferred + 5 redundant-kept = 22.

**Verification:** `cargo build -p lattice-cli --bin lattice`; 28 targeted fast
harness tests; `cargo test -p lattice-cli` (140 passed); `cargo fmt --all
--check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo xtask
harness-boundary`; `cargo xtask lancet-boundary`; `cargo xtask ci` (all checks
passed); JSON/Markdown lattice-cli row check (185/185, zero decision/layer
mismatches); `git diff --check HEAD`.

**Changed files:** `crates/lattice-cli/src/commands/session.rs`,
`crates/lattice-system-harness/tests/fast_session_cli.rs`,
`crates/lattice-system-harness/tests/fast_config_cli.rs`,
`docs/test-triage-decisions.json`, `docs/TEST-TRIAGE.md`, `FINDINGS.md`,
`FIX.md`.

## Codeberg #23 review-fix round 6: missing credential is fast input failure - 2026-07-13

Severity: TEST (final spec-axis follow-up). Parent #40; triage #44.

**Problem:** `smoke_session_send_missing_credentials_fails_without_secrets`
was still classified as an MSB scenario, although credential resolution occurs
inside `resolve_upper_config` before `prepare_upper`. The retained release smoke
only asserted nonzero and scanned stderr for a generic `sk-` prefix.

**Fix:** Added
`fast_session_send_missing_credential_is_redacted_input_error`, which drives the
public binary with a credential-requiring profile and asserts exit 2, the exact
missing env-var name, and no sentinel in either stdout or stderr. `run_inner`
now maps pre-stream `ApiError::BadRequest` input/config failures to exit 2. The
weak release smoke was deleted, and its ledger row moved to
`fast_binary/planned_session_cli` as a landed takeover.

**Final ledger correction:** decisions remain lattice-cli **118/57/10** and
global **1338/96/15**; elevated target layers are now **41 fast_binary + 16
real_microsandbox**. The original 101 CLI elevate rows partition as 36 takeover
- 16 deferred + 5 redundant-kept + 40 keep rejudgments + 4 delete. Deferred
total is **55** (54 MSB/KVM + 1 fast); CLI residual is 16 deferred + 5
redundant-kept = 21.

**Verification:** 29 targeted fast harness tests pass; `cargo test -p
lattice-cli` passes 139 tests; `cargo xtask ci` reports all checks passed;
`cargo xtask lancet-boundary` passes; JSON/Markdown contain 185/185 CLI rows
with zero decision/layer/reason mismatches; `git diff --check HEAD` passes.

**Changed files:** `crates/lattice-cli/AGENTS.md`,
`crates/lattice-cli/src/commands/run.rs`,
`crates/lattice-cli/tests/release_smoke.rs`,
`crates/lattice-system-harness/tests/fast_session_cli.rs`,
`docs/test-triage-decisions.json`, `docs/TEST-TRIAGE.md`, `FINDINGS.md`,
`FIX.md`.

## ENV-OOM-02: fix `unsafe env::set_var` parallel-test races (harness MSB_ENV + lattice-api XDG_STATE_HOME) — 2026-07-12

Severity: MEDIUM (test flakiness; both flakes reproduced under stress before
the fix, zero after).

**Problem:** FINDINGS ENV-OOM-02 documented two accepted-residual flakes that
only became visible once `xtask ci` (#27) started actually executing tests
per-crate (the old workspace gate OOM'd during compilation and never reached
them):

1. `lattice-system-harness` `layer_guard::tests`: `msb_requested_is_false_when_unset`
   and `msb_requested_is_true_when_one` mutate the process-global `MSB_ENV`
   with no synchronisation. The parallel libtest runner interleaves one
   test's `remove_var` between the other's `set_var` and its
   `msb_requested()` read. **Reproduced: 232/2000 stress iterations failed
   (~12%)**, always `assertion failed: v` in `msb_requested_is_true_when_one`.
2. `lattice-api` `turn::cep_tests::turn_runtime_constructs_one_adapter_per_turn`:
   its second `build_test_production_deps(..., in_memory_audit=false, ...)`
   call resolves the external audit DB via `dirs::state_dir()` — an
   **unguarded `XDG_STATE_HOME` read** — while three sibling tests
   (`attestation_matches_replay_statement`, `replay_without_stored_attestation`,
   `external_audit_preserves_lower`) point `XDG_STATE_HOME` at their own
   `TempDir` under the `test_env::ENV_MUTEX` lock the flaky test never took.
   When the read lands in that window, the audit SQLite store opens inside
   another test's tempdir, which is then dropped → `disk I/O error`.
   **Reproduced: 8/500 full-lib stress iterations failed (~1.6%)**, panic at
   `test_support.rs:73` with the DB path visibly under `/tmp/.tmpXXXXXX/…`
   (another test's tempdir) — confirming the FINDINGS hypothesis and also
   explaining the historical `ci-local.sh` "disk I/O error" flake noted in
   the #42 entry.

**Fix** (house pattern — hand-rolled test mutex as in
`lattice-api/src/test_env.rs` / `lancet-api`'s `XDG_TEST_LOCK`; no new
dependency, no `--test-threads=1` hammer):

- `crates/lattice-system-harness/src/layer_guard.rs`: added a
  `static MSB_ENV_LOCK: Mutex<()>` in the tests module; both tests take it
  before mutating `MSB_ENV`. SAFETY comments rewritten to state the actual
  invariant (all mutation serialised under the lock) instead of the false
  "no other code reads MSB_ENV concurrently" claim.
- `crates/lattice-api/src/turn/cep_tests.rs`: `turn_runtime_constructs_one_adapter_per_turn`
  now holds `crate::ScopedXdgStateHome::new()` for its whole body — it
  serialises against the other env-mutating tests via the shared
  `ENV_MUTEX` *and* gives the persisted audit DB an isolated
  `XDG_STATE_HOME` tempdir (previously, when the race did not fire, the
  test wrote the audit DB into the real platform state directory).

**Verification:**

- Stress re-run, same loops as reproduction: harness `layer_guard` filter
  **0/2000 failures** (was 232/2000); `lattice-api` full lib binary
  **0/500 failures** (was 8/500). Default thread count both times.
- `cargo test -p lattice-api` — 108 lib + all integration suites pass.
- `LATTICE_BIN=… cargo test -p lattice-system-harness` — 51 unit + fast
  lanes pass; 2 KVM scenarios stay `#[ignore]`.
- `cargo clippy -p lattice-system-harness -p lattice-api --all-targets -- -D warnings` — clean.
- `cargo fmt -p lattice-system-harness -p lattice-api --check` — clean.

**Survey of remaining env mutators** (same sweep, no action needed):
`lattice-llm/src/lib.rs` and `lancet-api/src/audit.rs` already serialise
under their own mutexes; `lattice-cli/src/commands/test_env.rs` mirrors
`lattice-api`'s guarded helper; `lattice-api/tests/{audit_turn_provenance,
dogfood_impact_matrix,release_e2e}.rs` are single-test binaries (no
intra-process race). `lattice-api/tests/upper_routing.rs` has one unguarded
mutator (`upper_routing_e2e` sets `TEST_SECRET`/`XDG_STATE_HOME` raw) among
7 tests — latent only (no sibling reads those vars; a theoretical
glibc `setenv`-vs-`getenv` hazard remains via e.g. tempfile's `TMPDIR`
reads), left as-is and noted in FINDINGS ENV-OOM-02's closure entry.

**Changed files:** `crates/lattice-system-harness/src/layer_guard.rs`,
`crates/lattice-api/src/turn/cep_tests.rs`, `FINDINGS.md`, `FIX.md`.

## Codeberg #37: production Turn operates directly on the canonical Local Workspace — 2026-07-17

Severity: FEATURE (Direct-Local migration step 1 + step 3 production cutover).

**Problem:** Production Turns created an MSB Upper for every call, routed
native handlers through `WorkspaceExecutionBackend` behind `ExecutionBackend`,
and carried projection/baseline, guest Git, snapshot/retained-state, KVM, and
`[workspace]` profile machinery. The closed five-method catalog did not
operate on the canonical Local Workspace directly.

**Fix summary:**

- `lancet-native`: added one concrete `LocalWorkspace` module
  (`crates/lancet-native/src/workspace/{mod,local,types}.rs`) owning secure
  per-segment path resolution (symlink parent/leaf rejection, root-escape
  rejection, containment for not-yet-existing create targets), bounded
  read/list/grep/find, content hashing (Local Revision), no-replace create
  (`O_CREAT|O_EXCL`), atomic edit install (same-dir tempfile → fsync →
  rename → fsync-dir), remove, and filesystem error normalization. Added a
  crate-internal `LocalExecutionBackend` + `direct_local_routing(root)`
  factory in `execution_backend.rs` that wraps the one concrete workspace
  behind the existing `ExecutionBackend` trait. `tempfile` promoted from
  dev-dep to runtime dep. 14 module-level ADR-step-1 tests cover all eight
  specified behavior classes.
- `lancet-api`: `LancetRuntimeConfig` no longer carries an `execution` field;
  `LancetRuntime::build` constructs the one Direct-Local routing internally
  from `policy.project_root`. `NullExecutionBackend` has no production
  references (left for #60).
- `lattice-api`: deleted `turn/{upper,workspace_backend,promotion,resume,
  snapshot_journal}.rs`; trimmed `turn/lifecycle.rs` to the terminal
  classification + `finalize_turn` Destroy path; removed
  `RunTurnRequest.workspace_override`, `ProductionTurnDeps.upper_lease`, the
  Upper/lease teardown sequence in `live.rs`, and the `fold_upper_destroy_error`
  branch. `ApiError` dropped `PromotionRecoveryRequired` /
  `PromotionRecoveryConflict` / `PromotionLifecycle`. Public re-exports in
  `lib.rs` + `turn/mod.rs` shrunk to the remaining lifecycle + turn types.
- `lattice-cli`: deleted the `session resume` and `session prune-snapshots`
  subcommands, their handler functions, and the three source-scan tests that
  asserted their presence. Removed the `lattice-workspace` dev-dependency.
- `lattice-tui`: removed `workspace_override: None` from the REPL turn
  request.
- `lattice-api` / `lattice-cli`: removed the `lattice-workspace` dependency
  declarations (runtime + dev) and the `integration_tests` feature.
- New acceptance test file
  `crates/lattice-api/tests/direct_local_acceptance.rs` (7 tests) exercises
  the real production runtime against a temp Local root: closed five-method
  catalog, observe sees external modifications between Tool calls, create
  rejects existing target, stale-hash edit leaves target unchanged and
  replans, correct-hash edit atomically installs, successful mutation
  survives a later failure, audit records the `direct-local` execution
  backend.

**Deleted test files:** `tests/{upper_routing,promotion_wiring,
turn_lifecycle_tests,resume_tests,turn_target_tests,snapshot_journal_tests}.rs`
(contracts no longer exist; turn_target/turn_lifecycle/resume/snapshot_journal
were bound to `lattice-workspace::promotion::ManagedLower` / retained
snapshots / the snapshot journal, all removed).

**Residual (deferred to #62):** `SessionApi::commit_prepared_turn` /
`get_turn_commit` / `get_snapshot_lost` are now unused (`#[allow(dead_code)]`
with #62 pointers); `get_snapshot_lost` and the `lattice-session` snapshot /
promotion contract are removed in #62. The `lattice-workspace` crate itself,
its `microsandbox` dependency, and the MSB/KVM harness scenarios are removed
in #62. `lancet-native::NullExecutionBackend` and the
`ExecutionRouting` / `UpperContext` carrier survive until #60 retires the
`ExecutionBackend` trait.

**Verification:**

- `cargo build --workspace --all-targets` — clean (0 warnings, 0 errors).
- `cargo test -p lancet-native --lib -- --test-threads=1 --skip fetch` —
  102 passed, 0 failed (6 network fetch tests skipped; they hit
  `https://evil.example.com` and are pre-existing flakes unrelated to #59).
- `cargo test -p lancet-api --lib` — 17 passed.
- `cargo test -p lattice-api` (lib + all integration tests) — 143 passed,
  0 failed.
- `cargo test -p lattice-cli` — 135 passed (126 unit + 4 cli_cutover + 5
  release_smoke).
- `cargo test -p lattice-tui` — 3 passed.
- Per-crate matrix (`lancet-protocol` 36, `lancet-catalog` 27, `net-guard` 15,
  `lancet-dispatch` 5, `lancet-cep` 7, `lancet-audit` 34, `lancet-authority`
  118, `lattice-profile` 96, `lattice-session` 67, `lattice-agent` 8,
  `lattice-llm` 133, `lattice-workspace --features test-utils` unchanged) —
  all green.
- Pre-existing failure unrelated to #59: `lancet-native --test cep_warn_harness`
  expects `docs/lancet-warn-report.md`, which was removed in `59d4c44c`
  (docs organization commit, before this work).

**Changed files:** `crates/lancet-native/Cargo.toml`,
`crates/lancet-native/src/lib.rs`,
`crates/lancet-native/src/execution_backend.rs`,
`crates/lancet-native/src/workspace/{mod,local,types}.rs` (new),
`crates/lancet-api/src/{config,runtime}.rs`,
`crates/lattice-api/Cargo.toml`, `crates/lattice-api/src/lib.rs`,
`crates/lattice-api/src/session.rs`,
`crates/lattice-api/src/turn/{mod,types,cep,live,lifecycle,test_support,
run_tests,cep_tests,live_tests}.rs`,
`crates/lattice-api/tests/{support/mod,consistency,release_e2e,usage_turn,
audit_turn_provenance}.rs`,
`crates/lattice-api/tests/direct_local_acceptance.rs` (new),
`crates/lattice-cli/Cargo.toml`,
`crates/lattice-cli/src/commands/{session,run,run_tests}.rs`,
`crates/lattice-cli/tests/release_smoke.rs`,
`crates/lattice-tui/src/runtime.rs`, `FINDINGS.md`, `FIX.md`.
Deleted: `crates/lattice-api/src/turn/{upper,workspace_backend,promotion,
resume,snapshot_journal}.rs` and `crates/lattice-api/tests/{upper_routing,
promotion_wiring,turn_lifecycle_tests,resume_tests,turn_target_tests,
snapshot_journal_tests}.rs`.

## Codeberg #37 review correction: no-replace create install + audit evidence — 2026-07-17

Post-review fixes to the #59 commit (9a20dedf) before push.

**Problem:** The /review spec sub-agent found that production `mutate.create`
routed through `EditStorage::write_edit_target` →
`LocalExecutionBackend::write_file` → `LocalWorkspace::install_atomic` (rename
overwrite), not the no-replace `O_CREAT|O_EXCL` install ADR 0003 decision 8
requires. A concurrent writer between preflight and install would be silently
overwritten instead of surfacing as ReplanRequired. Audit criterion 8 was
also partial: the §5 test only asserted the `"direct-local"` substring, not
real Local path / Local Revision / install-result evidence.

**Fix summary:**

- `lancet-native::execution_backend::ExecutionBackend` gains a `create_file`
  method (default impl: check-then-write fallback) that the Direct-Local
  `LocalExecutionBackend` overrides to call `LocalWorkspace::create_excl`
  (`O_CREAT|O_EXCL`). `EditStorage::create_edit_target` routes the create
  install through it; `write_file` stays the atomic-replace path for edits.
- `mutation::write_create` now calls `create_edit_target`; a "target already
  exists" install diagnostic maps to `CepErrorCode::ReplanRequired` +
  `Retriability::RequiresReplan` (ADR decision 7). `preflight_create`'s
  already-exists diagnostic also maps to ReplanRequired (stale create
  precondition), via `create_preflight_diag_to_cep`.
- `LocalWorkspace::create_excl` `create_parents` loop fixed to walk only
  ancestor directories (not the file leaf) — previously a root-level file
  like "race.txt" was mkdir'd as a directory before the O_EXCL open, causing
  a false AlreadyExists.
- `LocalExecutionBackend::resolve_path` now delegates to
  `LocalWorkspace::resolve_facts` (new `pub(crate)` accessor) instead of a
  duplicated free function; the duplicated `resolve_path_facts`/`file_kind`
  helpers are deleted. `LocalWorkspace::read_file` now uses the
  secure-resolved `full` path instead of re-joining lexically.
- Standards fixes: `execution_backend.rs` + `lifecycle.rs::finalize_turn`
  stale docstrings updated; `lancet-native/AGENTS.md` Sub-Architecture lists
  `src/workspace/`; grep/find `1000` magic numbers documented as
  handler-overridden safety caps; `workspace/mod.rs` `#[allow(unused_imports)]`
  annotated with the #60 pointer.
- Acceptance tests expanded to 10: added `create_install_is_no_replace_not_overwrite`
  (second create on existing target → ReplanRequired + original preserved),
  `observe_list_sees_external_addition_between_tool_calls` (list_directory
  sees an externally added file), `policy_deny_blocks_create_on_direct_local`
  (write-deny pattern blocks create on the Direct-Local path), and
  `audit_records_direct_local_execution_backend_with_local_evidence` now
  asserts path + content_hash (Local Revision) + canonical-root identity.
  `runtime_config_defaults_have_no_execution_field` now asserts the routing
  is `direct-local` via audit metadata (plan §3).

**Verification:**

- `cargo build --workspace --all-targets` — clean (0 warnings, 0 errors).
- `cargo test -p lancet-native --lib -- --test-threads=1 --skip fetch` —
  102 passed (6 pre-existing network flakes skipped).
- `cargo test -p lancet-api --lib` — 17 passed.
- `cargo test -p lattice-api --test direct_local_acceptance` — 10 passed.
- `cargo test -p lattice-api` (lib + all integration) — 143 + 10 = 153
  passed across the crate.
- `cargo test -p lattice-cli` — 135 passed. `cargo test -p lattice-tui` — 3.

**Changed files (correction):** `crates/lancet-native/src/execution_backend.rs`,
`crates/lancet-native/src/edit/storage.rs`,
`crates/lancet-native/src/handlers/mutation.rs`,
`crates/lancet-native/src/workspace/local.rs`,
`crates/lancet-native/AGENTS.md`, `crates/lancet-api/src/runtime.rs`,
`crates/lattice-api/src/turn/lifecycle.rs`,
`crates/lattice-api/tests/direct_local_acceptance.rs`, `FIX.md`.

## Codeberg #38: Structured Workspace Operations consolidated; ExecutionBackend routing layer deleted — 2026-07-17

Severity: FEATURE (Direct-Local migration step 2 + step 3 deletion half; architecture closure, no compat seams).

**Problem:** After #59's production cutover, the migration scaffolding was
still live: the `ExecutionBackend` trait, `ExecutionRouting`/`UpperContext`
carrier, `RoutingInvocationState` flags, `NullExecutionBackend`, the
`direct_local_routing` adapter, backend spy/null test implementations, and
routing identity metadata (`execution_backend`, `upper_workspace_id`,
`adapter_preflight_invoked`, `backend_invoked`) persisted in durable audit.
LANCET embedders could in principle inject alternative workspace
implementations, and `fetch.web`/`session.todo_write` carried workspace
routing state through the shared call context.

**Fix summary:**

- `lancet-native/src/execution_backend.rs` deleted whole-file:
  `ExecutionBackend`, `ExecutionRouting`, `UpperContext`,
  `NullExecutionBackend`, `RoutingInvocationState`, `direct_local_routing`,
  `changed_paths`. Neutral Local-operation vocabulary (`WorkspacePath`,
  `WorkspacePathError`, `PathFacts`, `PathIntent`, `PathKind`,
  `DirectoryEntry`, `GrepRequest`, `GrepMatch`, `FindRequest`) moved to
  `workspace/types.rs`; `PathFacts::to_guard()` now produces the Guard L1
  view directly, deleting the `path_facts_to_guard` bridge and three
  duplicated `path_kind` converters. `lib.rs` re-exports shrink accordingly.
- `HandlerContext` carries `workspace: Arc<LocalWorkspace>` (pub(crate));
  `execution`/`routing_invocation` and the mark/record routing helpers are
  gone. `ServerRunConfig` drops `execution`; the native server constructs
  the one concrete `LocalWorkspace` once per run from
  `cep_context.policy.project_root` inside `CepServerNativeExt::run_native`
  (new private `NativeServerRuntime`); `server.run(registry, config)` call
  sites migrated to `run_native`. `LocalWorkspace` gained an async
  `resolve_facts` (blocking pool) for handler/edit preflight;
  `resolve_secure` stays module-private; the unused `root()` accessor and
  `content_hash_of` were deleted rather than ported.
- Observation/mutation handlers and the edit engine (`EditStorage`,
  `apply_parsed_patch`) call the concrete workspace directly.
  `EditStorage::write_edit_target` returns `InstalledFile` and
  `create_edit_target` returns `CreatedFile`, so durable mutation metadata
  records the post-install content hash (Local Revision), the guarded
  normalized path, and `compiled_patch_format` (was `compiled_patch_backend`).
- Audit evidence replacement: routing metadata writes deleted from
  transport/registry; `RejectionStage` vocabulary de-Uppered
  (`AdapterPreflight` → `PathGuard`, `BackendExecution` → `LocalIo`,
  `VerificationFailure` deleted); `CallAuditMetadata.backend_invoked` field
  deleted; `mutation_preflight_metadata` drops `backend_invoked` and renames
  `expected_hash_status` → `local_revision`, carrying the real observed
  Local Revision on stale-hash replans via a structured
  `EditCompileFailure`. On success the edit handler records the
  deterministic post-install Local Revision (content hash of the compiled
  `after` content — the hashline apply re-verifies the old hash before
  writing, so no re-read and no race window). `backend_invoked` keys
  removed from observation/mutation/fetch/todo/guard metadata, transport
  compact metadata, and `CallAuditMetadata`; historical pre-#60 rows keep
  old stage labels (alpha no-compat, no migration). Assertions updated in
  the same commit, including the provenance projector fixture.
- Spies deleted (`registry.rs::SpyBackend`, `observation.rs::ObsSpyBackend`
  - `HostFsBackend`, `mutation.rs::CreateSpyBackend`/`MutSpyBackend`,
  `edit/apply.rs::HostFsBackend`, `effect_contract_tests::SupportFsBackend`,
  lattice-api `TestFsBackend` ×2). Tests now run tempdir + the real
  `LocalWorkspace`. The only fault injection is
  `LocalWorkspace::fail_next_install` (one-shot boolean, gated
  `#[cfg(any(test, feature = "test-fault-injection"))]`, covering
  `install_atomic` and `create_excl`) — not a composable seam.
- Behavior fixes surfaced by running tests against the real workspace:
  grep/find paths are workspace-root-relative (was spy-relative); the
  write-stage rollback test now fails installs through a read-only
  directory (atomic rename legitimately replaces read-only files).
- `cep_warn_harness`: regenerated `docs/lancet-warn-report.md` (missing
  since 59d4c44c — pre-existing failure); all four overreach-capable
  methods still produce `effect_warning` after the metadata cleanup.

**Verification:**

- `rg -n "ExecutionBackend|ExecutionRouting|UpperContext|NullExecutionBackend|RoutingInvocationState" crates --type rust` — zero.
- `cargo build --workspace --all-targets` — clean (0 errors, 0 new warnings).
- `cargo test -p lancet-native --lib -- --test-threads=1 --skip fetch_effect_contract_negative` —
  108 passed (L-26-01 pre-existing DNS-blackhole hang documented in FINDINGS.md).
- `cargo test -p lancet-native --test cep_warn_harness --test policy_integration --test prov_isomorphism` — green.
- `cargo test -p lancet-audit` — 34. `cargo test -p lancet-api` — 17.
- `cargo test -p lattice-api` (lib + all integration incl. `direct_local_acceptance`
  10/10 product smoke identical pre/post scaffold removal) — all green.
- Serial matrix (`--test-threads=1`): lancet-protocol 36, lancet-catalog 27,
  lancet-dispatch 5, lancet-cep 7, lancet-authority 10, lattice-profile 96,
  lattice-session 2, lattice-agent 0, lattice-llm 133, lattice-tui 3,
  lattice-cli 4+5, net-guard 15 — all green.
- Agent-visible catalog remains exactly the closed five methods
  (`direct_local_runtime_exposes_closed_five_method_catalog`).

**Changed files:** `crates/lancet-native/src/{lib.rs,execution_backend.rs(deleted)}`,
`crates/lancet-native/src/workspace/{mod.rs,types.rs,local.rs}`,
`crates/lancet-native/src/server/{registry.rs,transport.rs,native_registry.rs,call_durable_metadata.rs}`,
`crates/lancet-native/src/handlers/{observation.rs,mutation.rs,external.rs,session_state.rs,scaffold.rs}`,
`crates/lancet-native/src/edit/{storage.rs,apply.rs,types.rs}`,
`crates/lancet-native/src/enforcement/{guard_l1.rs,preflight.rs}`,
`crates/lancet-native/src/effect_contract_tests/support.rs`,
`crates/lancet-native/Cargo.toml`, `crates/lancet-native/tests/cep_warn_harness.rs`,
`crates/lancet-audit/src/{audit.rs,audit_tests.rs}`,
`crates/lancet-audit/src/provenance/projector.rs`,
`crates/lancet-api/src/{lib.rs,runtime.rs}`,
`crates/lattice-api/src/turn/cep_tests.rs`,
`crates/lattice-api/tests/{cep_round_trip.rs,dogfood_impact_matrix.rs,cep_mode_integration.rs,direct_local_acceptance.rs}`,
`docs/lancet-warn-report.md`, `FINDINGS.md`, `crates/lancet-native/AGENTS.md`,
`crates/lancet-api/AGENTS.md`, `crates/lancet-audit/AGENTS.md`, `FIX.md`.

## Codeberg #39: unified Policy-Visible Workspace read visibility — 2026-07-17

Severity: FEATURE (ADR 0003 Migration Sequence step 4 / Workstream A).

**Problem:** Observation (read/list/grep/find) exposed every resolvable Local
path. Gitignored secrets (e.g. `.env`) were Agent-visible; there was no
profile surface for ignored-path inclusion or read denial; System Path
Exclusions (`.git` / `.lattice`) were not enforced as a non-relaxable
runtime rule on the observation path.

**Fix summary:**

- `lattice-profile::LancetWorkspacePolicySection` gains
  `included_ignored_read_patterns` and `denied_read_patterns` (fail-closed
  unknown fields; same project-relative pattern validation as write deny).
- `lancet-authority` adds pure `policy/visibility.rs`:
  System Exclusion > denied_read > included_ignored > gitignore default.
  `WorkspacePolicy` carries the two new fields and builds
  `ReadVisibilityPolicy`; write blacklist / `authorized_region_for` unchanged.
- `lancet-native` adds per-Tool-call gix visibility engine
  (`workspace/visibility.rs`, isolated open, index + exclude stack, no
  subprocess). Non-git roots treat paths as non-ignored.
- `observe.workspace` read/list/grep/find share one decision. Hidden reads
  return the same Agent-facing not-found shape; reason is audit-only
  (`system_exclusion` / `read_denied` / `ignored_default`). Grep/find prune
  hidden dirs unless an include pattern may still reach inside.
- Workspace dependency: `gix = "0.85"`.

**Verification:**

- `cargo build --workspace --all-targets` — clean.
- `cargo test -p lancet-authority -- --test-threads=1` — green (incl. 7
  visibility unit tests).
- `cargo test -p lancet-native -- --test-threads=1` — 118 + 1 + 5 + 3 green
  (incl. gix engine tests + four-op policy-visible observation tests).
- `cargo test -p lattice-profile -- --test-threads=1` — 96 green.
- `cargo test -p lattice-api --lib -- --test-threads=1` — 98 green.
- `cargo test -p lattice-api --test direct_local_acceptance --test cep_round_trip -- --test-threads=1` — green.
- Closed five-method catalog unchanged.

**Changed files:** `Cargo.toml`,
`crates/lancet-authority/src/{lib.rs,policy/{mod.rs,default.rs,visibility.rs}}`,
`crates/lancet-authority/tests/policy_tests.rs`,
`crates/lattice-profile/src/schema.rs`,
`crates/lattice-api/src/turn/cep.rs`,
`crates/lancet-native/Cargo.toml`,
`crates/lancet-native/src/workspace/{mod.rs,local.rs,visibility.rs}`,
`crates/lancet-native/src/handlers/observation.rs`,
`crates/lancet-native/src/handlers/mutation.rs` (WorkspacePolicy literals),
`crates/lattice-api/tests/{direct_local_acceptance,dogfood_impact_matrix,cep_round_trip}.rs`,
`FINDINGS.md`, `FIX.md`.

---

## Codeberg #40: removed Upper configuration, Microsandbox, and lattice-workspace residue — 2026-07-17

Severity: FEATURE (ADR 0003 Migration Sequence steps 4–6 / Workstream A; closes M-LOCAL-01 with #59–#61).

**Problem:** After the Direct-Local runtime (#59–#61) stabilized, the repo
still carried the retired Upper/MSB surface: the mandatory `[workspace]`
profile section (egress/credentials/image/lifecycle limits), the
`lattice-workspace` crate and `microsandbox` dependency, MSB/KVM harness
scenarios behind `LATTICE_HARNESS_MSB`, the retained-snapshot ops-state
chain (`set_snapshot_lost`/`get_snapshot_lost`, `ops/<oid>.json`), the
`lattice-session` promoted-commit builder, dead `SessionApi` wrappers, and
stale active documentation describing the sandbox path.

**Fix summary:**

- **profile**: `AgentProfile.workspace` and the `WorkspaceSection` /
  `EgressPolicyConfig` / `CredentialRef` types and lifecycle-limit
  constants deleted. Legacy profiles declaring `[workspace]` now fail
  strict parsing with a named `ForbiddenSection` error (added to the
  loader pre-scan). Generated templates emit only `[lancet.workspace]`
  policy. The local dogfood profile `.lattice/agents/default/agent.toml`
  was rewritten on disk to the Direct-Local shape (also dropping retired
  `[lancet.command]` / `[lancet.verify]` / `[lancet.isolation]` dead
  surface and the invalid allowlist write policy); it is an ignored
  private path and is intentionally not committed. `[lancet.network]` /
  `[lancet.workspace]` (blacklist + read visibility) unchanged.
- **lattice-session**: `set_snapshot_lost`/`get_snapshot_lost` removed
  from the `SessionGraphStore` contract, `GitSessionGraphStore`, the
  `ops/<oid>.json` sidecar machinery, and the SQLite→Git migration write
  path (extraction no longer reads `graph_snapshot_state`); the
  consumer-less `derive_snapshot_name`/`validate_snapshot_name` retained
  snapshot helpers deleted. `promotion.rs` + `tests/promotion_commit.rs`
  deleted.
- **lattice-api / lattice-cli**: dead `SessionApi::{commit_prepared_turn,
  get_turn_commit, get_snapshot_lost}` wrappers removed (trait methods
  stay — covered by git-store tests); `ProductionTurnDepsInput.loaded_profile`
  residual parameter removed; `finalize_turn` sheds the unused legacy
  `session_id`/`prepared_commit_id` params; test fixtures drop the
  `[workspace]` section; `attach_upper_fixture` no-op helper deleted;
  `git_store_snapshot_lost.rs` deleted.
- **crate deletion**: `crates/lattice-workspace/` removed (adapter, guest
  Git baseline/candidate flow, warm templates, credential injection,
  egress planning, KVM probes, image-specific tests); root `Cargo.toml`
  drops the member and the `microsandbox` dependency;
  `cargo tree -i lattice-workspace` was empty before deletion.
- **harness**: `msb_full_turn.rs`, `layer_guard.rs` (probe +
  `LATTICE_HARNESS_MSB` gate), and the missing-credential scenario
  deleted; `ScenarioLayer::Microsandbox` removed. `pty_tui_smoke` now
  runs Direct-Local in the ordinary lane (no KVM, no `#[ignore]`).
  New `tests/direct_local_turn.rs` skeletons (#46 owns the full matrix):
  plain-text turn commits + exit 0, session reopen after an external
  (non-LATTICE) edit+commit with transcript continuity, and
  `mutate.create` denied by `[lancet.workspace].denied_write_patterns`
  (stream shows failed tool event, nothing on disk, turn recovers).
  `SseScript::Sequence` added to the fake LLM for multi-round scripts.
- **native guard**: `WorkspacePathError::UpperRootAlias` renamed
  `SandboxRootAlias`; the retired `/workspace` guest alias stays rejected
  so stale agent prompts fail closed.
- **docs**: root `AGENTS.md`, `README.md`, `docs/ARCHITECTURE.md` (layer
  graph, data flow, abstractions, removed-subsystems), `docs/CONFIGURATION.md`
  (schema tables, examples, env vars; also drops stale `[lancet.command]` /
  `[lancet.verify]` sections that no longer deserialize), `docs/GETTING-STARTED.md`,
  `docs/TESTING.md`, `docs/TEST-TRIAGE.md` (MSB lane re-targeted to #46),
  `docs/LANCET.md` transition notice, crate `AGENTS.md` files. Historical
  ADR/FIX records untouched.

**Verification:**

- `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` — clean
  (two pre-existing #61 breakages fixed in separate commits first: rustfmt
  drift in 14 files, 3 clippy lints in lancet-native).
- `cargo build --workspace --all-targets` — clean.
- Per-crate `timeout 600 cargo test -p <c> -- --test-threads=1` for all 16
  crates — green: lancet-protocol 36, lancet-catalog 27, lancet-dispatch 5,
  lancet-cep 7, lancet-audit 34, lancet-authority 142, lancet-native 127,
  lancet-api 17, lattice-profile 80, lattice-session 66, lattice-agent 24,
  lattice-llm 133, net-guard 15, lattice-api 98 + 17 suites, lattice-tui 3,
  lattice-cli 135.
- `cargo xtask harness-boundary` — PASS.
- Product smoke with no KVM/MSB daemon/OCI registry: `cargo test -p
  lattice-system-harness -- --test-threads=1` — 88 green incl. the three
  Direct-Local turn scenarios and the PTY smoke in the ordinary lane.
- Network policy regression: lancet-authority policy tests, net-guard
  URL/DNS safety tests, and fetch.web effect-contract tests all green —
  `[lancet.network]` behavior unchanged.
- Agent-visible catalog remains exactly the five methods
  (`direct_local_runtime_exposes_closed_five_method_catalog`).
- Residue scan `rg -i "microsandbox|upper|msb" crates --type rust`:
  only historical `#59/#40` comments, negative assertions
  (`upper_workspace_id` must not appear in audit metadata), and unrelated
  `uppercase` matches remain.

**Changed files:** `Cargo.toml`, `Cargo.lock`,
`crates/lattice-profile/src/{schema,defaults,lib,load,discovery}.rs`,
`crates/lattice-session/` (graph.rs, git_graph/{mod,commit,helpers}.rs, migrate/{extract,write}.rs, lib.rs, AGENTS.md; −promotion.rs, −tests/promotion_commit.rs),
`crates/lattice-api/` (session.rs, turn/{cep,execute,lifecycle,test_support,live_tests}.rs, tests/{usage_turn,release_e2e,consistency,audit_turn_provenance,support/mod}.rs; −tests/git_store_snapshot_lost.rs),
`crates/lattice-cli/src/commands/run_tests.rs`,
`crates/lancet-native/src/workspace/types.rs`,
`crates/lattice-system-harness/` (−tests/msb_full_turn.rs, −src/layer_guard.rs; +tests/direct_local_turn.rs; fake_llm/scenario/world/lib/driver/session_send/pty_tui/artifacts.rs, tests/{pty_tui_smoke,fast_session_cli,fast_audit_cli,fast_config_cli}.rs, AGENTS.md),
−`crates/lattice-workspace/` (whole crate), `AGENTS.md`, `README.md`,
`docs/{ARCHITECTURE,CONFIGURATION,GETTING-STARTED,TESTING,TEST-TRIAGE,LANCET}.md`,
`FINDINGS.md`, `FIX.md`.

**FINDINGS ledger:** M-LOCAL-01 closed (Workstream A complete); M-33-01/02/03/04/06,
L-21-02, L-34-02 closed as moot with the deleted adapter/probe/porcelain;
T-23-01 re-targeted — 54 ex-MSB elevations now owned by #46 Direct-Local
scenarios; T-23-02 residual note updated.

## Codeberg #39 follow-up: visibility traversal separation and durable evidence — 2026-07-17

Severity: MEDIUM (post-review correctness and audit evidence).

**Problem:** An ignored directory with an explicitly included descendant was
both navigable and emitted by parent list/find results, leaking the hidden
ancestor name. Successful read/list/grep/find calls also lacked durable
visibility-rule evidence, and recursive walkers discarded filtered decisions.

**Fix summary:**

- Split entry visibility from directory descent in the native visibility
  engine. Exact includes may navigate through an ignored ancestor without
  making that ancestor visible; directory-glob includes still make the
  ancestor itself visible.
- List and find emit only visible entries while grep/find recurse only through
  descendable directories. Explicit observation of a navigable hidden
  directory remains supported and exposes only visible children.
- Added per-call, content-free `visibility_evidence` success metadata with an
  `included_ignored` presence bit and deterministic `filtered_by` categories
  (`system_exclusion`, `read_denied`, `ignored_default`). No paths, contents,
  hashes, or counts are recorded. Direct-denial metadata and Agent-facing
  not-found behavior remain unchanged.
- Grep/find now use one visibility engine from root assessment through the
  recursive walk, so one Tool call accumulates coherent evidence.

**Verification:**

- `cargo fmt --all --check` and targeted `rustfmt --edition 2024 --check` on
  all modified Rust files — passed.
- `cargo test -p lancet-native -- --test-threads=1` — 124 unit, 1 warning
  harness, 5 policy integration, and 3 provenance tests passed.
- `cargo clippy -p lancet-native --all-targets -- -D warnings` — passed.
- `cargo test -p lancet-audit -- --test-threads=1` — 34 passed.
- `cargo test -p lattice-api --test cep_round_trip --test direct_local_acceptance -- --test-threads=1` — 5 + 10 passed.
- `cargo test -p lattice-api --test audit_turn_provenance -- --test-threads=1` — 1 passed.

**Changed files:** `crates/lancet-native/src/workspace/{local,visibility}.rs`,
`crates/lancet-native/src/handlers/observation.rs`,
`crates/lancet-native/src/server/native_registry.rs`,
`crates/lancet-native/AGENTS.md`, `FIX.md`.

## Gix migration review cleanup: restored workspace formatting — 2026-07-18

Severity: LOW (format-only).

**Problem:** Removing obsolete `std::fs` imports from seven gix-backed
`lattice-api` integration tests left adjacent blank lines between their module
documentation and imports, so workspace rustfmt validation failed.

**Fix summary:** Removed the redundant blank line in each affected test; no
behavior or test logic changed.

**Verification:** `cargo fmt --all --check` — passed; `git diff --check` —
passed.

**Changed files:**
`crates/lattice-api/tests/git_store_{branch_checkout,diff,head_transcript,profile_snapshot,reset,session_create_open,session_prefix_resolve}.rs`,
`FIX.md`.

## Codeberg #24 review cleanup: remove dead flexibility and impossible branches — 2026-07-23

Severity: LOW (behavior-preserving simplification and review-record hygiene).

**Problem:** The completed #46 series left two unused public harness wrappers,
configurable HTTP fixture fields that every caller fixed to `200 text/plain`, an
impossible `cancel_poll` fallback branch, duplicated timeout error construction,
stale crate-local change records, and a closed T-23-01 entry in the open section
of `FINDINGS.md`.

**Fix summary:**
- Removed the uncalled `create_session` / `send_prompt` wrappers and root
  re-exports; `SessionSendDriver` remains the only used session-turn driver.
- Reduced the HTTP fixture to its used fixed `200 OK` / `text/plain` contract.
- Made `cancel_poll` resolve as `()` only when cancelled, deleting the unreachable
  boolean fallback while preserving stalled-stream cancellation.
- Shared one `InvalidField` construction for timeout validation while preserving
  the zero and upper-bound diagnostic text.
- Updated affected crate guidance, moved T-23-01 to `OBSOLETE / CLOSED`, and
  recorded the three residual #46 review gaps as T-24-01, T-24-02, and L-24-03.
- Rust source/harness diff: 30 insertions, 86 deletions (net `-56` lines).

**Verification:**
- `cargo +1.96.0 fmt --all -- --check` — passed.
- `cargo +1.96.0 test -p lattice-profile llm_timeout_secs` — 5 passed.
- `cargo +1.96.0 test -p lattice-agent --features test-utils cancel_during_stalled_stream_terminates_cancelled` — 1 passed.
- `cargo +1.96.0 test -p lattice-system-harness --lib fixture_http` — 1 passed.
- `cargo +1.96.0 test -p lattice-system-harness --no-run` — all harness targets compiled.
- `cargo +1.96.0 xtask harness-boundary` — passed.
- Built `lattice`, then ran Direct-Local `direct_failures` + `direct_tools` with
  `LATTICE_BIN` and one test thread — 7 + 7 passed.
- Targeted clippy for `lattice-profile`, `lattice-system-harness`, and
  `lattice-agent` (test-utils), all targets, `-D warnings` — passed.
- `git diff --check` and pi-lens diagnostics — passed.

**Changed files:**
`crates/lattice-agent/{AGENTS.md,src/agent_loop/mod.rs}`,
`crates/lattice-profile/{AGENTS.md,src/load.rs}`,
`crates/lancet-authority/AGENTS.md`, `crates/lattice-api/AGENTS.md`,
`crates/lattice-system-harness/src/{fixture_http,lib,session_send}.rs`,
`crates/lattice-system-harness/tests/direct_tools.rs`, `FINDINGS.md`, `FIX.md`.

## Codeberg #24 review closures: root escape, replan identity, and private test support — 2026-07-23

Severity: MEDIUM verification gaps (T-24-01/T-24-02) + LOW maintainability
cleanup (L-24-03).

**Problem:** The Direct-Local matrix lacked explicit external-binary root-escape
coverage for all observation modes and did not pin stale edits to the public
`ReplanRequired` identity. Nine integration targets also repeated the same
harness-only driver/evidence/fake-LLM plumbing.

**Fix summary:**
- Extended `direct_observation` with traversal and absolute outside-root requests
  for read/list/grep/find. The scenario requires eight public `GuardDenied` CEP
  finishes, exact denial text, no outside-canary leakage, unchanged outside bytes,
  and a completed Turn.
- Extended stale mutation coverage to require public `cep_call_finished` fields
  `method=mutate.edit`, `ok=false`, and `error_code=ReplanRequired`, while
  preserving generic Tool failure, external bytes, and same-session reopen checks.
- Moved only shared integration-test plumbing into private `tests/support/mod.rs`
  across all nine Direct-Local targets. Scenario-specific fixtures and assertions
  remain local; no product/public API, manifest, or dependency changed.
- Direct-Local test diff including the new support module: 417 insertions, 1072
  deletions (net `-655` lines).

**Verification:**
- `cargo +1.96.0 fmt --all -- --check` — passed.
- `cargo +1.96.0 xtask harness-boundary` — passed.
- `cargo +1.96.0 build -p lattice-cli --bin lattice` — passed.
- Focused `direct_observation` — 1 passed. The first run exposed the real public
  classification as `GuardDenied` rather than the planned `PathContainment`; the
  assertion was corrected and the rerun passed.
- Focused `direct_mutation_stale_edit_and_external_visibility` — 1 passed.
- Full `lattice-system-harness` suite with `LATTICE_BIN` and one test thread —
  131 passed across unit, Direct-Local, fast CLI, and PTY targets.
- `cargo +1.96.0 clippy -p lattice-system-harness --all-targets -- -D warnings` — passed.
- `git diff --check`, no staged files, LSP/pi-lens diagnostics — passed.
- Independent final subagent review found no blocker/regression and approved closing
  T-24-01, T-24-02, and L-24-03.

**Changed files:**
`crates/lattice-system-harness/{AGENTS.md,tests/support/mod.rs}`,
`crates/lattice-system-harness/tests/direct_{failures,local_turn,mutation,observation,operator,secrets,session_isolation,tools,turn_contract}.rs`,
`FINDINGS.md`, `FIX.md`.

## GitHub #133 review blockers: close `.env` diagnostic and substitution leaks — 2026-08-01

Severity: HIGH (credential/endpoint disclosure and untrusted routing input).

**Problem:** The initial issue #99 trust boundary filtered rejected keys only
after `dotenvy` had added them to substitution state, forwarded malformed-line
parse errors verbatim, and redacted proxy userinfo only when a URL carried an
explicit scheme. A hostile workspace could therefore launder an endpoint into
an allowed variable or expose credentials through doctor output and persisted
failure artifacts.

**Fix summary:**
- Raw substitution dependencies on refused `.env` keys are tracked in dotenv
  occurrence order, with leading/inline comments and repeated assignments
  matching dotenvy's parsing semantics; transitive and process-shadowed
  laundering is refused without rejecting a safe first occurrence.
- `LineParse` diagnostics now expose only a byte offset, never the source line.
- Proxy userinfo redaction covers both schemeful and reqwest-supported
  scheme-less forms in text and JSON output.
- Doctor report internals retain typed `ValueSource` and `RejectReason` values;
  strings are produced only at the rendering boundary.
- Harness failure-artifact scans now include each scenario-specific secret.

**Verification:**
- Focused RED/GREEN unit and fast-binary tests covered substitution laundering,
  malformed lines, and scheme-less proxy credentials.
- `cargo test -p lattice-cli` — passed.
- `LATTICE_BIN=target/debug/lattice cargo test -p lattice-system-harness --test fast_env_boundary`
  — 13 passed.
- `cargo xtask ci` in the host session — all formatting, clippy, build,
  boundary, codegen, workspace test, harness, and artifact checks passed.
- `git diff --check` — passed.

**Changed files:** `Cargo.lock`,
`crates/lattice-cli/{AGENTS.md,Cargo.toml,src/commands/doctor.rs,src/env_boundary.rs}`,
`crates/lattice-system-harness/tests/fast_env_boundary.rs`, `FIX.md`.
## GitHub #132 review blockers: bound observation at the host trust boundary — 2026-08-01

Severity: HIGH (workspace disclosure/budget accounting and durable evidence
correctness) + MEDIUM (configuration drift and structured refusal fidelity).

**Problem:** The initial `observe.workspace` Component migration duplicated
observation limits, let `read_file` allocate and expose the full file to the
guest before slicing, inferred truncation incorrectly at exact limits, lost
structured host refusals in some paths, and did not persist the actual
Policy-Visible Workspace decision in queryable evidence.

**Fix summary:**
- Reused `ObservationPolicyConfig` end to end and added a bounded local-file
  read that rejects both pre-existing and concurrent growth over the ceiling.
- Made the host render the final read window, including line numbers and
  truncation/no-content markers, and charge that exact exposed content; the
  Component only wraps the host result in the outer response union. Truncation
  is derived by probing one item beyond the limit.
- Preserved typed authorization/input refusals and denial evidence for every
  failed host operation.
- Added content-free visibility facts to `ObservedEffect`, persisted them in
  four queryable SQLite columns, and added an additive v3-to-v4 migration with
  legacy-row preservation.
- Regenerated WIT, descriptor identity, host composition, and the pinned
  `observe.workspace` Component artifact.

**Verification:**
- `cargo test -p lancet-protocol -p lancet-audit -p lancet-runtime` — passed.
- `cargo xtask tool-build` — rebuilt and pinned the Component artifact.
- `cargo xtask ci` — all formatting, clippy, build, boundary, codegen,
  workspace test, Direct-Local observation, and reproducibility checks passed.
- `git diff --check` — passed.

**Changed files:** `crates/lancet-{protocol,audit,runtime,native}/**`,
`tools/observe-workspace/**`, `xtask/src/tool_codegen.rs`, `FIX.md`.

## GitHub #31 review blockers: harden Component workspace mutations — 2026-08-03

Severity: HIGH (stale-write/linearization, cancellation, and durable-history
correctness) + MEDIUM (structured error/evidence fidelity and hermetic tests).

**Problem:** The initial `mutate.create` / `mutate.edit` Component migration
left expected-revision validation separated from installation, allowed a stale
edit probe to reinstall old bytes, collapsed semantic diagnostics into generic
execution failures, omitted the install-boundary cancellation check, and
reported success after private-history persistence failed. The write ceiling,
wasm32 occurrence conversion, mutation-operation measurement, and networked
test fixtures also diverged from their contracts.

**Fix summary:**
- Moved the final revision check, cancellation check, and atomic replacement
  under one host-side mutation linearization point. No-replace create now
  writes and syncs a same-directory temporary inode, rechecks cancellation,
  atomically links the complete inode at the absent target, cleans the temp,
  and fsyncs the parent directory.
- Added host-owned semantic refusal operations so ambiguous/missing edits retain
  `ReplanRequired`, validation/no-op failures retain `SchemaMismatch`, and the
  write ceiling returns `ToolResourceExhausted` without changing the target.
- Made post-install history persistence failure an explicit `ExecutionFailed`
  while retaining the installed mutation effect, and moved the install →
  history → evidence sequence behind a gate shared by every mutation Host over
  the same `LocalWorkspace`. A parent-directory fsync failure after atomic name
  installation now follows the same installed receipt path: history and
  installed evidence are retained before `ExecutionFailed` reaches the caller.
- Rejected occurrences above `u32::MAX` before wasm32 conversion, removed
  operation-cell `context_measure`, and keyed structured host refusals by call.
- Replaced public-network test calls with local HTTP fixtures, added direct
  Component acceptance for semantic errors and external races, added a
  same-target double-create harness proving one winner plus one
  `ReplanRequired`, added product-level pre-cancel/install-fault no-write
  coverage and mutation evidence-field assertions, and removed legacy
  catalog/policy expectations for the migrated mutation Tools.

**Verification:**
- `cargo xtask tool-codegen --check` — passed.
- Focused runtime host/local-workspace mutation tests, direct-local acceptance,
  workspace-history acceptance, and native Component round trips — passed.
- `cargo xtask ci` — all formatting, Clippy, debug build, architecture
  boundaries, generated-artifact checks, workspace tests, Direct-Local harness,
  and reproducible Component digest checks passed.
- `git diff --check` — passed.

**Changed files:** `crates/lancet-{protocol,audit,authority,cep,runtime,native}/**`,
`crates/lattice-api/**`, `crates/lattice-system-harness/**`,
`tools/mutate-{create,edit,shared}/**`, `xtask/src/tool_codegen.rs`,
`docs/{LANCET.md,plans/**}`, root/scoped `AGENTS.md`, `CONTRIBUTING.md`, `FIX.md`.

## PR #134 CI: make the supported host lane Linux-only — 2026-08-03

Severity: MEDIUM (unsupported-platform CI failure cancelled the supported
Linux gate).

**Problem:** The ordinary CI workflow still declared an Ubuntu/macOS matrix
even though ADR 0004 limits the built host target to Linux. On PR #134 the
macOS job failed in `fast_dotenv_agents_dir_cannot_redirect_profile_lookup`,
and the matrix fail-fast behavior cancelled the Ubuntu job before it could
provide the supported-platform verdict.

**Fix summary:**
- Replaced the ordinary CI platform matrix with one `ubuntu-latest` job.
- Removed the matrix-dependent failure-artifact name.
- Documented Linux as the only current release and CI host target; macOS and
  Windows are explicitly unsupported until multi-platform work is resumed.
- Left incidental portable implementation branches intact without treating
  them as support commitments.

**Verification:**
- Linux-only workflow assertion (`macos-latest` / `windows-latest` absent from
  `.github/workflows`) — passed after reproducing the pre-fix failure.
- YAML parse check for `.github/workflows/ci.yml` — passed.
- `cargo xtask ci` — all formatting, Clippy, debug build, architecture
  boundaries, generated-artifact checks, workspace tests, Direct-Local harness,
  and reproducible Component digest checks passed.
- `git diff --check` — passed.

**Changed files:** `.github/workflows/ci.yml`, `CONTRIBUTING.md`,
`docs/TESTING.md`, `FIX.md`.

## Linux-only host contract: remove non-Linux compatibility paths — 2026-08-04

Severity: MEDIUM (the documented Linux host target still carried non-Linux
fallback behavior and portability-driven implementation residue).

**Problem:** The Linux-only CI declaration did not remove project-owned
generic-Unix, Windows, macOS, and BSD compatibility paths. The system harness
still compiled no-op non-Unix process handling and skipped Linux `/proc`
assertions elsewhere; audit storage fell back to non-Linux state directories;
tests normalized Windows separators; and Local Workspace errno handling kept a
BSD-only branch. Documentation also described Linux as a current CI choice
rather than the sole host contract, leaving room for future portability work to
weaken Linux behavior.

**Fix summary:**
- Removed host-platform `cfg` branches and target-specific dependency sections
  from the maintained crates. Linux process groups, signals, PTY cleanup,
  symlink/permission tests, and `/proc` assertions now run directly.
- Removed non-Linux audit-state fallback, BSD `EMLINK` handling, and Windows
  separator normalization from Linux test helpers.
- Established Linux as the only development, build, test, deployment, and
  runtime host in root, contributor, architecture, testing, and getting-started
  documentation. The `wasm32-unknown-unknown` Source Tool target is explicitly
  a guest artifact format, not another host.
- Recorded the architecture rule that hypothetical future platform support
  cannot justify portable no-op fallbacks or weaker Linux primitives; expanding
  the host set requires a separate architecture decision and a complete native
  implementation.

**Verification:**
- Focused `lancet-runtime`, `lattice-session`, `lattice-system-harness`, and
  `lattice-api` tests — passed.
- `cargo xtask harness-boundary` — passed.
- `cargo xtask ci` — passed on the Linux host, including formatting, Clippy,
  debug build, architecture boundaries, all workspace tests, Direct-Local/PTY
  harness scenarios, generated-artifact checks, and Component digest
  reproduction.
- `git diff --check` — passed.

**Changed files:** root Cargo/workflow/platform documentation; active
`docs/**`; `crates/lancet-{audit,authority,native,runtime}/**`;
`crates/lattice-{api,cli,session,system-harness}/**`; root/scoped `AGENTS.md`;
`FINDINGS.md`; `FIX.md`.

## GitHub #114: retire Turn/audit provenance and attestation — 2026-08-09

Severity: MEDIUM (the operator audit surface still exposed a second, retired
provenance/attestation model instead of the durable Cell-evidence records).

**Problem:** Query, tail, replay, and status did not share one typed,
finalized-row adapter. Native and Component calls were split by the retired
projection path; raw metadata, provenance fragments, unsigned Turn statements,
and `attestation_records` remained reachable even though versioned Cell
evidence was already the durable record.

**Fix summary:**
- Replaced operator query/tail/replay outputs with serde DTOs over every
  finalized SQLite audit row and a compact replay-call trace.
- Added optional typed `OperatorCellEvidence` to query, tail, replay, and
  LANCET status: native rows serialize `evidence: null`; Component rows resolve
  the existing versioned Cell evidence. Missing, dangling, unsupported,
  corrupt, or mismatched Component evidence is a read error.
- Removed the protocol provenance module, audit projector/canonicalizer,
  attestation writer/store API, old tests, and Turn-attestation writes; store
  open now drops the retired `attestation_records` table without a migration.
- Replaced the release gate's deleted replay test with a release-binary
  native/Component/mixed operator-audit and status matrix. Updated active
  architecture, audit, test, and crate guidance without changing artifact
  identity or bundle-provenance contracts.

**Verification:**
- `cargo test -p lancet-audit` — 81 passed.
- `cargo test -p lancet-api` — 8 passed.
- `cargo test -p lattice-cli` — 132 unit and 4 integration tests passed.
- `cargo test -p lancet-native --test cep_component_round_trip` — 17 passed.
- `cargo test -p lattice-api --test dogfood_impact_matrix` and
  `cargo test -p lattice-api --test release_e2e` — passed.
- Host exact `direct_operator_audit_session_lancet_cross_validate` — passed.
- `cargo xtask ci` — passed on the host, including Clippy, boundaries,
  workspace tests, Direct-Local harness, and Component digest verification.
- `git diff --check` — passed.

**Changed files:** `crates/lancet-{protocol,audit,cep,dispatch,native,runtime,api}/**`,
`crates/lattice-{api,cli,system-harness}/**`, `xtask/src/rollout.rs`, active
`docs/**`, root/scoped `AGENTS.md`, `FINDINGS.md`, and `FIX.md`.

## GitHub #114 review closeout: honor every finalized-row contract — 2026-08-10

Severity: HIGH (the first implementation hid valid pointerless Component rows
and left replay/status/release coverage weaker than the accepted contract).

**Problem:** The initial #114 implementation treated a finalized Component row
without `evidence_id` as corrupt, capped turn replay at 10,000 rows, loaded the
entire audit table before slicing status to 20, and reused one mixed
Direct-Local test as the release gate. Its first commit was also labeled
`refactor` despite deliberate public JSON/API/database behavior changes.

**Fix summary:**
- Made a missing pointer a valid `evidence: null` state for every finalized
  row. A present, readable, row-matching pointer materializes existing evidence
  independent of semantic stage; dangling, unsupported, corrupt, or mismatched
  pointers still fail the operator read. No native evidence producer was added.
- Removed the query cap from turn replay and added a 10,001-row regression;
  query retains its 10,000-row cap and inclusive cursor pagination.
- Added typed audit ordering and changed status to ask SQLite for
  `audit_id DESC LIMIT 20` before materialization. The regression places an
  unreadable 21st row outside the window and verifies newest-first output,
  verdict, duration, and optional evidence.
- Split the Direct-Local contract into native-only, Component-only, mixed, and
  22-to-20 bounded-status scenarios. Added a feature-gated
  `release_operator` target with independent N/C/M fixtures and assertions;
  rollout executes it instead of rerunning `direct_operator`.
- Rewrote the unpushed feature history to two commits headed by
  `feat(audit)!`, preserving a separate append-only records commit.

**Verification:**
- RED: pointerless Component query/status returned `missing its evidence_id
  pointer`; matching cross-stage pointer returned `non-Component audit row`;
  10,001-row replay returned 10,000; bounded status decoded an unreadable row
  outside its newest-20 window; rollout still named the `direct_operator` test.
- `cargo test -p lancet-audit` — 83 passed.
- `cargo test -p lancet-api` — 9 passed.
- `cargo test -p xtask` — 113 passed.
- Host `direct_operator` target — 6 passed, including independent N/C/M and
  exact 22-to-20 status coverage.
- Host feature-gated `release_operator_native_component_mixed_matrix` — 1
  passed against the built binary.
- `cargo xtask ci` — passed on the host, including formatting, Clippy,
  dependency boundaries, workspace tests, Direct-Local scenarios, and
  Component digest verification.
- `git diff --check` — passed.

No unresolved #114 residual remains for `FINDINGS.md`.

**Changed files:** `crates/lancet-audit/**`, `crates/lancet-api/**`,
`crates/lattice-system-harness/**`, `xtask/src/rollout.rs`, `docs/**`, scoped
`AGENTS.md`, and `FIX.md`.

## GitHub #143: `lattice-store` private-Git durability kernel + Session/WorkspaceHistory migration — 2026-08-16

| ID | Severity | Problem | Fix |
|---|---|---|---|
| #143 | FEATURE | Session graph and WorkspaceHistory each hand-rolled their own bare-repo init, locks, fsync, and reachability; mutable `.lattice/sessions/**` sidecars were a second, non-Git authority; the store auto-initialized on open, and session state lived under the configuration root. | Added the ADR 0005 domain-neutral `lattice-store` kernel and hard-cut both domains onto it. |

### What landed

- New Layer-0 `crates/lattice-store`: XDG `StateLayout` topology,
  `ProjectStoreKey` (domain-separated SHA-256 over the raw bytes of the full
  canonical Local Workspace root), typed strict-open/first-create with a
  `refs/lattice/meta` identity record (kernel format + domain store kind and
  schema version + Global/Project scope), object/tree/commit writes, ref CAS
  and multi-ref transactions, the standard per-repository write lock,
  fsync/rename publication barriers, atomic first-create (sibling temp +
  `renameat2(RENAME_NOREPLACE)` + parent fsync + winner/loser revalidation),
  retention pins, per-location capability probes, and typed store errors.
- `lattice-session`: mutable sidecars replaced by immutable `SessionState`
  commits at `refs/lattice/sessions/<id>/state` (canonical JSON, parent-chained)
  and session branches at `refs/lattice/sessions/<id>/heads/<branch>`; commits
  and resets publish head+state in one multi-ref transaction plus one
  `fsync_pending` barrier. `SessionGraphStore` gained `lock_execution`; every
  mutation takes the per-session `SessionExecutionLock` then the repository
  write lock, and `status` takes a non-blocking shared hold and reports
  `SessionBusy`. `create_session` takes a `ProjectStoreKey`; the store is
  strictly opened and never initializes on open.
- `lancet-runtime`: `WorkspaceHistory` is a project-scoped kernel store at
  `projects/<ProjectStoreKey>/workspace-history.git`. Normal recording writes
  no per-record refs (everything appended is an ancestor of `current`); a
  Rewind retains the abandoned tip through the kernel retention pin first.
  The per-project `WorkspaceOperationLock` is wired at the ADR 0005 §5 lock
  order: observation shared; mutation and rewind exclusive, strictly wider
  than the repository write lock taken underneath.
- `lattice-api`: new `LocalWorkspaceRoot` resolver (`root.rs`) shared by
  CLI and TUI, and `stores.rs` project-store composition. `session new` is
  the only first-create path: it resolves the root, derives the key,
  first-creates the project's stores, and only then publishes the
  SessionState. Every other path strictly opens; expected-missing stores
  fail closed with an actionable error.
- CLI/TUI address stores through the XDG state root
  (`$XDG_STATE_HOME/lattice/sessions.git`); the config-root directory keeps
  only the REPL history file.
- `invocations.git` is deliberately absent (owned by #139-A); no migration of
  old config-root session state or old WorkspaceHistory state; the
  transitional SQLite audit path is untouched until #139-B.

### Verification

- `cargo xtask ci` — full pass on the host (fmt, Clippy, boundaries,
  workspace tests, tool digests, harness scenarios), with `LATTICE_BIN`
  unset so the harness uses the freshly built worktree binary.
- New acceptance coverage:
  `crates/lattice-store/tests/integration.rs`,
  `crates/lattice-session/src/git_graph/tests/state_and_locks.rs`,
  the operation-lock tests at the end of
  `crates/lancet-runtime/src/history.rs`, and the rewritten
  `crates/lattice-api/tests/workspace_history_acceptance.rs`.
- Independent three-focus review found no BLOCKER; the MAJOR findings
  (flattened kernel errors into domain strings, vacuous/weak tests, stale
  docs) were fixed in the review-fix commit (`StoreError::InvalidInput`,
  typed `WorkspaceHistoryError::Kernel`, `SessionError::InvalidInput`,
  ref-lock contention no longer mapped to `CasConflict`).

### Changed files

`crates/lattice-store/**`, `crates/lattice-session/**`,
`crates/lancet-runtime/**`, `crates/lattice-api/**`, `crates/lattice-cli/**`,
`crates/lattice-tui/**`, `crates/lattice-system-harness/**`, `Cargo.toml`,
`docs/**`, scoped `AGENTS.md`, and `FIX.md`.

## GitHub #143 review follow-up: session and durability correctness — 2026-08-16

| ID | Severity | Problem | Fix |
|---|---|---|---|
| #143-R1..R14 | BLOCKER/MAJOR | Review identified unvalidated ProjectStoreKey bindings, incomplete Turn/Rewind locking, swallowed checkpoint errors, same-process flock ambiguity, missing publication barriers, unsafe retention ordering, global test-state leakage, Tokio-worker blocking, detached-HEAD fallback to `main`, dropped list errors, doctor fallback, process-CWD recovery side effects, overstated filesystem claims, and an unused kernel dependency. | Bound every Session path to its canonical Local Workspace key and paired StateLayout; hold SessionExecutionLock and the shared WorkspaceOperationLock across the full live Turn close and Rewind; make lock ownership explicit and validate every locked API's exact exclusive guard capability; add pre/post object/ref barriers and durable retention checks; isolate tests and offload blocking regions; fail closed for detached HEAD and list/doctor/init errors; make the probe/test language honest; remove `rustix` and delete the obsolete `open_workspace_history` seam. |

### Verification

- `cargo test -p lattice-store --lib -- --test-threads=1` — 43 passed.
- `cargo test -p lattice-store --test integration -- --test-threads=1` — 5 passed.
- `cargo test -p lattice-session --tests -- --test-threads=1` — 65 passed.
- `cargo test -p lattice-api --test rewind_acceptance -- --test-threads=1` — 17 passed.
- `LATTICE_BIN="$PWD/target/debug/lattice" cargo test -p lattice-system-harness --test direct_failures -- --test-threads=1` — 8 passed.
- `LATTICE_BIN="$PWD/target/debug/lattice" cargo test --workspace` — passed.
- `cargo fmt --all --check`, `git diff --check`, `cargo xtask lancet-boundary` — passed.
- `cargo xtask ci` — passed, including Clippy, build, boundaries, codegen,
  per-crate tests, harness scenarios, and Component digest verification.

The transitional SQLite audit key/hash split remains unchanged for #139-B.
No real XDG state was cleaned, and no staging, commit, push, or GitHub update
was performed.

### Changed files

`crates/lattice-store/**`, `crates/lattice-session/**`,
`crates/lancet-runtime/**`, `crates/lattice-api/**`, `crates/lattice-cli/**`,
`crates/lattice-system-harness/**`, `Cargo.lock`, and `FIX.md`.

## GitHub #118: lattice-tui full-screen shell — 2026-08-22

| ID | Severity | Problem | Fix |
| --- | --- | --- | --- |
| #118 | MAJOR | `lattice-tui` was an inline rustyline REPL (no alternate screen, no ratatui) with three known defects from #101 A9: no turn-stream idle timeout (a pending `stream.next()` hung forever), `error_summary` never displayed for failed tools, and a Ctrl-C bare window between watcher spawn and poll. | Rewrote `lattice-tui` as a full-screen ratatui/crossterm shell per `docs/plans/2026-08-22-issue118-tui-shell-design.md`: three regions (transcript / input / status), exhaustive `TurnEvent` match with no wildcard, generic tool status lines only (zero per-tool-name dispatch), pinned-follow viewport that never drags a scrolled-up reader, bracketed-paste never submits, Esc = cooperative cancel / clear-input, double-Ctrl-C exit. Idle-timeout constant sunk to `lattice_api::STREAM_IDLE_TIMEOUT_SECS` with a one-line re-export in `run.rs`; `error_summary` carried by `ToolState::Failed`; Ctrl-C is a raw-mode KeyEvent from the first frame (no watcher, no bare window). Deleted `runtime.rs`; `crates/lattice-cli/src/main.rs` unchanged. |
| #118-CJK | MINOR | Transcript wrap and input-height math used `chars().count()` as display width; CJK glyphs occupy two terminal cells, so Chinese text overflowed the viewport and the input cursor landed wrong. | Switched all display-width math in `view.rs` to `unicode_width` (already pinned in the workspace; new consumer, not a new dependency). Buffer char indexing in `input.rs` intentionally unchanged. Added a narrow-width CJK wrap test. |
| #118-PTY | BLOCKER (CI) | `pty_tui_smoke` runs in the normal CI lane; `wait_for_prompt` matched `>` in ANSI-stripped text (full-screen apps print no prompt) and `READ_BUDGET = 256 KiB` is exhausted in seconds by a frame-redrawing app. | Added `wait_for_raw` (raw-byte predicate), renamed `wait_for_prompt` → `wait_for_ready` matching the alternate-screen entry sequence `\x1b[?1049h`, raised `READ_BUDGET` to 4 MiB, updated the smoke call site. |
| #118-PTY-CR | BLOCKER (CI) | `send_line` terminated input with `\n`. The full-screen TUI runs in raw mode, where crossterm maps `\n` to Ctrl+J rather than `KeyCode::Enter` (crossterm #371); the prompt landed in the input buffer and no turn ever started, so the smoke hung until the CI-level timeout. The inline REPL tolerated `\n` because readline ran in cooked mode. | `send_line` now writes `\r`, what a real terminal sends for Enter in raw mode. |
| #118-PTY-DEADLINE | MAJOR | `drain_until` checked its deadline only *after* a blocking read returned. A full-screen TUI is silent whenever nothing changes, so the scenario's 30s/60s bounds never fired: the crate hit the CI timeout with no diagnostic, turning a one-line bug into a blind hunt. | Moved the single blocking read into a reader thread feeding an mpsc channel; `drain_until` waits with `recv_timeout`, fails fast when the PTY closes, and the timeout error now carries an ANSI-stripped tail of what the TUI did emit. |

### Review round 1 (double-axis: standards + spec), all applied

| ID | Severity | Problem | Fix |
| --- | --- | --- | --- |
| #118-R1 | BLOCKER | The stream idle timeout could never fire. `poll_turn` built a *relative* `tokio::time::timeout(300s, ...)` inside `tokio::select!`, and the 33 ms frame ticker wins that race ~30x a second, so the future was dropped and rebuilt with a fresh deadline before it could ever elapse — the #101 A9 defect 1 fix was functionally inert. | Hoisted an **absolute** `tokio::time::Instant` deadline into the loop, set when a turn starts and pushed forward only by events that actually arrive; `poll_turn` now uses `timeout_at`. Added `idle_deadline_survives_repeated_poll_rebuilds`, which drives a never-yielding stream through repeated rebuilds and asserts the bound fires *at* the original deadline. Verified by injection: the test fails against the old code with "deadline was reset by rebuilding: fired 200s late". |
| #118-R2 | MAJOR | Viewport math reserved the wrong input height: the draw path reserved *wrapped* rows (`input_visual_height`, 1–6) while `transcript_height` counted *logical* lines, so a long single line wrapping to six rows left follow-to-bottom and page size disagreeing with the screen and hid the last transcript rows. | `transcript_height` now calls the same `view::input_visual_height` the draw path uses. |
| #118-R3 | MAJOR | `pty_tui_smoke` could not fail on a broken `/quit`: it slept 500 ms and then dropped the driver, whose `Drop` kills the process group — so the survivor assertion only ever proved SIGKILL works. | Added `PtyTuiDriver::wait_for_exit`, which polls `try_wait` without killing; the scenario now requires the TUI to exit on its own before `Drop`. Verified by injection: with `/quit` made a no-op the test fails with the driver's diagnostic tail instead of passing. |
| #118-R4 | MINOR | `insert_paste` stripped every `\r`, so a paste whose newlines arrive as bare CR (a real terminal convention) collapsed into one line. | Normalise `\r\n` and lone `\r` to `\n` before splitting; the paste test now covers all three conventions. |
| #118-R5 | MINOR | Six places still described the deleted rustyline inline REPL: `README.md` (crate table + quick start), `docs/GETTING-STARTED.md`, `docs/ARCHITECTURE.md` (turn walkthrough + crate list), and the `lattice-tui` crate description. | All six updated to the full-screen ratatui shell, matching the new AGENTS.md Crate Roles row. |
| #118-R6 | MINOR | Four acceptance tests did not prove their names: the paste test resolved an unrelated printable key, the history test only moved the cursor, the tool-completion test had a single tool in flight (so "matching id" was unfalsifiable), and the locale test passed `false` by hand instead of exercising `locale_utf8()`. | Each strengthened to drive the named path: three paste conventions, real `History` prev/next, three concurrent tools with the other two required to stay `Running`, and a real `LC_ALL`/`LANG` swap with restoration. |
| #118-R7 | NIT | `App::new` built its history from `Path::new("")`, immediately overwritten by `run_app`. | `App::new` takes `session_dir`; the double construction is gone. |

### Verification

- `cargo fmt --all --check` — clean.
- `cargo test -p lattice-tui` — 24 unit tests pass (state machines only; one
  `TestBackend` smoke asserts the three regions render without panic).
- `LATTICE_BIN="$PWD/target/debug/lattice" cargo test -p lattice-system-harness` —
  `pty_tui_smoke` and `direct_failures` pass.
- `env -u LATTICE_BIN cargo xtask ci` — passed.

### Changed files

`crates/lattice-tui/**` (rewritten; `runtime.rs` deleted),
`crates/lattice-api/src/turn/execute.rs` (`STREAM_IDLE_TIMEOUT_SECS`),
`crates/lattice-cli/src/commands/run.rs` (one-line re-export),
`crates/lattice-system-harness/src/pty_tui.rs`,
`crates/lattice-system-harness/tests/pty_tui_smoke.rs`,
root `AGENTS.md`, `crates/lattice-tui/AGENTS.md`, `Cargo.toml`, `Cargo.lock`,
`docs/plans/2026-08-22-issue118-tui-shell-design.md`, `FIX.md`.
## GitHub #139-A: InvocationStore qualification and review repairs — 2026-08-17

| ID | Severity | Problem | Fix |
|---|---|---|---|
| #139-A-R1..R15 | BLOCKER/MAJOR | The initial non-production adapter did not preserve exact ordered plan authority, used open terminal strings and detached evidence, treated a published-but-undurable ref as ordinary failure, trusted a moved/truncated ledger ref, omitted clean-collision I/O closure, and measured simulated rather than real store paths. Re-reviews also found missing SessionStore CAS lineage, missing-session batch validation, permissive durable decoding, a publish sample on a classify-only path, stale dependency/docs statements, a torn multi-ref publication assumption, non-idempotent candidate revision durability retries, old UUID reuse, timing contamination, weak wire-shape assertions, and a qualification bundle without immutable source history. | Added exact `PlanReservation` authority and order-sensitive idempotence; closed terminal reason/material/error sums with atomic Cell evidence; ordered ledger-before-witness publication with lag recovery and linear-topology validation; retried post-publication durability barriers including same-stage candidate revision; prohibited any old UUID reuse; added pinned views, PendingTurn projection, and one SessionStore revision executor; rejected mixed-session batches and unknown durable fields; replaced qualification loops with real operations and corrected timing/fairness samples; added exact wire-shape tests and an immutable Git history bundle; aligned docs and dependency records. |

### Verification

- `cargo test -p lancet-audit -p lattice-session` — 270 passed, 2 qualification harnesses intentionally ignored.
- `LOCAL_LEDGER_STRESS=1 ... --ignored --exact` and `RESULT_FINALIZE_STRESS=1 ... --ignored --exact` — both real-operation harnesses passed; these diagnostic runs did not approve or populate `FinalizeBudget`.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo check --benches -p lancet-audit`, `bash -n benchmarks/invocation-store/run-qualification.sh`, and `git diff --check` — passed.
- `cargo xtask ci` — passed every gate, including all 27 crate suites and the system harness.
- `cargo test --workspace` — all tests passed except the known environment-sensitive `direct_failure_session_lock_serializes_same_session_turns`, which also fails on clean `main` at `62100b1b` on this host (project memory CONSTRAINTS #66); the same test passed under `cargo xtask ci`.
- Two independent final re-reviews returned PASS with no remaining blocker or major finding.

### Changed files

`AGENTS.md`, `Cargo.lock`, `FINDINGS.md`, `crates/lancet-audit/**`,
`crates/lattice-session/**`, `benchmarks/invocation-store/**`,
`docs/plans/2026-08-17-issue139a-decision-record.md`, and `FIX.md`.

## L-GH139A-01 / GitHub #139-A: bounded event ledger and measured finalization budget — 2026-08-18

| ID | Severity | Problem | Fix |
|---|---|---|---|
| L-GH139A-01 | MAJOR | The qualification implementation wrote the complete accumulated Invocation snapshot on every lifecycle transition, causing quadratic persistent bytes and a 90.55 s local-ledger wall p50. | Replaced snapshot commits with strict versioned append-only events, deterministic replay, incremental cooperating-write suffix validation, and a regression test that bounds every stored event independently of ledger length. The rejected 20+20 bundle remains immutable and annotated. |
| #139-A-R16 | MAJOR | The first proposed 10 s `FinalizeBudget` used the uncontended result-finalize maximum and ignored the accepted 50.31 s contended repository-lock wait. | Approved a 60 s monotonic budget covering lock acquisition, revalidation, publication, and retry delay; synchronized ADR 0005; and fixed Btrfs regression limits from the retained distributions. |
| L-GH139A-02 | MAJOR residual | Append-only events bound writes but cold pinned reads replay full project history. | Recorded an explicit non-production #139-B wiring gate in `AGENTS.md`, `FINDINGS.md`, and the approval record: add a bounded read strategy and cold-reopen/repeated-facts Btrfs measurements before production cutover. |

### Verification

- `cargo test -p lancet-audit -p lattice-session` — 271 passed; only the two explicit qualification harnesses were ignored.
- `cargo xtask ci` with `LATTICE_BIN` unset — passed fmt, Clippy, build, boundaries, codegen, all 27 crate suites, system harnesses, and Component checks.
- `benchmarks/invocation-store/run-qualification.sh` — retained 20 local-ledger plus 20 result-finalize Arch Linux/Btrfs release runs in `results/20260817T214110Z-7c64a4f3ec34/`; all correctness assertions passed.
- Accepted measurements: local-ledger wall p50/p95/max 52.82/101.77/112.37 s; maximum single lock wait 50.31 s; full-finalize p50/p95/p99/max 21.47/24.40/34.29/2041.60 ms; uncertain revalidation p99/max 0.145/0.212 ms; CAS retries zero by exclusive-flock structure.
- `git merge-base --is-ancestor 7c64a4f3ec34 HEAD`, `git bundle verify .../source-history.bundle`, and `git bundle list-heads .../source-history.bundle` — passed; the bundle contains exact source HEAD `7c64a4f3ec34944d944ce2c16565e9e5656ade5e`.
- Independent final spec and adversarial risk rereviews returned PASS with no blocker or major finding after the 60 s budget and read-scaling gate remediation.

### Changed files

`crates/lancet-audit/src/invocation_store.rs`, retained
`benchmarks/invocation-store/results/**` bundles, `AGENTS.md`, `FINDINGS.md`,
ADR 0005, both #139-A plan/approval records, and `FIX.md`.

## 2026-08-18 — GitHub #139-A second-review remediation (corrections and supersessions)

A second independent review of the unpushed #139-A branch returned
`REQUEST CHANGES`. This entry is append-only: the 2026-08-18 entries above are
retained verbatim, and the list below records exactly which of their claims
this round overturns.

### Claims overturned by this round

| Superseded claim | Where it was recorded | Why it is wrong now |
|---|---|---|
| `lancet-audit -> lattice-session` is a legitimate landed #139-A edge. | `AGENTS.md` graph + delta list + crate table, `crates/lancet-audit/AGENTS.md`, decision record "Ownership and dependency decisions". | ADR 0005 §19's exact post-#139-B graph has no such edge. As a normal dependency it made `lancet-api -> lancet-audit -> lattice-session` real, so an operator read pulled the LATTICE session layer into LANCET. Documentation was changed to match an implementation instead of the implementation matching the ADR. |
| `RESULT_FINALIZE_STRESS` qualified the finalize path; its 20-run distribution is accepted evidence. | `docs/plans/2026-08-18-issue139a-qualification-approval.md`, FIX.md 2026-08-18 "Verification", decision record. | The harness published Invocation terminals while the PendingTurn was still `Bound` and asserted `Bound` at the end of every step — a durable order the pure reducer classifies as corruption. It never measured ADR 0005 §12's real order. |
| The 60 s `FinalizeBudget` and the result-finalize regression thresholds are approved. | `#139-A-R16` row above, ADR 0005 §12, approval record. | Derived in part from the withdrawn group's 2.042 s maximum full-finalize latency. Withdrawn pending a corrected rerun; ADR 0005 §12 again states no approved number. |
| `L-GH144-01` is closed by unit-level truth-table tests. | FINDINGS.md, `crates/lancet-audit/AGENTS.md`. | The closure is real but the evidence was insufficient: there was no store-backed CAS/reopen closed loop. It now has one; the finding stays CLOSED on that stronger basis. |

### Fixes applied

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R2-01 | BLOCKER | `lancet-audit` depended on `lattice-session` as a normal dependency, violating ADR 0005 §19 and LANCET's independent embeddability. | Removed the edge. `PinnedLedgerView::project_facts`, the record/plan/binding projections, and `execute_collision_revision` left the production module; `lattice-session` is now a dev-dependency only. Cross-domain projection and the reconcile call live in the non-production seam `crates/lancet-audit/tests/invocation_qualification.rs`; applying a revision is session-domain I/O (`GitSessionGraphStore::execute_candidate_revision`). `cargo tree -p lancet-api -e normal` contains no `lattice-session`. |
| R2-02 | BLOCKER | A clean collision returned a `ReconciliationPlan` carrying a provisional `Interrupted(ProcessInterruptedDuringToolBinding)` outcome plus a single `ReviseBindingCandidates` action, so a non-final decision was shaped like a closed Turn. | `reconcile` now returns `ReconciliationDecision::{Closure, ReviseCandidates}`. `ReconciliationAction::ReviseBindingCandidates` was deleted. `CandidateRevisionRequest` carries the exact session, Turn, byte-exact durable `BindingPlan`, and observed colliding candidates, and carries no outcome. `execute_candidate_revision` verifies the request against the pinned `active-turn` tip and CAS-pins that tip. A collision at `Bound` is now typed corruption, because `Bound` is published only after the exact plan is durably reserved. |
| R2-03 | BLOCKER | The collision path had only scattered unit tests: no real store-backed CAS/reopen closed loop. | Added `clean_collision_revises_candidates_end_to_end_over_real_stores`: foreign owner in a real `InvocationStore` → pinned facts → projection → pure reconcile → fresh UUIDs → real SessionStore CAS → reopen → complete revised plan → re-projection/reconcile. It asserts every candidate replaced with no reuse, unchanged order/address/method/descriptor/arguments digests, recomputed binding digests, post-CAS durability retry, CAS mismatch leaving the durable tip untouched, and no `StepClosed`/`TurnClosing`/TurnCommit before the revision. |
| R2-04 | MAJOR | `InvocationFactsSnapshot::validate_against_plan`'s exact branch checked per-binding presence but not key-set equality, so extra records could pass as an exact match. | The snapshot now fails closed unless `invocations.keys()` is a subset of the plan's candidate set, and duplicate candidates in the plan are corruption; combined with the per-binding presence check, `ExactMatch` requires exact key-set equality. Added extra-own, extra-foreign, extra-without-plan, and duplicate-candidate tests. |
| R2-05 | BLOCKER | `RESULT_FINALIZE_STRESS` published terminals in the `Bound` window and asserted `Bound` at the end. | Rewritten to the real durable order per binding: `BindingPlanned → exact reservation → Bound → StepClosing(cursor 0, no closures, no candidate)`, then per binding `i`: `DispatchStarted → StepClosing(cursor i + exact ResultDraft candidate) → CleanAbsent \| Durable \| Uncertain-then-revalidated → typed terminal + evidence → StepClosing(cursor i+1, closure appended, candidate cleared)`, ending at `StepClosed(ContinueModel)` and verified by reopen + `reconcile` producing one legal closure rather than corruption. The uncertain path revalidates through a fresh strict `GitSessionGraphStore::open` (reopen-equivalent). Every store write is preceded by a strict non-blocking probe proving the other repository's write lock is free, so the two repository write locks are never held together. Full-finalize timing now spans the real finalize scope (candidate pin through terminal evidence). |
| R2-06 | MAJOR | Missing regression coverage: no zero-Indeterminate assertion, no post-CAS fault matrix for `reserve_batch`/terminal. | `local_ledger_stress` now asserts every terminal is exactly `Succeeded` and that zero terminals are `Indeterminate`. Added `reserve_batch_published_but_undurable_retries_exactly_and_rejects_a_different_batch`, `terminal_published_but_undurable_retries_exactly_and_rejects_a_different_terminal`, and `dispatch_started_published_but_undurable_rejects_a_foreign_invocation_retry`, each covering published-but-undurable exact retry and mismatch fail-closed. The 1,280 exact IDs, 2,720 unique commits, linear ancestry, query/tail/reopen consistency, and writer completion/fairness assertions are unchanged. |
| R2-07 | MINOR | `environment.json` recorded no distribution, backing device, mount options, scheduler, CPU governor, free space, or load average. | `benchmarks/invocation-store/run-qualification.sh` emits environment schema v2 with `distribution` (`/etc/os-release` ID/VERSION_ID/PRETTY_NAME), `store_device` (source, node, parent kname, model, rotational, scheduler, mount and FS options), `store_free_bytes_at_start`, `store_total_bytes`, `load_average_at_start`, and `cpu_governors`, all degrading to `null` rather than failing the run. Existing bundles keep schema v1 and were not backfilled or rewritten. |

### Verification

- `cargo test -p lattice-session` — 163 passed.
- `cargo test -p lancet-audit` — 114 lib + 1 contract-shape + 2 integration passed; the two qualification harnesses ignored.
- `cargo test --release -p lancet-audit result_finalize_stress -- --ignored` — one functional run passed at its new location.
- `cargo test --release -p lancet-audit local_ledger_stress -- --ignored` — one functional run passed at its new location.
- `cargo tree -p lancet-api -e normal` — zero `lattice-session` occurrences.
- `bash -n benchmarks/invocation-store/run-qualification.sh` and a direct execution of its environment block — passed.

### Retained evidence

Both qualification bundles under `benchmarks/invocation-store/results/` are
unchanged byte-for-byte. No sample, outlier, or summary was deleted or
rewritten. The local-ledger group of
`20260817T214110Z-7c64a4f3ec34/` remains valid historical evidence; its
result-finalize group and the numbers derived from it are withdrawn.

### Changed files

`crates/lancet-audit/Cargo.toml`, `crates/lancet-audit/src/invocation_store.rs`,
`crates/lancet-audit/tests/invocation_qualification.rs` (new),
`crates/lattice-session/src/pending/facts.rs`,
`crates/lattice-session/src/pending/reconcile.rs`,
`crates/lattice-session/src/pending/mod.rs`,
`crates/lattice-session/src/git_graph/pending.rs`,
`crates/lattice-session/src/git_graph/tests/pending_crash.rs`,
`crates/lattice-session/src/git_graph/tests/pending_journal.rs`,
`benchmarks/invocation-store/run-qualification.sh`, `AGENTS.md`,
`crates/lancet-audit/AGENTS.md`, `crates/lattice-session/AGENTS.md`,
`docs/ARCHITECTURE.md`, ADR 0005, both #139-A plan records, `FINDINGS.md`, and
`FIX.md`.

## 2026-08-18 — GitHub #139-A third review pass (P2/P3 follow-ups)

A third independent review confirmed the second-review blockers are closed and
raised documentation-sync and adversarial-robustness follow-ups. Nothing above
is deleted; this section records what changed after it.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R3-01 | P2 | `docs/ARCHITECTURE.md`'s workspace dependency diagram omitted `SESSION --> STORE`, `SESSION --> PROTOCOL`, `AUDIT --> STORE`, and `RUNTIME --> STORE`; the layer table's `lattice-session` row was incomplete and `lancet-runtime` had no row at all. | Added the four missing edges, corrected the `lattice-session` row to `lancet-protocol, lattice-conversation, lattice-store`, and added the missing `lancet-runtime` row. Each row now matches that crate's `[dependencies]`. |
| R3-02 | P2 | `docs/ARCHITECTURE.md`'s `lancet-audit` crate description did not mention the #139-A `InvocationStore`. | Documented the non-production Git `InvocationStore`: `invocations.git`, the append-only ledger ref plus ordered high-water witness, exact ordered `reserve_batch`, atomic typed terminal + evidence, pinned linear-ancestry reads, the audit-domain-only public surface, and the standing prohibition on a normal `lattice-session` dependency. |
| R3-03 | P2 | `FINDINGS.md` L-GH139A-02 still claimed #139-A had proven "write/finalize distributions", and the decision record still pointed at "approved numbers". | Both now state that only the local-ledger group survives, that the result-finalize group and the 60 s budget are withdrawn, and that #139-A has no approved finalize distribution or threshold today. |
| R3-04 | P2 | `execute_candidate_revision` minted UUIDs internally; a CAS-published-but-undurable revision could only be recovered by a caller able to replay the exact same UUIDs, which a real random source cannot do. The old test only passed because it replayed a deterministic sequence. | The executor now performs strict-reopen-and-continue: it re-reads the durable `active-turn` first and, if a candidate revision of exactly `request.plan` is already grafted on the expected tip, repeats that publication's durability barrier and returns it **without generating anything**. Recognition uses one shared predicate, `is_candidate_revision_of`, which is also the admission rule inside `revise_binding_candidates`, so publication and recovery can never disagree. New tests assert recovery with a deliberately different generator (session-level and in the store-backed E2E) and that a genuine CAS mismatch — the journal moved past `BindingPlanned` — still fails closed with the durable tip untouched. |
| R3-05 | P2 | `RESULT_FINALIZE_STRESS` only called `reconcile` at the final `StepClosed`, a stage that does not validate the projected invocation facts. | The harness now reduces over the projected facts mid-loop at `StepClosing` — the stage that *does* validate them — on every binding of the first step (covering all three draft classifications) and on the last binding of every step, asserting the closure resumes at the exact lowest unclosed ordinal without skipping and closes every remaining binding. It also asserts every terminal is a deterministic `Succeeded` with zero `Indeterminate`, and reports `unexpected_indeterminate_count` in the qualification record. |
| R3-06 | P3 | The harness comment, decision record, and FINDINGS wrote the order as `Bound → DispatchStarted → StepClosing`, omitting the mandatory empty `StepClosing(cursor 0)` baseline. | All three now spell out `Bound → StepClosing(cursor 0, no closures, no candidate)` followed by the per-binding loop, and state that pinning a candidate and advancing the cursor are always separate transitions. |
| R3-07 | P3 | Several documents were dated `2026-08-19` while the host date is `2026-08-18`. | Corrected to `2026-08-18` across `AGENTS.md`, `FIX.md`, `FINDINGS.md`, `docs/ARCHITECTURE.md`, ADR 0005, and both plan records. |

### Verification

- `cargo test -p lattice-session` — 164 passed.
- `cargo test -p lancet-audit` — 114 lib + 1 contract-shape + 2 integration passed.
- `cargo test --release -p lancet-audit result_finalize_stress -- --ignored` — one functional run passed; 160 invocations, CleanAbsent 60 / Uncertain 60 / Durable 40, `unexpected_indeterminate_count` 0.
- `cargo test --release -p lancet-audit local_ledger_stress -- --ignored` — one functional run passed.
- `cargo fmt --all --check`, `git diff --check`, targeted Clippy, `cargo xtask lancet-boundary`, `cargo tree -p lancet-api -e normal`, and `env -u LATTICE_BIN cargo xtask ci` — all clean.

The qualification status is unchanged: the corrected harnesses have been run only
as functional checks, and the 20+20 Arch Linux/Btrfs requalification plus a
separate approval commit remain required before any `FinalizeBudget` or
result-finalize threshold is approved again.

## 2026-08-18 — GitHub #139-A requalification and re-approval

Maintainer-authorized rerun of the full qualification with the corrected
harnesses, and re-derivation of the numbers withdrawn earlier today.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| L-GH139A-03 | BLOCKER (withdrawn approval) | The `RESULT_FINALIZE_STRESS` group, the 60 s `FinalizeBudget`, and every result-finalize threshold were withdrawn because the harness that produced them published Invocation terminals while the PendingTurn was still `Bound`. | Ran a fresh 20+20 Arch Linux/Btrfs release qualification with the corrected harness and the schema-v2 runner, retained it immutably as `benchmarks/invocation-store/results/20260818T115511Z-4a51b5d42484/`, and re-derived the budget and every threshold from it in a Requalification section of `docs/plans/2026-08-18-issue139a-qualification-approval.md`. ADR 0005 §12 again states an approved 60 s budget, now sourced from the corrected run. Finding closed. |

### Evidence

- Bundle `20260818T115511Z-4a51b5d42484`, source SHA `4a51b5d42484154c7580efd15941e1e6a60c97cb`, bundle SHA-256 `02176e82219c6028070d45a4de34c2b3035fc83284b5c67c7da38a0cefa0512e`; `git bundle verify` reports a complete history and `git bundle list-heads` resolves that exact HEAD.
- Environment schema v2 on real Btrfs: Arch Linux rolling, kernel `7.1.8-1-cachyos-bore`, `/dev/nvme0n1p2` (`rw,ssd,discard=async,space_cache=v2`), KINGSTON OM8PGP41024N-A0, non-rotational, `mq-deadline`, `performance` governor, 167.4 GB free, load 2.07/1.69/2.55.
- All 20 local-ledger runs: 1,280 invocations, 2,720 linear commits, 340 writes per writer across 8 writers, zero unexpected `Indeterminate`, zero CAS retries.
- All 20 result-finalize runs: 160 invocations through the full durable order, exactly 60 `CleanAbsent` / 60 `Uncertain` / 40 durable-first drafts, mid-loop `StepClosing` reduction resuming at the exact lowest unclosed ordinal, legal `StepClosed(ContinueModel)` closure at reopen, zero unexpected `Indeterminate`, zero CAS retries.
- Measured: full terminal finalize p50/p95/p99/max 41.26 / 49.70 / 66.21 ms / 4.827 s; terminal+evidence publish p99 32.89 ms; uncertain draft revalidation p99/max 0.605 / 0.639 ms; result-finalize wall p95 25.67 s; local-ledger wall p50/p95/max 60.94 / 111.15 / 160.99 s; maximum single repository-lock wait 42.69 s; maximum writer-wall ratio 1.414; maximum consecutive waits 313.
- Approved: 60 s monotonic `FinalizeBudget` (42.69 s worst contended acquisition + 4.827 s complete publication + 1 s capped backoff + a second publication = 53.4 s), deterministic 25 ms → 1 s exponential backoff, and the eleven regression gates in the approval record.

The two earlier bundles remain retained and byte-identical; nothing was deleted
or rewritten. FINDINGS L-GH139A-02's bounded production-read gate stays open and
still blocks #139-B's production wiring.

## 2026-08-18 — GitHub #139-A strict repository lock made FIFO, and requalified

Follow-up to the requalification above. The bundle it approved measured a
barging lock, so its local-ledger contention numbers described starvation
rather than queueing.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R4-01 | MAJOR (tail latency) | `acquire_flock_strict` woke every waiter with one `Condvar::notify_all()`, and whichever thread first won the registry mutex proceeded. A writer that had just released was already running and routinely re-acquired ahead of peers queued far longer. Under 8 concurrent writers this produced 313 consecutive losses for a single writer, a 42.69 s worst single acquisition, and a 1.414 writer-wall ratio, while aggregate throughput looked acceptable. | Blocking strict acquisitions now take a ticket from a per-repository FIFO queue and are granted in arrival order. The registry entry is retained while waiters are queued so the ticket counters survive the gap between release and the next acquisition; a failed re-acquisition advances `serving` so the queue is never stranded; and a non-blocking acquire never joins the queue, reporting `Busy` whenever the lock is held or anyone is waiting. `StoreRepo::write_lock`'s interface is unchanged, cross-process exclusion is still the kernel `flock`, and operation locks keep their shared-nesting semantics. |

### Measured effect

Requalified as bundle
`benchmarks/invocation-store/results/20260818T131229Z-da1d2ac15ce5/` (source SHA
`da1d2ac15ce5`, bundle SHA-256 `8468e352…`), same Arch Linux/Btrfs host,
one-minute load average 4.22 at start against 2.07 for the bundle it improves
on.

| Local-ledger metric | Barging | FIFO | Change |
|---|---:|---:|---:|
| Maximum consecutive waits | 313 | 7 | -97.8% |
| Maximum single repository-lock wait | 42.69 s | 12.47 s | -70.8% |
| Single repository-lock wait p95 | 41.41 s | 8.79 s | -78.8% |
| Maximum writer-wall ratio | 1.414 | 1.032 | -27.0% |
| `reserve_batch` / `DispatchStarted` / terminal max | 22.34 / 42.71 / 27.69 s | 8.38 / 13.05 / 12.28 s | -62.5% / -69.5% / -55.6% |
| Harness wall p50 / p95 | 60.94 / 111.15 s | 64.20 / 105.86 s | +5.4% / -4.8% |

`max_consecutive_wait` is exactly 7 in all 20 runs — `N-1` for 8 writers, the
structural floor for a fair queue — and the writer-wall ratio is 1.000-1.002 in
19 of 20. Per-operation p50 rose to a shared ~133 ms because the hot writer's
barging fast path is gone; that is the honest serialized cost, and throughput is
unchanged by design. The single-writer result-finalize group moved less than 2%
on every metric except terminal publish p99/max, which improved 10.8% / 26.3%,
confirming the queue does not tax the uncontended path.

All 40 runs kept every correctness assertion: 1,280 invocations and 340 writes
per writer per local-ledger run, 160 invocations with 60/60/40 draft
classifications per result-finalize run, zero unexpected `Indeterminate`, zero
CAS retries.

### Approval consequences

The 60 s `FinalizeBudget` is unchanged; only its margin improves, from 1.12x to
2.59x (worst case now 12.47 + 4.889 + 1 + 4.889 = 23.2 s). The three fairness
regression gates were calibrated against barging and are tightened to detect a
reintroduced barging lock: maximum single repository-lock wait 55 s -> 20 s,
maximum consecutive waits 750 -> 16, maximum writer-wall ratio 4.0 -> 1.5. Every
result-finalize gate is unchanged and still passes. Full record: the FIFO
requalification section of
`docs/plans/2026-08-18-issue139a-qualification-approval.md`.

### Verification

- `cargo test -p lattice-store` — 46 lib (including the new `strict_lock_serves_waiters_in_arrival_order` and `strict_nonblocking_does_not_jump_a_queued_waiter`) + 5 integration passed.
- `cargo test -p lattice-session -p lancet-audit -p lancet-runtime` — passed.
- `cargo clippy -p lattice-store --all-targets -- -D warnings` — clean.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed.

### Not done here

Step one of the agreed optimization sequence only. Still open, in order: a
shorter critical section (event construction, canonical JSON, and
state-independent validation outside the lock), a verified derived checkpoint
bound to an exact ledger OID so cold replay does not happen under the lock
(shared with L-GH139A-02), and `ProjectStoreKey` admission control in #139-B
with its queueing charged to the `FinalizeBudget`. Rejected without a new ADR:
removing fsync, batching terminals, merging lifecycle transitions, multiple
ledger refs, and sharding `invocations.git`.

## 2026-08-18 — GitHub #139-A ledger events prepared outside the repository write lock

Step two of the agreed optimization sequence, after the FIFO lock above.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R5-01 | MAJOR (tail latency) | Event construction, canonical JSON serialization, and the commit message all ran inside the repository write lock, which serializes every writer of one project's ledger. Terminal evidence is the largest payload the ledger carries, so every peer waited through its serialization. | Admission split into `validate_event_shape` (everything decidable without the ledger: empty batches, cross-session batches, duplicate candidates within one batch, addresses not belonging to the plan key, non-ascending part ordinals, malformed terminal evidence or material) and `validate_event_against_snapshot` (an already-reserved plan key, a candidate already owned, the legal-successor matrix). `validate_ledger_event` remains both halves for replay. `PreparedEvent` runs the shape half plus canonical serialization before the lock. Object publication, ref CAS, witness advance, and the durability barrier deliberately stay inside. |
| R5-02 | MINOR (provenance) | `environment.json` sampled the host once at bundle start, so a distribution outlier could not be attributed and the "annotate outliers in place" rule was not executable. | `run-conditions.jsonl` records load average, free store bytes, and running-process count immediately before and after every run; environment schema v3 names it. Earlier bundles keep their schema and were not backfilled. |
| R5-03 | MINOR (gate calibration) | The `uncertain_draft_revalidation` maximum gate was set at 2 ms from one bundle's 0.639 ms baseline, but that statistic is a single sample of a sub-millisecond filesystem open and read. | Raised to 10 ms with the p99 gate unchanged at 1 ms, on the evidence of four retained bundles where p99 stays within 0.08 ms while the maximum ranges 0.63-3.94 ms. Recorded that the maximum-latency gates and the `FinalizeBudget` are one system: `lock_wait_gate + 2 x full_finalize_gate + backoff_cap <= FinalizeBudget`, so `full_terminal_finalize` max stays at 10 s despite only 1.18x headroom. |

### Measured effect

Requalified as `benchmarks/invocation-store/results/20260818T153043Z-3ee216ba4f5f/`
(source SHA `3ee216ba`, bundle SHA-256 `33bf18b6`, environment schema v3).

| Local-ledger metric | FIFO only | FIFO + prepared | Change |
|---|---:|---:|---:|
| `reserve_batch` / `DispatchStarted` / terminal publish p95 | 177.49 / 181.34 / 182.00 ms | 154.76 / 158.48 / 159.18 ms | -12.8% / -12.6% / -12.5% |
| `reserve_batch` max | 8.376 s | 5.953 s | -28.9% |
| Maximum single repository-lock wait | 12.47 s | 10.94 s | -12.2% |
| Maximum writer-wall ratio | 1.032 | 1.002 | -2.9% |
| `DispatchStarted` / terminal publish p50 | 133.16 / 133.14 ms | 132.51 / 132.48 ms | -0.5% |

p50 is flat and p95 improves about 12.5% on all three write paths, which is the
expected shape: each writer waits through seven peers' critical sections per
turn, so a fixed cost removed from one section is repaid sevenfold in the
contended tail. The retained invalid bundle measured the same effect
independently (-11.4% / -11.2%). The single-writer result-finalize group is the
control and is unchanged (wall p95 +2.1%). `max_consecutive_wait` is 7 in all 20
runs again, and all 40 runs kept every correctness assertion with zero
unexpected `Indeterminate` and zero CAS retries.

### Retained invalid bundle

`20260818T144803Z-ca768c80d80e` is retained unmodified and approves nothing: two
of its twenty result-finalize runs took 65.6 s and 66.9 s against 17-26 s for
the other eighteen, breaching two gates on two samples. The change under test
only removes work from the critical section and that harness is single-writer,
so it cannot have caused a 2.6x wall regression there — but the environment was
not provably valid, so the whole qualification was rerun with the old bundle
retained rather than an outlier dropped. That bundle's numbers must not be cited.

### Verification

- `cargo test -p lancet-audit` — 116 lib (2 new) + 1 contract-shape + 2 integration passed.
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed.

Two existing tests were strengthened rather than relaxed, because shape
rejection is now earlier: `duplicate_id_in_another_plan_fails` rebases its
binding onto the second plan key so the duplicate ID is the only remaining
fault, and `same_ids_in_different_order_is_not_idempotent` swaps IDs between
shape-legal ascending addresses so it still reaches the order-sensitive
durable-plan comparison. `a_descending_part_order_is_rejected_as_shape` covers
the earlier rejection, and
`shape_rejection_does_not_wait_for_the_repository_write_lock` proves the shape
half runs outside the lock by rejecting while another guard holds it.

### Not done here

Still open, in order: a verified derived checkpoint bound to an exact ledger OID
so a cold writer does not replay project history under the lock (also
L-GH139A-02's bounded-read gate), and `ProjectStoreKey` admission control in
#139-B with its queueing charged to the `FinalizeBudget`. The
rejected-without-an-ADR list is unchanged.

## 2026-08-18 — GitHub #139-A verified derived on-disk checkpoint cache (L-GH139A-02)

Step three of the agreed optimization sequence, closing FINDINGS L-GH139A-02.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R6-01 | MAJOR (cold replay scaling) | The append-only event ledger bounds per-transition write amplification, but cold pinned reads and the first writer in a process replayed the entire project history from genesis. Under large project histories, cold read/write latency grew with all historical project invocations. | Added `DerivedCheckpoint` (`derived-checkpoint.json`) in the store directory, bound to an exact authoritative ledger commit OID and verified with `validate_snapshot_shape` and `is_first_parent_ancestor`. It is a derived, rebuildable projection, never a second authority: if missing, corrupted, or mismatched with authoritative tip ancestry, it is discarded and reconstructed from genesis. On cold read (`open -> snapshot/pinned_view`) and cold write (`open -> write_lock -> write_state`), the verified checkpoint is loaded in O(1) time without replaying genesis; incremental writes only apply delta events since the checkpoint. Unit tests cover cold reopen, deletion rebuild, corruption discard/rebuild, tip mismatch discard, lagging ancestor delta replay, and cold first-writer under write lock. |

### Verification

- `cargo test -p lancet-audit` — 121 lib (5 new tests) + 1 contract-shape + 2 integration passed.
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed.
- `L-GH139A-02` moved to CLOSED in `FINDINGS.md`.

## 2026-08-18 — GitHub #139-A authenticated checkpoint & cold-reopen qualification

Resolves re-review findings P1-1, P1-2, P1-3, P1-4, and P3, and requalifies the complete suite.

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R7-01 | BLOCKER (cryptographic authentication) | `derived-checkpoint.json` was an unauthenticated external file: a shape-valid forged empty snapshot naming the current tip was accepted without ledger verification, causing existing invocations to vanish and creating a second unauthenticated authority (P1-1). | Every commit tree now embeds a 32-byte `state_digest` blob (SHA-256 of canonical snapshot JSON). `load_derived_checkpoint` cryptographically verifies the snapshot against the commit tree in O(1); any tampered, forged, or mismatched snapshot is rejected and rebuilt from genesis. Added `forged_empty_checkpoint_for_populated_ledger_is_rejected` proving a forged empty snapshot is rejected and the true state reconstructed. |
| R7-02 | BLOCKER (write amplification) | Every reserve/dispatch/terminal serialized and wrote the full `LedgerSnapshot` inside the write lock, causing $O(\text{history}^2)$ write amplification (P1-2). | Transitions under lock write only `event.json` and `state_digest` (~250 bytes) into git objects. `derived-checkpoint.json` is written periodically every `CHECKPOINT_INTERVAL` (32) commits, eliminating write amplification under the lock. |
| R7-03 | BLOCKER (cold writer fix) | `write_state()` in a fresh handle called `validated_snapshot_at(tip, tip)` from genesis under the write lock, bypassing the checkpoint cache (P1-3). | `write_state()` now uses `resolve_snapshot_for_write`, loading verified checkpoints and replaying only bounded delta events ($\le 32$) without replaying genesis under the lock. |
| R7-04 | BLOCKER (qualification evidence) | Previous qualification bundle did not cover the checkpoint implementation and lacked cold-read / projection benchmark distributions (P1-4). | Added `cold_reopen_benchmark` (20 runs over 2,561-commit ledger) into `run-qualification.sh`. Requalified complete 20×LOCAL + 20×RESULT + 20×COLD suite in bundle `20260818T184822Z-e5c23481be0d`. |
| R7-05 | MINOR (provenance) | `run_group` in `run-qualification.sh` skipped `sample_conditions "after"` on failed runs (P3). | Guaranteed `sample_conditions "after"` executes unconditionally on both success and failure. |

### Measured effect & Requalification

Requalified as `benchmarks/invocation-store/results/20260818T184822Z-e5c23481be0d/`
(source SHA `e5c23481`, bundle SHA-256 `9b3fbe4b`, environment schema v3 with 120 run-condition samples).

- **Cold read with checkpoint (2,561 commits)**: p50 **11.34 ms**, p95 **13.59 ms**, max **16.51 ms** vs genesis replay p50 **75.45 ms** (**6.7× speedup**).
- **Repeated `project_facts` projection**: p50 **5 µs**, p95 **13 µs**.
- **LOCAL**: wall p95 130.08 s ($\le 170$ s), max lock wait 8.53 s ($\le 20$ s, −22%), writer ratio 1.091 ($\le 1.5$), `max_consecutive_wait` 7 in 20/20 runs.
- **RESULT**: wall p95 26.68 s ($\le 40$ s), full finalize p99 63.03 ms / max 4.690 s ($\le 10$ s, −44.8%), terminal publish p99 32.10 ms, revalidation p99 0.55 ms / max 0.66 ms.
- **FinalizeBudget**: 60 s ceiling provides **3.17× headroom** over worst-case chain (8.53 + 4.690 + 1.0 + 4.690 = 18.91 s).
- All 60 runs passed every correctness assertion with zero unexpected `Indeterminate` and zero CAS retries.


## 2026-08-18 — GitHub #139-A P1-2 actually closed: O(event) critical section

A further review round established that the previous entry's P1-2 claim did not
match the shipped code, and that its `remove in-lock clone` commit message
described a change that had been reverted during that same round. This entry
records the real fix and supersedes the P1-2 and complexity claims above.

### Claims overturned by this round

| Superseded claim | Where it was recorded | Why it was wrong |
|---|---|---|
| Transitions "write only ~250 bytes" and O(history²) amplification is "eliminated". | R7-02 row above, approval record, `FINDINGS.md`, ADR 0005. | Every transition still cloned the full `LedgerSnapshot`, canonicalized it, and SHA-256'd it inside the repository write lock, so the critical section was O(state). A *fixed* 32-commit checkpoint interval also leaves cumulative checkpoint bytes at `32 + 64 + … + N` = O(N²). |
| `fix(audit): … remove in-lock clone` (commit `cdf21ef2`) removed the clone. | That commit's message. | The in-place edit was reverted inside that round after it broke a post-CAS durability test; only the schema bump, the regression test, and doc sync actually landed. The message was not corrected. |
| Checkpoint verification and cold start are "O(1)". | ADR 0005, approval record, `FINDINGS.md`, R6-01/R7-01 rows above. | Verification must parse, canonicalize, and hash the full snapshot. The correct statement is **O(state bytes), decoupled from history depth**. |

### Fixes applied

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R8-01 | BLOCKER | Every transition cloned, canonicalized, and hashed the whole snapshot under the repository write lock. | `state_digest` is now written only on *anchor* commits. An ordinary transition writes one event blob and nothing else. The cached snapshot advances **in place** via the new `InvocationStore::append`, so no clone happens in the lock. In-place mutation means a failed publication leaves the cache ahead of the durable tip, so any apply or publish failure drops the cache and the next call rebuilds from the last anchor plus a bounded delta — which is why the earlier in-place attempt broke `dispatch_started_published_but_undurable_rejects_a_foreign_invocation_retry` and why this one does not. |
| R8-02 | BLOCKER | A fixed 32-commit checkpoint interval keeps cumulative checkpoint bytes at O(N²). | The interval now scales with state size: `checkpoint_interval(len) = max(CHECKPOINT_MIN_INTERVAL, len / CHECKPOINT_STATE_DIVISOR)`. Writing O(state) bytes every O(state) commits amortizes to O(1) bytes per commit, so cumulative checkpoint volume over N transitions is O(N). |
| R8-03 | MAJOR | No test proved either property, so the previous false claim survived review. | Added `ordinary_transitions_write_no_snapshot_sized_object_and_anchors_thin_out`: over 600 invocations (1,201 commits) it walks every commit and asserts (a) each non-anchor commit tree contains exactly `event.json` and nothing else, and (b) anchors are strictly sparser than a fixed-interval schedule would place them, that anchor gaps never shrink, and that the final gap exceeds the floor — the structural signature of state-scaled anchoring. It then asserts a cold read still reconstructs the exact state from those sparse anchors. |
| R8-04 | P2 | Documentation claimed O(1) checkpoint verification and cold start. | ADR 0005, the approval record, and `FINDINGS.md` now state **O(state bytes), decoupled from history depth** for verification/cold start and **O(event bytes)** for the per-transition critical section. |

### Consequential test changes

Three checkpoint tests asserted `checkpoint.ledger_tip == tip`, which only held
while *every* commit carried a digest. They now assert the real contract through
a shared `assert_authenticated_checkpoint` helper: the checkpoint names a commit
reachable from the tip, that commit's tree carries a `state_digest`, and that
digest equals the digest of the snapshot the checkpoint stores. That is strictly
stronger than the old equality, which never checked authentication at all. The
cold-reopen test was widened to 15 invocations so the ledger actually crosses
`CHECKPOINT_MIN_INTERVAL` and exercises a real periodic anchor rather than only
genesis.

### Measured effect

Cold reopen over the same 2,561-commit ledger: with checkpoint p50 **15.5 ms**,
without checkpoint (genesis replay) p50 **105 ms** — a **6.8×** speedup.
Repeated `project_facts` projection p50 **5 µs**. Cold read is slightly slower
than the previous bundle's 11.34 ms because an anchor now lags the tip by up to
one interval and the reader replays that bounded delta; that is the deliberate
price of removing O(state) work from every writer's critical section.

### Verification

- `cargo test -p lancet-audit` — 123 lib (1 new) + 1 contract-shape + 2 integration passed.
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed.

### Requalification

Requalified in bundle `20260819T080953Z-403836c2bf59` (source SHA `403836c2`,
bundle SHA-256 `31e02bad`, environment schema v3, 120 run-condition samples).
All 60 runs correct with zero unexpected `Indeterminate` and zero CAS retries;
`max_consecutive_wait` 7 in 20/20 local runs; all 12 regression gates green.

Contended local-ledger write paths improved 38-44% at p50 and p95 with
`reserve_batch` p99 down 90.3% (2396.36 ms to 231.38 ms), harness wall p50 down
31.9%, and the writer-wall ratio down to 1.003 — the measurable signature of
snapshot-sized work leaving the critical section. Cold read is 16.02 ms p50
against 112.92 ms for genesis replay (7.05x). `FinalizeBudget` stays at 60 s
with 3.05x headroom. `L-GH139A-02` is closed on this evidence.


## 2026-08-19 — GitHub #139-A anchor cadence must survive a restart

The preceding entry closed `L-GH139A-02` on a bounded critical section that was
real, but the anchor cadence it introduced was counted per **handle lifetime**.
A writer that reopens the store before each write — the InvocationStore's
central recovery scenario — therefore never reached the interval and never
anchored, so cold reads degraded back to full genesis replay. This entry
records that regression and its fix; the preceding entry's latency measurements
remain valid for the long-lived-handle case.

### Claims overturned by this round

| Superseded claim | Where it was recorded | Why it was wrong |
|---|---|---|
| `L-GH139A-02` is closed. | `FINDINGS.md`, preceding FIX entry, approval record. | Anchoring never advanced under restart, which is the scenario the finding exists for. Reproduced: 80 writes each through a fresh `InvocationStore::open` produced 81 commits, **1** anchor (genesis only), and a checkpoint carrying **0** plans. |
| The 60-run bundle `20260819T080953Z-403836c2bf59` qualifies the checkpoint design. | Approval record's Bounded Critical-Section section. | Every group in it drove writes through long-lived handles, so the restart path was never exercised. Its contended-write and cold-read latencies stay valid for that shape; it does not evidence restart anchoring. |

### Fixes applied

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R9-01 | BLOCKER | `commits_since_checkpoint` was set to `0` unconditionally whenever a handle resolved its state, so cadence restarted with every process. | Renamed to `commits_since_anchor` and derived from the ledger: `advance_anchor_distance(base, suffix)` walks a commit suffix adding one per ordinary commit and resetting at every anchor. `resolve_snapshot_for_write` now returns `ResolvedWriteState { tip, snapshot, commits_since_anchor }`; the checkpoint-hit path starts at 0 (the checkpoint names an anchor) and carries the distance across the replayed suffix; the rebuild path returns the distance measured during replay. `write_state` and `resolve_snapshot_at_tip` consume that value instead of writing a literal `0`, and `write_state`'s peer-catch-up branch advances the distance across the peer's commits so another writer's anchor counts for this handle too. |
| R9-02 | MAJOR | `rebuild_from_genesis` selected an anchor on the mere presence of a 32-byte `state_digest`. A corrupt but right-length digest would be written into the checkpoint and then rejected by every later load, so every cold read would rebuild from genesis forever. | An anchor is now selected only when its recorded digest equals the digest of the snapshot just replayed at that commit. |
| R9-03 | MAJOR | No test covered the restart path, which is why the regression shipped. | Added `anchor_cadence_survives_a_fresh_open_before_every_write` (80 writes, each through a fresh open; asserts at least two anchors, a checkpoint carrying real state, and a tip within one interval of the newest anchor) and `a_corrupt_anchor_digest_is_not_selected_as_the_rebuild_anchor` (forges a right-length digest onto a real anchor, rebuilds descendants over it, and asserts the rebuild falls back to a verifying anchor whose checkpoint reloads). Both were confirmed non-vacuous by reverting each fix in turn and observing the matching failure. |
| R9-04 | MAJOR | The qualification harness populated its ledger through one long-lived handle, so the 60-run suite could not have caught R9-01. | `cold_reopen_benchmark` now performs **one `InvocationStore::open` per write** and asserts the published checkpoint carries real state. It records `writer_handle_lifetime` and `checkpoint_plans_after_restart_writes` in the qualification record, so restart anchoring is evidenced by the bundle rather than asserted in prose. |

### Verification

- `cargo test -p lancet-audit` — 125 lib (2 new) + 1 contract-shape + 2 integration passed.
- Restart-shaped `cold_reopen_benchmark`: 1,280 invocations written through 2,560 fresh handles leave a checkpoint carrying **1,224 plans** (previously 0).
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed.

| R9-05 | BLOCKER | R9-02's anchor verification recomputed the O(state) snapshot digest at **every** commit, because `==` evaluates both operands. That reintroduced the O(N^2) replay this whole line of work exists to remove, and put it under a cold writer's repository write lock. Caught by the requalification run, not by unit tests: cold rebuild went from 113 ms to **11.66 s** over 2,561 commits. | The digest is now recomputed only where a commit actually records one, via a `verify` closure that short-circuits on absence. Anchors are sparse, so verification is O(anchors x state) overall. Cold rebuild returns to **170 ms** and the cold-reopen harness wall drops from 267 s to 29.3 s per run. |

### Verification (second pass)

- Cold rebuild without checkpoint: 170.15 ms p50, versus 11.66 s before R9-05 and 113 ms before R9-02 existed.
- Cold read with checkpoint: 16.78 ms p50, so the speedup over a full rebuild is 10.1x.
- `checkpoint_plans_after_restart_writes` = 1,224 after 2,560 fresh-handle writes.

`L-GH139A-02` is reopened and stays open until the full suite is requalified
against the restart-shaped harness.


## 2026-08-19 — GitHub #139-A restart requalification: two gates FAIL, bundle retained, approves nothing

Bundle `benchmarks/invocation-store/results/20260819T125758Z-0c4d5dc04dd3/`
(source SHA `0c4d5dc0`, bundle SHA-256 `f1096e11`, environment schema v3, 120
run-condition samples) is retained unmodified and **approves nothing**. Two of
the twelve regression gates fail:

| Gate | Measured | Limit | Status |
|---|---:|---:|:---:|
| Full terminal finalize p99 | 217.84 ms | 100 ms | **FAIL** |
| Terminal/evidence publish p99 | 68.01 ms | 50 ms | **FAIL** |

The other ten pass, including local-ledger wall p95 (103.01 s), maximum single
repository-lock wait (8.44 s), writer-wall ratio (1.002), maximum consecutive
waits (7 in 20/20 runs), full terminal finalize max (4.82 s), and both
uncertain-revalidation gates. All 60 runs kept every correctness assertion with
zero unexpected `Indeterminate` and zero CAS retries, and the restart evidence
is present: `writer_handle_lifetime = one_open_per_write` with
`checkpoint_plans_after_restart_writes = 1224` in 20/20 cold runs.

### What the failure is, and what it is not

It is **not** a regression from this branch's changes. The only production delta
between the preceding bundle's source (`9e21a5bf`) and this one (`0c4d5dc0`) is
14 lines inside `rebuild_from_genesis` that make anchor-digest verification
short-circuit. That code runs only on a cold rebuild and only removes work; it
cannot touch terminal finalize latency. Its intended effect is visible and
large: cold rebuild fell from 11.66 s to 180.61 ms in the same comparison.

The failure is an artifact of how the gate is computed. It pools all 20 runs'
samples and takes p99 across the pooled 3,200, so only 32 samples sit above the
threshold and a handful of filesystem stalls can move it by an order of
magnitude. Every bundle in this series contains such stalls at essentially the
same rate, including both bundles that were approved:

| Bundle | Pooled p99 | Median of per-run p99 | Runs whose own p99 exceeds 100 ms |
|---|---:|---:|---:|
| `20260818T184822Z` (approved) | 63.13 ms | 60.23 ms | 3/20 |
| `20260819T080953Z` (approved) | 63.46 ms | 61.93 ms | 3/20 |
| `20260819T102309Z` | 68.43 ms | 65.67 ms | 4/20 |
| `20260819T125758Z` (this one) | **458.92 ms** | **58.70 ms** | 3/20 |

This bundle has the **lowest** median-of-per-run p99 of all four and the **same**
stall rate as the two approved ones. By the robust statistic the implementation
is marginally the best measured so far; by the pooled statistic it is seven
times the worst. The two earlier passes were therefore luck about where stalls
landed inside their runs, not evidence of a better implementation. Recorded
load averages during the four stalled runs (2.69-2.88) were not elevated
relative to the clean runs, so `run-conditions.jsonl` does not license calling
the environment invalid.

### What is deliberately not done here

The gate is **not** widened, and no sample is dropped, reordered, or rerun away.
Relaxing a threshold so a failing run turns green is the exact antipattern this
record already documents once, and the maximum-latency gates are additionally
coupled to the `FinalizeBudget` by
`lock_wait_gate + 2 x full_finalize_gate + backoff_cap <= FinalizeBudget`.

The gate appears mis-specified rather than violated, and the proposed
replacement is a robust form — for example gating the median of per-run p99, or
allowing at most K of 20 runs to exceed the threshold — but changing a gate
definition so a failing run passes requires the same maintainer authorization as
changing the budget. It is proposed, not applied.

### Status

`L-GH139A-02` remains **open**. #139-A is not approvable on this bundle, and
#139-B must not be unblocked by it.

## 2026-08-19 — GitHub #139-A restart anchoring qualified; latency gate statistic replaced

| ID | Severity | Problem | Fix |
|---|---|---|---|
| R10-01 | BLOCKER | Anchor cadence was counted per handle lifetime, so a writer that reopened the store before each write never anchored: 80 writes through 80 fresh opens produced 81 commits, one anchor (genesis), and a checkpoint carrying zero plans. Every later cold read then degraded to a full genesis replay. Restart is the InvocationStore's central recovery scenario. | Cadence is derived from the ledger. `advance_anchor_distance` walks a commit suffix, adding one per ordinary commit and resetting at each anchor; `resolve_snapshot_for_write` returns `ResolvedWriteState { tip, snapshot, commits_since_anchor }`; `write_state` and `resolve_snapshot_at_tip` consume it instead of writing a literal zero, and the peer-catch-up branch advances it across another writer's commits so a peer's anchor counts for this handle. |
| R10-02 | MAJOR | `rebuild_from_genesis` selected an anchor on the mere presence of a 32-byte `state_digest`. A corrupt but right-length digest would be written into the checkpoint and rejected by every later load, so every cold read would rebuild from genesis forever. | An anchor is accepted only when its recorded digest equals the digest of the snapshot just replayed at that commit. |
| R10-03 | MAJOR | The qualification harness populated its ledger through one long-lived handle, which is why R10-01 escaped 60 runs. | `cold_reopen_benchmark` performs one `InvocationStore::open` per write and records `writer_handle_lifetime` and `checkpoint_plans_after_restart_writes`, so restart anchoring is evidenced by the bundle rather than asserted in prose. |
| R10-04 | BLOCKER | R10-02's verification recomputed the O(state) snapshot digest at every commit, because `==` evaluates both operands. That reintroduced the O(N^2) replay under a cold writer's write lock: cold rebuild went from 113 ms to 11.66 s. Caught by requalification, not by unit tests. | The digest is recomputed only where a commit records one, via a `verify` closure that short-circuits on absence. Anchors are sparse, so verification is O(anchors x state). Cold rebuild returned to 180 ms and the cold harness wall fell from 267 s to 29.3 s per run. |
| R10-05 | MAJOR (gate) | Both finalize latency gates used a p99 pooled over every run's samples. With 3,200 samples only 32 sit above p99, so one run hitting a sustained host transaction-commit stall contributed 19-22 slow samples and decided the gate by itself. | Maintainer-authorized change to the median of per-run p99, published as `summary.json`'s `operations_per_run_p99`. Thresholds unchanged; maximum gates unchanged and still bound by the `FinalizeBudget` coupling rule. |

### Root cause of the stalls (FINDINGS L-GH139A-04), established by measurement

- They are Btrfs transaction-commit time, not our code. The result-finalize harness drives **exactly 863 transaction commits in every run measured** (22 runs across three campaigns) while `total_commit_ms` for that identical work varies more than fivefold: 2,650 ms average on clean runs against 9,936 ms on stalled ones.
- The commits are ours, not ambient: the idle filesystem commits at 0.1/s against roughly 50/s during a run.
- The commit count is not excess work. The local-ledger harness performs about 4,320 durable writes with eight concurrent writers and needs 631 commits (0.15 per write) because concurrent writers share a commit; the single-serial-writer result-finalize harness gets 1.00 per write. One commit per durable publish is what a serial fsync-ing writer costs on this filesystem.
- The barrier shape is not the trigger: a faithful standalone replica of `fsync_publication` (object write, fanout fsync, `objects/` fsync, lock-file rename, ref fsync, every ancestor directory fsync, git-dir fsync) forces **zero** transaction commits in 200 iterations on this kernel.
- Sustained degradation is cross-run backlog: `sync` plus a 15 s settle between runs removes the 19-22-consecutive-stall runs and cuts the worst run's commit time from 16,550 ms to 5,844 ms. It is still not a fix, because the pooled p99 over twelve settled runs remains 950.85 ms while the median of per-run p99 is 66.09 ms.

Retroactive evaluation across all ten retained bundles, raw samples unmodified:
pooled p99 spans 34.29-952.78 ms (a 28-fold spread that tracks stall luck),
while the median of per-run p99 spans 28.39-75.99 ms and passes in every bundle
including both that the pooled gate failed.

### Requalification

Authoritative bundle `benchmarks/invocation-store/results/20260819T150644Z-a9eca929371e/`
(source SHA `a9eca929`, bundle SHA-256 `9ba078c0`, environment schema v3, 120
run-condition samples). All twelve gates pass. All 60 runs held every
correctness assertion with zero unexpected `Indeterminate` and zero CAS
retries, `max_consecutive_wait` was 7 in 20/20 local runs, and
`checkpoint_plans_after_restart_writes` was 1,224 in 20/20 cold runs. Cold read
17.32 ms p50 against 180.65 ms for a full rebuild (10.43x); repeated
`project_facts` 5.48 us p50. `FinalizeBudget` stays 60 s with a worst-case
chain of 20.80 s and 2.88x headroom.

Both regression fixes were confirmed non-vacuous by reverting each in turn and
observing the matching failure. `L-GH139A-02` and `L-GH139A-04` are closed.

### Verification

- `cargo test -p lancet-audit` — 124 lib tests (2 new) passed.
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed.

## 2026-08-20 — GitHub #139-A re-review repairs (qualification reopened)

### Confirmed findings and applied fixes

- **L-GH139A-06 (P1, confirmed)** — A cached reader that caught up with peer
  commits through `snapshot()` dropped the ledger-derived anchor distance before
  its next write. A red public-behavior test reproduced the missed checkpoint;
  the suffix distance is now carried through `resolve_snapshot_at_tip` and a
  reader-catch-up regression test passes.
- **L-GH139A-07 (P2, confirmed)** — Suffix replay treated any right-length
  `state_digest` as an anchor without checking the replayed snapshot. A red
  retained-checkpoint/forged-suffix test reproduced the cadence reset; replay
  now distinguishes absent and malformed digests and verifies sparse anchors,
  while ordinary commits avoid O(state) hashing. A mismatch falls back to the
  older authenticated checkpoint.
- **L-GH139A-05 (P3, confirmed)** — Median-of-run-p99 alone could hide up to
  nine affected runs. `run-qualification.sh` now emits summary schema v2
  `run_incidence` records with explicit K-of-20 gates. K=3 for both finalize
  thresholds is derived from the maximum exceedance in earlier approved
  bundles, not selected from the current bundle.

### Qualification disposition

Independent replay of retained bundle
`20260819T150644Z-a9eca929371e` counts 8/20 full-finalize runs over 100 ms and
7/20 terminal/evidence-publish runs over 50 ms, so it fails K=3/20. Its prior
authoritative/closed claim is withdrawn; raw samples remain byte-identical and
the bundle remains retained. A maintainer-authorized complete 20+20+20
qualification is required after these repairs.

### Verification

- `cargo test -p lancet-audit reader_catch_up_preserves_anchor_cadence_for_a_later_write` — PASS.
- `cargo test -p lancet-audit corrupt_suffix_anchor_does_not_reset_reader_catch_up_distance` — PASS.
- `cargo test -p lancet-audit invocation_store::tests` — 44 passed.
- `bash -n benchmarks/invocation-store/run-qualification.sh` — PASS.

## 2026-08-20 — GitHub #139-A post-re-review repairs (qualification still reopened)

Correction to the preceding 2026-08-20 re-review entry. That entry remains
historical; this one records the second-pass findings. No retained bundle is
current or authoritative. #139-A is pending requalification; #139-B remains
blocked. Do not start 20+20+20 until a maintainer authorizes it.

### Confirmed findings and applied fixes

- **L-GH139A-05 K baseline (P1, confirmed)** — K=3/20 for both finalize
  metrics used only `4a51b5d4`, `e5c23481`, and `403836c2`. The still-effective
  approved set also includes `da1d2ac15ce5` (4/20 full-finalize, 2/20
  terminal-publish) and `3ee216ba4f5f` (4/20, 0/20). K is now the maximum
  exceedance across that complete set: **4/20** full-finalize and **3/20**
  terminal-publish. It is not chosen to pass `a9eca929` (8/20 and 7/20).
  Bundles that approve nothing or whose authority was withdrawn remain on disk
  and are not a K baseline.
- **L-GH139A-05 enforcement (P1, confirmed)** — `run-qualification.sh` recorded
  PASS/FAIL but exited zero on a failed incidence gate and skipped a missing
  metric. `summarize_qualification.py` is now the testable seam: `qualify`
  exits 1 on a gate fail and 2 on malformed/incomplete input; `report`
  inspects a retained bundle without failing the process on a well-formed
  FAIL. Both gated metrics, exactly 20 runs, and harness sample counts are
  required. Filtering a missing `record[key]` cannot drop `run_count` below 20
  and still pass.
- **L-GH139A-08 (P1, confirmed)** — `write_state` applied a failing suffix in
  place and left `cached.snapshot` advanced. A later `reserve_batch` on the
  same handle replayed the suffix again and failed as `DifferentPlan`. Replay
  failure now restores the original snapshot and distance; both attempts fail
  closed with the same malformed-digest `Corruption`.
- **L-GH139A-09 (P1, confirmed)** — `rebuild_from_genesis` used
  `try_read_state_digest_at`, which treated a wrong-length digest as absent.
  Rebuild now uses `recorded_state_digest_at`: absent remains an ordinary
  commit, malformed/unreadable fails closed, and ordinary commits still skip
  O(state) hashing.

### Status documentation

`FINDINGS.md` lists L-GH139A-02/04/05/06/07/08/09 as HIGH and pending
requalification. L-GH139A-02 is OPEN, not CLOSED. Historical closes are marked
superseded. ADR 0005 §12, root and crate `AGENTS.md`, and the qualification
approval record state that no bundle is current and that #139-B remains
blocked. The preceding FIX.md entry's K=3/20 claim is superseded by this
correction. `report` is read-only: it does not rewrite a retained bundle's
`summary.json` / `summary.md`.

### Verification

- `cargo fmt --all -- --check` — clean.
- `git diff --check` — clean.
- `bash -n benchmarks/invocation-store/run-qualification.sh` — PASS.
- `python3 -m unittest test_summarize_qualification.py` — 16 passed.
- `cargo test -p lancet-audit` — 130 lib + 1 contract + 2 invocation_qualification
  (+3 ignored harnesses) + 1 qualification_summary (16 Python tests) passed.
  New tests include `writer_catch_up_failure_does_not_pollute_the_cached_snapshot`,
  `rebuild_fails_closed_on_a_malformed_genesis_digest`,
  `rebuild_fails_closed_on_a_malformed_periodic_anchor_digest`, and
  `qualification_summarizer_enforces_incidence_and_input_shape`.
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `cargo xtask lancet-boundary` — PASS.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed (home-disk
  `CARGO_TARGET_DIR`; an earlier sandbox `/tmp` run hit disk quota and is not
  the recorded result).

**Changed files:** `crates/lancet-audit/src/invocation_store.rs`,
`crates/lancet-audit/tests/qualification_summary.rs`,
`crates/lancet-audit/AGENTS.md`,
`benchmarks/invocation-store/run-qualification.sh`,
`benchmarks/invocation-store/summarize_qualification.py`,
`benchmarks/invocation-store/test_summarize_qualification.py`,
`docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`,
`docs/plans/2026-08-18-issue139a-qualification-approval.md`,
`AGENTS.md`, `FINDINGS.md`, `FIX.md`.

## 2026-08-20 — GitHub #139-A post-repair 20+20+20 retained as a valid 5/20 FAIL

The repaired source `0f3d19a6` was qualified on Arch/Btrfs. That run is a
**valid failure**, not an invalid-environment rerun and not a reason to raise
K.

- Bundle `benchmarks/invocation-store/results/20260820T061235Z-0f3d19a6bdb3/`
  (source-history SHA-256 `20403fe9…`, 20+20+20, 120 conditions).
- `qualify` exit 1: full-finalize **5/20** against K=4/20; terminal-publish
  3/20 PASS against K=3/20. `report` exit 0. Median-of-run full p99 59.08 ms.
- Exceeding runs: full 1, 11, 12, 16, 18; terminal 1, 8, 11.
- Full-only 12/16/18 are two-sample spikes: on the second sample, InvocationStore
  terminal/draft have recovered while remainder (Session `StepClosing` pin)
  has not. Run 11 is 21 consecutive samples on both metrics.
- K stays 4/20. #139-B stays blocked. L-GH139A-10 records the failure.
- The result-finalize harness now emits `session_journal_pin_ns` for the next
  qualification. Do not rerun 20+20+20 until an implementation change lands.

### Verification

- Replayed `result-finalize.jsonl` remainder = full − terminal on runs 12/16/18.
- Maintainer verified bundle completeness, `git bundle verify`, and source `0f3d19a6`.
- Bundle JSONL/conditions/summary bytes were not rewritten.

## 2026-08-20 — GitHub #139-A Session journal pin diagnosis and CAS barrier contract

Directed diagnosis, not a new 20+20+20. Five Btrfs `RESULT_FINALIZE_STRESS`
rounds at `7f57b449` with `session_journal_pin_ns` confirmed that full-only
slow samples match the pin at the same index and the same order of magnitude
(3/3; round 3 i=105 pin 2269 ms vs terminal 22.5 ms). The remainder lives in
`advance_pending_turn`. Healthy pin p50 stays ~19 ms; seconds-scale samples
are fsync wait, not snapshot serialize or lock wait.

`cas_ref` already fsyncs objects, CAS-publishes, then fsyncs the ref. Journal
CAS callers (`begin_pending_turn`, `advance_pending_turn`, binding-candidate
revision) had a follow-up `fsync_pending` — a third `git_dir` fsync with no
pending objects. That extra call is removed. Exact retry still
`fsync_pending`s because it does not call `cas_ref`. StepClosing
durable-before-draft order is unchanged. This does not claim the next
20+20+20 will pass K=4/20.

### Verification

- `cargo fmt --all` — clean.
- `git diff --check` — clean.
- `cargo clippy -p lattice-store -p lattice-session --all-targets -- -D warnings` — clean.
- `cargo test -p lattice-store --lib` — 47 passed, including
  `successful_cas_ref_runs_one_object_barrier_and_one_ref_barrier`.
- `cargo test -p lattice-session --lib` — 166 passed, including
  `step_closing_candidate_pin_object_barrier_failure_does_not_publish_the_candidate`,
  `step_closing_candidate_pin_uses_one_object_barrier_and_one_ref_barrier`,
  and the existing published-but-undurable exact-retry fixture.

**Changed files:** `crates/lattice-store/src/fault.rs`,
`crates/lattice-store/src/repo.rs`, `crates/lattice-store/AGENTS.md`,
`crates/lattice-session/src/git_graph/pending.rs`,
`crates/lattice-session/src/git_graph/tests/pending_crash.rs`,
`crates/lattice-session/AGENTS.md`, `crates/lancet-audit/AGENTS.md`,
`AGENTS.md`, `FINDINGS.md`, `FIX.md`,
`docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`,
`docs/plans/2026-08-18-issue139a-qualification-approval.md`.

## 2026-08-20 — GitHub #139-A barrier-fix 20+20+20 retained as a valid 9/20 / 9/20 FAIL

Source `d9744add` (redundant post-`cas_ref` `fsync_pending` removed) was
qualified on Arch/Btrfs. That run is a **valid failure**, not an
invalid-environment rerun and not a reason to raise K.

- Bundle `benchmarks/invocation-store/results/20260820T112752Z-d9744add1280/`
  (source-history SHA-256 `e9291b64…`, 20+20+20, 120 conditions).
- `qualify` exit 1: full-finalize **9/20** against K=4/20; terminal-publish
  **9/20** against K=3/20. `report` exit 0. Median-of-run full p99 81.31 ms;
  terminal 38.29 ms.
- Exceeding runs: full 1, 3, 7, 9, 14, 16, 17, 18, 19; terminal 1, 3, 6, 7,
  9, 14, 16, 17, 18.
- Most exceeding runs are both metrics. Runs 7 and 18 are consecutive shared
  stalls (22 and 16 samples; walls 64.3 s and 58.7 s). Full-only exceeding
  run 19 still matches `session_journal_pin_ns` on the recovered sample.
- The extra-fsync removal is not an incidence fix and is not proven to have
  caused the 9/20 (InvocationStore terminal path unchanged).
- K stays 4/20 and 3/20. #139-B stays blocked. L-GH139A-10 records the
  failure. Do not rerun this group to wash it green. Do not raise K to 9.

### Verification

- `python3 benchmarks/invocation-store/summarize_qualification.py report`
  on this bundle: exit 0, 9/20 and 9/20 FAIL.
- `git bundle verify` on `source-history.bundle`: HEAD `d9744add1280`.
- Bundle JSONL/conditions/summary bytes were not rewritten.

## 2026-08-20 — GitHub #139-A loose-object fanout topology (mechanism, not a qualification)

Steady-state `objects/` directory fsync was the Btrfs transaction-commit
amplifier (~863 commits per `RESULT_FINALIZE_STRESS` round). First-create now
precreates `objects/00`..=`objects/ff` and `fsync_tree`s them before rename.
Strict open validates that topology and does not repair it. Steady-state
object publication fsyncs the object file and that object's exact fanout
directory only. A ref-only barrier does not touch the object namespace.
Object-before-ref and exact-retry after a post-CAS ref-barrier failure are
unchanged. This does not weaken crash durability and is not a 20+20+20.

Five Btrfs diagnostic rounds (non-skipping fsync tracer, `TMPDIR` on the
worktree filesystem):

| Round | Commits | Commit ms | Wall s | Full p99 ms | Term p99 ms | Pin p99 ms |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 43 | 1716 | 67.72 | 611.79 | 346.69 | 252.47 |
| 2 | 43 | 2635 | 67.13 | 914.22 | 679.91 | 268.19 |
| 3 | 44 | 1815 | 76.41 | 1107.88 | 538.15 | 408.60 |
| 4 | 43 | 3208 | 67.29 | 650.31 | 363.38 | 259.34 |
| 5 | 43 | 234 | 28.85 | 44.12 | 22.83 | 20.03 |

Live `sessions.git/objects` and `invocations.git/objects` slow fsyncs: 0/5.
Remaining basename-`objects` fsyncs are first-create (`fsync_tree` of probe
`mini-staging.git` and unpublished `*.git.tmp-*`). K unchanged. #139-B
blocked. Do not treat this as a passing qualification.

### Verification

- `cargo fmt --all -- --check` — clean.
- `cargo test -p lattice-store --lib` — 59 passed.
- `cargo test -p lattice-store --test integration` — 5 passed.
- `cargo test -p lattice-session --lib` — 166 passed.
- `cargo test -p lancet-audit` — passed (ignored qualification tests not run).
- `cargo test -p lancet-runtime --lib` — 174 passed.
- `cargo clippy -p lattice-store --all-targets -- -D warnings` — clean.
- `cargo xtask lancet-boundary` — passed.
- `git diff --check` — clean.

**Changed files:** `crates/lattice-store/src/durability.rs`,
`crates/lattice-store/src/fault.rs`, `crates/lattice-store/src/repo.rs`,
`crates/lattice-store/AGENTS.md`, `FINDINGS.md`, `FIX.md`,
`docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`,
`docs/plans/2026-08-18-issue139a-qualification-approval.md`.

## 2026-08-21 — GitHub #139-A fanout 20+20+20 retained as a valid 10/20 / 9/20 FAIL

Source `d2300158` (loose-object fanout topology) was qualified on Arch/Btrfs.
That run is a **valid failure**, not an invalid-environment rerun and not a
reason to raise K.

- Bundle `benchmarks/invocation-store/results/20260820T173608Z-d2300158fee2/`
  (source-history SHA-256 `e377b753…`, 20+20+20, 120 conditions).
- `qualify` exit 1: full-finalize **10/20** against K=4/20; terminal-publish
  **9/20** against K=3/20. `report` exit 0. Median-of-run full p99 51.49 ms;
  terminal 26.53 ms.
- Exceeding runs: full 2, 4, 6, 7, 9, 10, 12, 14, 17, 20; terminal 2, 4, 6,
  7, 9, 10, 14, 17, 20.
- Nine of ten full-exceeding runs are both metrics. Runs 2, 4, 7, 10, 14, 17,
  20 are consecutive shared stalls (16–23 samples; walls ~70 s). Full-only
  exceeding run 12 still matches `session_journal_pin_ns` on the recovered
  sample (pin/rem 0.99).
- Quiet result-finalize walls ~21.5 s are consistent with the 43-commit
  diagnostic. The fanout topology is not an incidence fix.
- K stays 4/20 and 3/20. #139-B stays blocked. L-GH139A-10 records the
  failure. Do not rerun this group to wash it green. Do not raise K.

### Verification

- `python3 benchmarks/invocation-store/summarize_qualification.py report`
  on this bundle: exit 0, 10/20 and 9/20 FAIL.
- `git bundle verify` on `source-history.bundle`: HEAD `d2300158fee2`.
- Bundle JSONL/conditions/summary bytes were not rewritten.

## 2026-08-21 — GitHub #139-A RESULT_FINALIZE_STRESS uses one global sessions.git

The old harness first-created `sessions-0.git`…`sessions-19.git` inside the
measured round (and mixed 256-fanout `fsync_tree` into the same Btrfs
window as timed finalize). Each round now first-creates one global
`sessions.git` and one `invocations.git`, initializes all 20 session
fixtures, then starts the finalize timer. Durable order and 160 invocations
are unchanged. K is unchanged. The retained 10/20 / 9/20 FAIL stands as
old-benchmark evidence and must not be used to prove steady-state
implementation performance.

Structural assertions: `session_repository_create_count == 1`,
`invocation_repository_create_count == 1`, timed finalize first-creates
`== 0`. First-create durability remains in `lattice-store` tests
(`first_create_constructs_all_256_fanout_directories` and friends).

Five back-to-back Btrfs diagnostic rounds: quiet rounds 1/4/5 walls **12 s /
5 commits / full p99 44–54 ms**. Creates always 1/1/0. That is the confirmed
quiet-baseline drop from ~21.5 s / 43 commits. Rounds 2/3 stay in the raw
record and are excluded from the conclusion (contaminated host; see the
correction entry below). Not a 20+20+20. #139-B blocked.

### Verification

- `cargo test -p lattice-store --lib` — 60 passed.
- `cargo test -p lancet-audit --lib` — 130 passed.
- `cargo test -p lancet-audit --test invocation_qualification` — 3 passed,
  3 ignored.
- `cargo test -p lancet-audit --test invocation_qualification result_finalize_stress -- --ignored --exact` — passed; creates 1/1/0, 160 invocations.
- `cargo clippy -p lattice-store -p lancet-audit --all-targets -- -D warnings` — clean.
- Five Btrfs rounds as tabled in the qualification-approval record.
- Changed files: `crates/lattice-store/src/fault.rs`, `crates/lattice-store/src/repo.rs`,
  `crates/lancet-audit/tests/invocation_qualification.rs`, plus AGENTS/FINDINGS/FIX/plan.

## 2026-08-21 — GitHub #139-A diagnostic rounds 2/3 are contaminated, not stall proof

The earlier five-round note overclaimed that “the ~70 s stall window did
not disappear.” Two of five back-to-back diagnostic rounds were
contaminated by an already-stalled host; they do not determine whether the
corrected harness retains the prior incidence pattern.

Start load was `3.14, 12.97, 21.24`. Warmup already stalled (wall 39.3 s,
~19 slow tail samples). Rounds ran with no settle; each `TempDir` teardown
deleted two full-fanout repos into the next start. Round 2 commit time
1.689 s does not explain ~49 s extra wall; Round 3 commit time 4.513 s does
not explain ~45 s extra wall. Round 2 indices 98–126 slowed dispatch,
terminal publish, journal pin, reserve batch, and draft publish together.

Confirmed: quiet baseline 21.5 s → 12 s. Raw rounds 2/3 retained. Next
short diagnosis uses a compiled binary, `sync` plus load5/I/O PSI settle,
kept temp repos, inter-run commit/PSI wait, and concurrent fsync /
`commit_stats` / I/O PSI / diskstats. Not a 20+20+20. K unchanged.

## 2026-08-21 — GitHub #139-A settled 5-round RESULT_FINALIZE_STRESS

Ran the protocol above. I/O PSI `some avg10` stayed ~40–60% for 12 idle
minutes and during every round, so the PSI<5 gate is unachievable on this
host. load5 ~1.2. Creates 1/1/0. Timed Btrfs commits always 5. Quiet rounds
1/3/4: wall **12.0–12.8 s**, full p99 **42–59 ms**. Round 2: 22.7 s, 11
consecutive >100 ms, 291 fsyncs ≥10 ms on live session/invocation objects,
fanout, and refs (`commit_ms` 90). Round 5: 20.4 s, 2 consecutive, 1296 ms
ref fsync (`commit_ms` 38). Live top-level `objects/` directory slow fsyncs:
0. Kept repos 1→5 without monotonic slowdown. The old ~70 s / 16–23
consecutive window was not reproduced. Not a 20+20+20. K unchanged.
#139-B blocked.

## 2026-08-21 — GitHub #139-A qualification runner retains per-run fixtures

The corrected `RESULT_FINALIZE_STRESS` topology still dropped `TempDir` at
process exit, so a 20-run group could delete two full-fanout repositories
into the next sample. `run-qualification.sh` now prebuilds the release
`invocation_qualification` binary once and execs it for every sample. Each
run gets a unique empty child of the runner-owned qualification temp root.
Those roots stay until after every measured group and summary generation.
Ordinary `cargo test` still uses automatic `TempDir` cleanup. Domain JSONL
records `fixture_retention`, not an ephemeral absolute path. K, 100/50 ms
thresholds, and incidence gates are unchanged. No exclusive-host, PSI,
`sync`, sample-deletion, or outcome-dependent rerun gate was added.

### Verification

- `cargo fmt --all -- --check` — clean.
- `git diff --check` — clean after this entry (the previous extra EOF blank
  line in this file is gone).
- `cargo test -p lattice-store --lib` — 60 passed.
- `cargo test -p lattice-store --test integration` — 5 passed.
- `cargo test -p lattice-session --lib` — 166 passed.
- `cargo test -p lancet-audit --lib` — 130 passed.
- `cargo test -p lancet-audit --test invocation_qualification` — 15 passed,
  3 ignored.
- `cargo test -p lancet-audit --test qualification_summary` — 1 passed
  (22 Python tests: summarizer + isolation).
- `cargo test -p lancet-runtime --lib` — 174 passed.
- `cargo clippy -p lattice-store --all-targets -- -D warnings` — clean.
- `cargo clippy -p lattice-session --all-targets -- -D warnings` — clean.
- `cargo clippy -p lancet-audit --all-targets -- -D warnings` — clean.
- `cargo xtask lancet-boundary` — pass.
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed (531.5 s).

Changed files: `crates/lattice-store/src/{fault,repo}.rs`,
`crates/lancet-audit/tests/{invocation_qualification,qualification_summary}.rs`,
`benchmarks/invocation-store/{run-qualification.sh,qualification_isolation.py,test_qualification_isolation.py}`,
plus AGENTS/FINDINGS/FIX/ADR/plan.

## 2026-08-21 — GitHub #139-A corrected-harness 20+20+20 is a valid 5/20 FAIL

Official `run-qualification.sh` on committed `d474236` completed 20+20+20.
`qualify` exit 1: full-finalize **5/20** against K=4/20; terminal-publish
**5/20** against K=3/20. `report` exit 0. Bundle
`20260821T115648Z-d474236ca1e6` is retained. Creates 1/1/0; CAS 0;
Indeterminate 0; cold checkpoint 1224 in 20/20. Quiet walls ~12 s;
exceeding walls ~20 s with 2–3 consecutive samples. Isolation provenance
is present. Conditions do not show an invalid environment. K unchanged.
#139-A is not approved. #139-B remains blocked. Do not rerun this group.

### Verification

- `python3 benchmarks/invocation-store/summarize_qualification.py report \
  benchmarks/invocation-store/results/20260821T115648Z-d474236ca1e6`
  — full 5/20 (K=4) FAIL; terminal 5/20 (K=3) FAIL; exit 0.
- `git bundle verify …/source-history.bundle` — HEAD
  `d474236ca1e66b3c155c3e528da3afb9ed3f868c`.
- Source-history SHA-256 matches `environment.json`:
  `928703e51a674d17e92d641455640a6a7044eef91400ad0e24a7fe9cbc82ac16`.

## 2026-08-21 — GitHub #139-A qualification contract v2

Redesigned the #139-A qualification protocol from an explicit error model
instead of retuning K. Contract v1 gated on run incidence: at most K of 20
process runs may have a per-run p99 above 100 ms / 50 ms. Four facts from the
retained evidence retire it.

1. `ceil(160 x 0.99) - 1 = 158`, so "per-run p99" over 160 samples is the
   second-largest sample. The gate is literally "count runs holding >= 2 of 160
   samples over the threshold".
2. It tracked clustering, not slowness. `da1d2ac1` (approved) had 14
   super-threshold samples in 4 flagged runs and `3ee216ba` (approved) had 15 in
   4 with an 8.497 s maximum; `d474236` has 13 in 5 with a 4.163 s maximum and
   the lowest median-of-run p99 ever recorded — and failed.
3. A same-code control already existed. `ca768c80` -> `3ee216ba` differ only in
   benchmark files (`git diff --stat` over `crates/` is empty). Byte-identical
   product: incidence 10/20 FAIL vs 4/20 PASS, pooled p99 +1340 %, while
   median-of-run p50 differs 0.9 % and p90 0.3 %.
4. The verdict depended on where a host stall landed. Eight of `d474236`'s runs
   absorbed an ~8 s stall; run 10's arrived while the harness timer was stopped
   (process 21.46 s, harness wall 11.98 s, untimed 9.47 s against 1.26–1.93 s
   elsewhere) and it scored clean.

Contract v2 separates six generators into six gate families: D deterministic
structure, T typical cost (median-of-run p50/p90 against a baseline pinned to
`(source SHA, harness shape)`), S scaling invariance (within-run drift, derived
from ADR 0005's decoupled-from-history-depth claim), B safety (absolute maxima
under the unchanged coupling rule), F fairness, A stall attribution (every
super-threshold cluster must show >= 2 co-elevated stages and a run barrier
maximum accounting for >= 25 % of the excess; zero unattributed clusters
allowed). Run incidence, pooled quantiles, every p99-of-160 statistic, cluster
shape, and the timed/untimed split are recorded as diagnostics with no verdict.

`FinalizeBudget` is unchanged at 60 s monotonic with its 25 ms -> 1 s backoff
and its coupling rule `lock_wait_gate + 2 x full_finalize_gate + backoff_cap <=
FinalizeBudget`; a test now asserts that inequality against the manifest. No
durable ordering, failure semantics, or strict-reopen rule changed. No sample
was deleted, no bundle rerun, no recorded verdict rewritten, and K was never
raised.

| Component | Change |
|---|---|
| `benchmarks/invocation-store/qualification-contract-v2.json` | new; sole authority for every threshold, run count, and sample shape |
| `summarize_qualification.py` | rewritten as manifest-driven estimators; modes `qualify` / `report` / `baseline` / `render-gates` / `check-doc`; prints a measured/limit/verdict row per gate |
| `test_summarize_qualification.py` | rewritten; 45 contract tests |
| `run-qualification.sh` | run counts and versions read from the manifest; environment schema v5 records `qualification_contract_version` and `harness_shape_version` |
| `crates/lancet-audit/tests/invocation_qualification.rs` | stamps `HARNESS_SHAPE_VERSION = 2` on every record |
| `crates/lancet-audit/tests/qualification_summary.rs` | renamed to the v2 contract it now proves |
| ADR 0005 | sample counts move from a flat "twenty times" to the contract manifest (20 / 10 / 8, each justified) |

Run counts are 20 result-finalize / 10 local-ledger / 8 cold-reopen. The
result-finalize 20 is justified by host-stall exposure for family B
(`P(observe >= 1 stall) = 1 - 0.6^n`), not by family T, which reaches 0.32 %
standard error at n=5. The other two were oversampled: writer ratio cv 0.05 %,
max-consecutive-wait cv 0 %, and both cold headline values are exact integers.
A campaign drops from roughly 52 to 26 minutes.

Two findings the v1 contract could not see, both from bytes already retained:

- **L-GH139A-11** (new, OPEN): `uncertain_draft_revalidation_ns` times strict
  `GitSessionGraphStore::open` plus `classify_draft`. Under harness shape 2 its
  median across 20 runs rises monotonically within a run, 1.01 -> 2.57 ms, slope
  0.027 ms/op, no sawtooth at step boundaries; under shape 1 it is flat at
  0.45 -> 0.48 ms, slope 0.0004. The only `crates/` delta outside the harness is
  a first-create tracer in `lattice-store`, so strict open costs O(accumulated
  global `sessions.git` content) and it runs inside `FinalizeBudget`.
- **L-GH139A-12** (new, OPEN diagnostic): `session_journal_pin_ns` rises
  12.86 -> 15.24 ms across the eight bindings of a step and resets at every step
  boundary, in both harness shapes.

L-GH139A-04 and L-GH139A-05 are closed by this contract. The shape-1
`uncertain_draft_revalidation` p99 <= 1 ms threshold is void across the shape
change; the shape-1 K population is void. `d9744add` measured 20.899 ms against
that record's unenforced 10 ms maximum and was never adjudicated — the concrete
cost of a summarizer that enforced two of a dozen written gates.

`d474236` external evaluation under v2 (the bundle's own bytes are untouched and
its recorded contract-v1 5/20 & 5/20 FAIL stands): D pass, T
`insufficient_evidence` (predates version recording; no baseline exists), S
**FAIL** on `S-finalize-revalidation-drift` 2.26x against 1.15x, B pass with
chain `8.278 + 2x4.163 + 1.0 = 17.61 s` against 60 s (3.41x headroom), F pass, A
pass with 0 unattributed of 7 clusters.

No baseline campaign has run, so every family-T gate is `UNADJUDICABLE` and
`qualify` cannot return 0. #139-A is not approved and #139-B remains blocked.

### Verification

- `cargo fmt --all -- --check`
- `cargo test -p lattice-store --lib` — 60 passed
- `cargo test -p lattice-session --lib` — 166 passed
- `cargo test -p lancet-audit --lib` — 130 passed
- `cargo test -p lancet-audit --test invocation_qualification` — 15 passed, 3 ignored
- `cargo test -p lancet-audit --test qualification_summary` — 1 passed (51 Python contract tests)
- `cargo clippy -p lattice-store --all-targets -- -D warnings`
- `cargo clippy -p lattice-session --all-targets -- -D warnings`
- `cargo clippy -p lancet-audit --all-targets -- -D warnings`
- `cargo xtask lancet-boundary` — passed
- `env -u LATTICE_BIN cargo xtask ci` — All checks passed
- `git diff --check` — clean
- Every one of the 15 retained bundles opens under `report` with exit 0 and no
  byte written; `git status --short` shows no modification under
  `benchmarks/invocation-store/results/`.

Changed files: `benchmarks/invocation-store/{qualification-contract-v2.json,
summarize_qualification.py,test_summarize_qualification.py,
test_qualification_isolation.py,run-qualification.sh}`,
`crates/lancet-audit/tests/{invocation_qualification,qualification_summary}.rs`,
`docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`,
plus AGENTS/FINDINGS/FIX/ADR.

## 2026-08-21 — GitHub #139-A L-GH139A-11 bound plus family-T baseline provenance

Two closeout gaps, neither a contract redesign.

**L-GH139A-11.** Strict `GitSessionGraphStore::open` re-parsed every Session
state, branch, Turn pin, and PendingTurn object in the global `sessions.git`.
Unrelated accumulated history therefore grew `uncertain_draft_revalidation_ns`
inside `FinalizeBudget`. The structural regression
`strict_open_validation_work_does_not_grow_with_unrelated_session_history`
failed on HEAD before the change: eight extra sessions moved state-object
parses from the target session's coverage to a growing total.

The fix is a derived per-session projection at
`refs/lattice/session-validation/<id>`. It is authenticated to the live
`(suffix, oid)` set, rebuilt in the same kernel ref transaction as the session
mutation, and never written on open. A miss or mismatch forces a full
per-session object validation before the handle is returned. Tampered
unrelated state still fails closed (`strict_open_still_rejects_tampered_unrelated_session`
and the existing `pending_journal` strict-open guards). `lattice-store` stays
domain-neutral; there is no global cross-session write bottleneck.

**Family-T provenance.** The prose contract pinned a baseline to
`(source SHA, harness shape)`, but `render_baseline()` emitted only
`baseline_ns` values. The manifest now stores `baseline_provenance`, `qualify`
marks leftover values without provenance or a stale shape as `UNADJUDICABLE`,
and a baseline campaign still defines family T without grading it. Missing
`git_sha` fails closed. No baseline is installed yet.

`S-finalize-revalidation-drift` stays 1.15x. The retained `d474236` bundle is
untouched and remains a valid v2 FAIL on that gate. L-GH139A-12 is untouched.

| Component | Change |
|---|---|
| `crates/lattice-session/src/git_graph/validation.rs` | new; derived projection + same-transaction publish |
| `crates/lattice-session/src/git_graph/mod.rs` | skip object parse on an exact projection hit; structural work counters under `cfg(test)` |
| session write paths | `publish_session_edits` replaces bare `ref_transaction` |
| `tests/strict_open_scaling.rs` | structural non-growth + tamper fail-closed |
| `qualification-contract-v2.json` | `baseline_provenance` object, currently null |
| `summarize_qualification.py` | provenance-bound baseline fragment; fail-closed on missing/stale binding |
| `test_summarize_qualification.py` | missing provenance, stale shape, fragment identity, baseline does not self-grade |

### Verification

Focused (this worktree, before the full gate):

- `cargo fmt --all -- --check`
- `cargo test -p lattice-store --lib` — 60 passed
- `cargo test -p lattice-session --lib` — 168 passed
- `cargo test -p lancet-audit --lib` — 130 passed
- `cargo test -p lancet-audit --test invocation_qualification` — 15 passed, 3 ignored
- `cargo test -p lancet-audit --test qualification_summary` — 1 passed (51 Python contract tests)
- `cargo clippy -p lattice-store --all-targets -- -D warnings`
- `cargo clippy -p lattice-session --all-targets -- -D warnings`
- `cargo clippy -p lancet-audit --all-targets -- -D warnings`
- `cargo xtask lancet-boundary` — passed
- `git diff --check` — clean

Full gate: `env -u LATTICE_BIN LATTICE_TEST_TIMEOUT_SECS=900 cargo xtask ci` — All checks passed. The default 300s/crate bound timed out on `lattice-api` because `rewind_acceptance` is ~5 min; that crate is green under the raised bound.

Baseline campaign `20260821T192437Z-742cfd205e0c` (source `742cfd20`, harness
shape 2, contract v2): D pass, S pass including `S-finalize-revalidation-drift`
1.10x (was 2.26x on `d474236`), B pass with chain 19.50 s against 60 s (3.08x
headroom), F pass, A **FAIL** (1 unattributed of 15 clusters: run 2 sample 159,
peak 107.6 ms, `elevated_stages` empty, barrier 1.54 s). Family T is
UNADJUDICABLE. This bundle cannot define family T. No `baseline_ns` values
were written into the manifest.

Changed files: `crates/lattice-session/src/git_graph/{mod,commit,pending,refs,state,validation}.rs`,
`crates/lattice-session/src/git_graph/tests/{mod,strict_open_scaling}.rs`,
`benchmarks/invocation-store/{qualification-contract-v2.json,summarize_qualification.py,test_summarize_qualification.py}`,
`docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`,
plus AGENTS/FINDINGS/FIX.

## 2026-08-22 — Family A estimator revision 2 (component-excess attribution)

The r1 classifier required ≥2 stages at 5× the run median *and* a run-wide
`append_fsync_barrier_max` covering ≥25% of cluster excess. That misclassified
campaign `20260821T192437Z-742cfd205e0c` run 2 sample 159 (full 107.6 ms, pin
52.3, terminal 36.4, residual 18.9): none of pin/terminal/dispatch reached 5×,
and the 1.54 s barrier belonged to a different `reserve_batch` sample.

Revision 2 partitions the same sample: `residual = full - pin - terminal`.
Substantial contribution is share ≥ 1/4 of summed positive excess against that
run's p50. Attributed iff ≥2 components are substantial. Identity `full < pin
+ terminal` fails closed. `dispatch_started` and the run-wide barrier are out
of span and unused. Contract version and harness shape stay 2: no timed path
changed. `summary.json` schema is 4.

Recorded `summary.json` of `742cfd20` remains a valid r1 A FAIL (1 of 15).
Live `report` under r2: A PASS, 0 unattributed of 15. Family T is installed
from that campaign (source `742cfd20`, harness shape 2, bundle
`20260821T192437Z-742cfd205e0c`). `d474236` live r2 A is 1 unattributed of 7 (run 20 pin share 0.76);
that is an external evaluation, not a rewrite of its recorded bytes.

Independent campaign `20260821T221243Z-f9377b0dbf80` (source `f9377b0d`) is a
valid contract-v2 FAIL: T p90 193.770 ms vs 120 ms absolute, B full-max 37.422 s
vs 10 s, B revalidation-max 165.903 ms vs 25 ms, A 4 unattributed of 36 (all
pin-only substantial). D/S/F and the T p50s passed, including S-revalidation
drift 1.06×. Budget chain 84.09 s vs 60 s does not fit. Retained. No wash-green
rerun. Do not loosen A, B, T p90, or FinalizeBudget.

## 2026-08-22 — GitHub #139-A approved; performance qualification moved to #153

**Severity:** process blocker / benchmark maturity. The non-production
InvocationStore correctness and structural work is accepted for #139-A, while
the current durability-performance campaign is not stable enough to remain a
prerequisite for the #139-B hard cut.

Maintainer disposition approves #139-A and unblocks #139-B without rewriting a
historical campaign verdict or changing any durability ordering, failure
semantics, threshold, sample shape, or `FinalizeBudget`. GitHub #153 owns a
replacement performance contract and the remaining `session_journal_pin` growth
work.

Applied changes:

- diagnostic `benchmark` mode preserves strict input and sample-shape
  validation, writes the same summaries, and reports gate verdicts without
  turning them into a blocking exit. Strict `qualify` remains available for
  #153; generated baseline provenance uses the ignored local artifact path;
- `run-qualification.sh` writes new local artifacts under the ignored
  `target/invocation-store-bundles` path instead of source control;
- all 17 existing bundle directories (272 tracked files) were relocated to that
  local path and removed from the current Git tree. Every relocated file's Git
  object hash was checked against the pre-removal `HEAD` blob before commit;
- the manifest records `diagnostic_only` enforcement and points its installed
  baseline provenance at the local bundle path;
- ADR 0005, the contract-v2 decision record, crate guidance, and open findings
  now record #139-A approval, #139-B readiness, and #153 ownership. Historical
  summaries remain available in Git history and byte-identical local copies.

Verification:

- red first: benchmark-mode regression returned the unknown-mode input error;
- green: `python3 -m unittest test_summarize_qualification.py` — 54 passed;
- `cargo test -p lancet-audit --test qualification_summary` — 1 passed (Python
  contract and isolation suites passed);
- `cargo test -p lancet-audit --test invocation_qualification` — 15 passed,
  3 ignored benchmarks;
- `bash -n benchmarks/invocation-store/run-qualification.sh`;
- relocated bundle hash audit — 272/272 matched;
- `git diff --check`.

Changed files:
`benchmarks/invocation-store/{qualification-contract-v2.json,run-qualification.sh,summarize_qualification.py,test_summarize_qualification.py}`,
removed `benchmarks/invocation-store/results/**`, and updated
`{AGENTS.md,FINDINGS.md,FIX.md,crates/lancet-audit/AGENTS.md,crates/lattice-session/AGENTS.md,docs/adr/0005-durable-turn-invocation-and-private-git-stores.md,docs/plans/2026-08-21-issue139a-qualification-contract-v2.md}`.

## PR #154 CI: deterministic lattice-store SIGKILL fixture — 2026-08-22

- **ID/severity:** PR #154 CI / test-hygiene.
- **Problem:** `sigkill_mid_create_leaves_only_absent_or_fully_valid_stores` slept
  for a fixed 120 ms before SIGKILL and then required at least one published store.
  Under CI scheduling pressure the child had not reached publication, so PR #154
  failed with `child should have published some stores` despite no store damage.
- **Fix:** the child now publishes one complete store, enters a second store's
  pre-publication init callback, writes a ready marker, and parks. The parent waits
  for that exact kill point before SIGKILL, then proves the first store opens, the
  interrupted target is absent, and stale-temp reclamation permits recreation.
- **Verification:** focused test passed; the previous single-CPU contention repro
  passed 10/10; `cargo test -p lattice-store` passed 65 tests;
  `cargo clippy -p lattice-store --all-targets -- -D warnings` passed;
  `env -u LATTICE_BIN cargo xtask ci` passed all gates.
- **Changed files:** `crates/lattice-store/tests/integration.rs`, `FIX.md`.

## PR #154 follow-up: synchronize InvocationStore witness and read-path docs — 2026-08-22

- **ID/severity:** PR #154 follow-up / documentation.
- **Problem:** ADR 0005 named only the authoritative InvocationStore ledger ref and
  omitted the non-authoritative high-water witness. The #139-A decision record
  also described every public read as replaying the full ancestry from genesis,
  which no longer matched the authenticated checkpoint plus bounded suffix fast
  path used by tip reads.
- **Fix:** ADR 0005 now names both refs and records their ordered durability, crash
  repair, and fail-closed topology semantics. The decision record now distinguishes
  historical full-lineage reads from authenticated tip-checkpoint replay while
  preserving ledger events as the sole authority.
- **Verification:** `git diff --check`; reviewed against
  `crates/lancet-audit/src/invocation_store.rs` and the synchronized architecture
  guidance.
- **Changed files:**
  `docs/adr/0005-durable-turn-invocation-and-private-git-stores.md`,
  `docs/plans/2026-08-17-issue139a-decision-record.md`, `FIX.md`.

## GitHub #139-B Commit 6: production durable Turn and invocation hard cut — 2026-08-24

- **ID/severity:** #139-B / CRITICAL architecture and durability cutover.
- **Problem:** production Turn execution still crossed legacy unbound/numeric CEP identity,
  mutable PendingTurn/legacy Turn dual authority, SQLite audit/evidence authority, and
  status/disposition outcome projections. Crash recovery could not close every invocation
  from one durable Git ledger without compatibility paths.
- **Fix:** made caller-supplied `InvocationBinding` and typed `InvocationId` mandatory;
  added strict server-side durable admission; moved live execution to the journal-backed
  `TurnCoordinator` saga; persisted typed bound conversation messages and typed
  `TurnOutcome`/`ClosureOrigin`/workspace checkpoints; published ResultDraft, terminal
  material, and Cell evidence under one finite finalize budget; terminalized every skipped
  binding and stopped live continuation after cancellation/deadline/indeterminate outcomes;
  switched query/tail/replay/status and CLI surfaces to `invocations.git`; deleted SQLite
  stores/dependencies, numeric audit identity, legacy outcome schema, and the product
  `CepExecutorAdapter`; migrated black-box operator, privacy, failure, rewind, and release
  harnesses. Review follow-ups added exact uncertain-draft revalidation, typed durability and
  recovery errors, UTF-8-safe bounded errors, fail-closed journal timestamps, and deadline
  enforcement while the provider stream is still opening.
- **Verification:** `cargo fmt --all -- --check`; `git diff --check`;
  `cargo check -p lancet-protocol -p lancet-dispatch -p lancet-audit -p lancet-runtime
  -p lancet-native -p lancet-api -p lattice-session -p lattice-api -p lattice-cli
  -p lattice-tui --all-targets`; package suites for the same crates; `cargo test -p
  lattice-api` (98 unit tests plus all integrations, including Rewind 19/19);
  `cargo xtask lancet-boundary`; `cargo xtask harness-boundary`; pinned-binary system
  harnesses including `direct_failures` 8/8, `direct_secrets` 2/2, direct Tool/operator/
  mutation/workspace-history/turn-contract targets, and release operator 1/1.
- **Changed files:** protocol binding/evidence/error contracts; LANCET dispatch/runtime/native/
  API and Git operator surfaces; lattice conversation/session/agent/API/CLI/TUI; system
  harnesses; boundary gate; workspace manifests and lockfile; AGENTS.md; FIX.md.

## GitHub #139-B Commit 7: delete legacy orchestration and outcome residue — 2026-08-24

- **ID/severity:** #139-B / HIGH architecture cleanup.
- **Problem:** after the production hard cut, test-facing direct Turn commit methods,
  obsolete `GraphMessage` DTOs, duplicate AgentTurn event/status projections, and the
  temporary legacy-symbol allowlist still preserved alternate vocabulary and could
  obscure a future bypass of the PendingTurn-only production path.
- **Fix:** deleted `GraphMessage`/`GraphToolCall`, routed coordinator/model/CEP events
  directly as public `TurnEvent`, derived diagnostics from typed `TurnOutcome`, removed
  the allowlist mechanism and made the legacy scan permanently zero-hit across every
  Rust target under `crates/` and `tools/`. Removed direct commit methods/types from the
  default `SessionGraphStore`/crate API; CAS fixture helpers now compile only under
  `test-utils`, with gates rejecting direct, forwarded, or default-enabled test features
  from production dependencies. Closed M-GH100-01 with explicit UUID TurnId/operator
  isolation coverage and L-GH144-02 with the typed v4 manifest as sole authority.
- **Verification:** `cargo fmt --all -- --check`; `git diff --check`; `cargo test -p
  lattice-conversation`; `cargo test -p lattice-session`; `cargo test -p lattice-agent
  --features test-utils`; focused sibling-Turn operator isolation; `cargo test -p
  lattice-api`; `cargo test -p lattice-cli`; `cargo test -p lattice-tui`; `cargo test -p
  xtask lancet_boundary`; default-feature `cargo check -p lattice-session
  --no-default-features --lib` and `cargo check -p lattice-api --lib`; `cargo xtask
  lancet-boundary`.
- **Changed files:** conversation/session/agent/API contracts and fixtures; operator
  regressions; permanent boundary gates; AGENTS/ARCHITECTURE/FINDINGS/FIX records.


## GitHub #139-B Commit 8：端到端崩溃、Rewind、身份与隐私门禁 — 2026-08-23

**Severity:** HIGH

补齐生产 durable Turn/Rewind hard cut 的真实进程崩溃证明与边界回归。新增仅由
`test-crash-barriers` feature启用的fsynced acknowledged barriers，覆盖
`TurnStarted`、model attempt、binding/reservation/Bound、`DispatchStarted`、
candidate/draft/terminal、每个多Tool cursor、`StepClosed`、`TurnClosing` final
transaction，以及 Rewind 的 workspace-history、Session ref、success-audit 三个窗口。
Provider请求与Tool执行使用位于workspace/XDG之外的append-only fsynced日志；每个case
在SIGKILL前验证非零计数、typed `InvocationId`和精确durable stage，strict reopen后验证
provider不重放、Tool不重派发、`active-turn`仅在完整closure后删除。

崩溃矩阵发现并修复四个真实恢复缺口：Tool-bearing assistant未进入reconciled transcript；
已有pinned candidate恢复时错误重发cursor-0 baseline；已有exact terminal被重复发布；
`StepClosed`未持久化完整post-user transcript delta。`StepClosingState`现以required
`messages`作为单一provider transcript authority，validator逐cursor校验assistant、
ToolResult身份/outcome/content与material digest/length，live/reconcile/TurnClosing复用同一
bytes。另修Rewind workspace barrier原先误放在Session ref移动之后，并使workspace
checkpoint lock在Session repo写入前释放，避免双repo lock重叠。

同时收紧身份/隐私：public Tool started/finished事件统一使用durable `InvocationId`；native
admission错误不回吐store path/detail；operator DTO隐私测试改为真实shape gate；duplicate
admission断言ledger tip不变；同parent sibling Turn operator隔离回归落地。

**Verification:**

- `cargo test -p lattice-session` — 132 passed。
- `cargo test -p lancet-audit` — 56 unit + contract/qualification suites passed。
- `cargo test -p lancet-native` — all targets passed。
- `cargo test -p lattice-api` — 99 unit + integrations passed；`release_e2e` passed。
- feature binary Turn SIGKILL matrix — 4/4 targets passed，含multi-Tool cursor-1/cursor-2。
- feature binary Rewind SIGKILL matrix — 3/3 action-to-audit windows passed。
- `cargo xtask lancet-boundary` / `cargo xtask harness-boundary` — passed。
- `cargo fmt --all -- --check` / `git diff --check` — clean。

**Changed areas:** `lattice-session::pending`, `lattice-api::turn`/Rewind,
`lancet-native` admission, `lancet-audit` operator regressions, and
`lattice-system-harness` crash/provider/effect fixtures.

## GitHub #139-B Commit 9: production provider codec hard cut and ledger closeout — 2026-08-24

**Severity:** HIGH

**Problem:** The durable Turn/Invocation cutover was complete, but production provider
adapters still encoded request messages independently, the Agent still retained a parallel
response assembler, Gemini synthesized `tc_*` Tool-call IDs, and wire usage had a second
`u32` authority. That left replay identity, ToolResult joining, malformed-batch rejection,
and provider-switch behavior dependent on adapter-local conventions. Current architecture
and onboarding documents also still described transitional SQLite/audit and direct-loop
surfaces.

**Fix:** `ChatRequest.messages` now carries `NativeMessage<BoundToolCall>` directly.
All three production request builders call the single `request_messages_to_wire` projector;
all SSE parsers accumulate the provider-complete response and publish one typed
`MessageComplete` through `wire_to_native`; all usage goes through `wire_usage` into the
single `lattice-conversation::TokenUsage { u64 }` vocabulary. Same-provider opaque replay
state is byte-preserved, cross-provider projection intentionally drops provider IDs and
reasoning, ToolResults join by durable `InvocationId`, and a missing provider call ID fails
closed without synthesis. The legacy request DTOs, buffered `chat`/`ChatResponse` surface,
Agent-side assembler, duplicate usage type, and Gemini `tc_*` generation were deleted.
Malformed arguments/usage, conflicting stream fragments, duplicate terminal frames, and
multi-chunk OpenAI/Gemini responses now have production-seam regressions. A source boundary
test requires every shipped provider to use the shared request, response, and usage
projectors.

Final-gate review also closed adjacent correctness gaps without compatibility paths: public
Session diff ancestry now excludes PendingTurn journal commits; observation and operator
harnesses assert the typed Git-ledger/error envelopes; native finished events derive status
from the exact result; Turn startup fails closed if the persisted SessionId or frozen LANCET
policy cannot be represented. `L-GH139B-01` moved to CLOSED, while historical ADR/plan
records remain historical rather than being rewritten.

**Verification:**

- `cargo test -p lattice-llm` — 81 unit + 34 official-wire projection tests passed.
- `cargo test -p lattice-agent --features test-utils` — 12 unit + 5 streaming-seam
  tests passed.
- `cargo test -p lattice-session` — 132 passed.
- `cargo test -p lattice-api` — 99 unit plus every integration, including 19 Rewind
  acceptance cases, passed.
- `LATTICE_BIN=$PWD/target/debug/lattice cargo test --workspace` — passed, including
  the complete ordinary black-box system-harness matrix.
- `env -u LATTICE_BIN cargo xtask ci` — passed: format, Clippy `-D warnings`, debug
  binary, system/LANCET boundaries, generated Tool artifacts, all 27 workspace-member
  suites, black-box scenarios, Component identities, and performance regression gates.
- Final specification and security reviews: PASS, no blocking findings.

**Changed areas:** `lattice-llm` provider projection/parsers/contracts,
`lattice-agent::model_step`, Turn test fixtures and startup wiring, Session public ancestry,
typed black-box harness assertions, `FINDINGS.md`, and this append-only record.

## GitHub #157: bind Turn recovery to the authoritative Session project stores — 2026-08-25

**Severity:** P0 / BLOCKER

**Problem:** Turn startup derived project stores from the request workspace and reopened the
layout-default `sessions.git` before proving that those handles matched the persisted Session and
PendingTurn project binding. A wrong workspace, custom Session repository, or mismatched
InvocationStore/WorkspaceHistory backing could therefore reach reconciliation through the wrong
repository authority.

**Fix:** `TurnStartedFacts` now persists a strict typed `ProjectStoreKey`; PendingTurn and Session
store schemas hard-cut to v2 and strict-open rejects old or malformed development state. Turn
startup holds the authoritative Session execution lock, reads the Session/PendingTurn binding from
the injected Git store, compares the canonical request root before opening project stores, and
passes the same key into `TurnCoordinator`. Runtime composition independently verifies canonical
Local Workspace containment plus matching InvocationStore/WorkspaceHistory keys and StateLayout
roots before provider, reservation, dispatch, mutation, or reconciliation work. No compatibility
reader, import, fallback, dual write, or reverse crate dependency was added.

**Verification:**

- `cargo test -p lattice-session -p lancet-audit -p lancet-runtime -p lancet-api -p lattice-api`
  passed: Session 134 tests, LANCET runtime/API/audit suites, lattice-api 104 unit tests and every
  integration including 19 Rewind cases.
- `cargo clippy -p lattice-session -p lancet-audit -p lancet-runtime -p lancet-api -p lattice-api --all-targets -- -D warnings` — passed.
- `cargo xtask lancet-boundary` / `cargo xtask harness-boundary` — passed.
- `cargo fmt --all -- --check` / `git diff --check` — clean.
- `cargo tree` checks confirmed no `lancet-audit -> lattice-session`,
  `lattice-session -> lancet-audit`, or `lancet-api ->` LATTICE-layer edge.

**Changed areas:** `lattice-session` PendingTurn/session-store contracts, `lattice-api` Session and
Turn startup/coordinator wiring, `lancet-api` runtime store validation, InvocationStore key
exposure, and focused unit/integration fixtures.

## GitHub #158: isolate CEP requests and preserve post-dispatch uncertainty — 2026-08-25

**Severity:** P0 / BLOCKER

**Problem:** CEP transport used durable `InvocationId` as cancellation/task correlation and mapped a
lost response channel to one generic transport failure. A caller drop could therefore leave stale
cancellation state for another request, while a worker panic after durable `DispatchStarted` was
reported as deterministic failure even though the Tool may already have executed.

**Fix:** Transport now gives every request a private pointer-identity key while retaining
`InvocationId` exclusively as the domain binding/event/ledger identity. All completion, panic,
shutdown, queue-drop, and cancellation paths clean up the exact request entry. Native admission
marks the shared response phase only after `InvocationStore::admit_reserved` has durably published
`DispatchStarted`; response loss before that boundary is `ExecutionFailed`, while loss afterward is
typed `OutcomeIndeterminate` and the saga persists `Indeterminate(MayHaveOccurred)` without
redispatching later bindings. Async and sync EventSink paths overwrite payload-provided identity
with the host-bound invocation. Admission/store/JoinError messages are stable, bounded, path-free
classifications. No unsafe post-dispatch abort or observer-backpressure scope expansion was added.

**Verification:**

- `cargo test -p lancet-dispatch -p lancet-native -p lancet-api -p lattice-api` — passed: dispatch
  7 tests, native 49 unit plus all integrations, LANCET API 10, lattice-api 105 unit plus every
  integration including 19 Rewind cases.
- `cargo clippy -p lancet-protocol -p lancet-dispatch -p lancet-native -p lancet-api -p lattice-api --all-targets -- -D warnings` — passed.
- `cargo xtask lancet-boundary` / `cargo xtask harness-boundary` — passed.
- `cargo fmt --all -- --check` / `git diff --check` / LSP diagnostics — clean.

**Changed areas:** `lancet-dispatch` request transport and EventSink, `lancet-native` admission/error
mapping and recovery regressions, `lattice-api` saga indeterminate/batch-stop coverage, and current
crate-local architecture guidance.

## GitHub #159: preserve cumulative Turn recovery and authoritative workspace checkpoints — 2026-08-26

**Severity:** P0 / BLOCKER

**Problem:** PendingTurn snapshots retained only the current model step's transcript delta and the
reconciled candidate substituted an empty workspace-history checkpoint. A crash after a Tool-bearing
step could therefore lose bound provider replay state or prior ToolResults, and a post-mutation
reopen could commit a Turn without the WorkspaceHistory OID that the Tool actually produced.
Strict reopen also lacked a finite lineage bound and exact authentication for several same-stage,
step-closing, and Turn-closing fields.

**Fix:** Pending snapshot/session-store schema v3 carries one mandatory cumulative post-user
transcript authority. Every journal edge derives and validates its exact transcript prefix; candidate
revision usage, ContinueModel closures/digest, TurnClosing basis, and Committed candidate are
authenticated exactly. Provider batches are bounded at 32 calls, frozen attempts at 11, and the
complete PendingTurn lineage at 2,979 snapshots; reopen retains only bounded OID metadata and two
adjacent full snapshots. Live and reopen share one candidate-basis derivation. The composition layer
samples a canonical 40-lowercase-SHA-1 WorkspaceHistory checkpoint while holding the shared
WorkspaceOperationLock through TurnClosing and final Session publication. Strict Session
constructors serialize cross-ref validation with repository writers, finalized manifests validate
the same checkpoint shape, and usage aggregation saturates rather than wrapping. No compatibility
reader, placeholder reconstruction, reverse dependency, or second transcript authority was added.

**Verification:**

- `cargo test -p lattice-conversation` — 32 passed.
- `cargo test -p lattice-llm` — 81 unit + 34 provider-projection tests passed.
- `cargo test -p lattice-session` — 152 passed, including strict lineage, edge-authentication,
  constructor-lock, malformed-manifest, saturation, exact-retry, and cumulative-transcript cases.
- `cargo test -p lattice-api` — run twice; each run passed 107 unit tests and every integration,
  including all 19 Rewind acceptance cases.
- `cargo clippy -p lattice-store -p lattice-conversation -p lattice-session -p lattice-api -p lancet-audit --all-targets -- -D warnings` — passed.
- `cargo clippy -p lattice-system-harness --all-targets --features crash-barriers -- -D warnings` — passed.
- `cargo xtask lancet-boundary` / `cargo xtask harness-boundary` — passed.
- Worktree-local feature binary plus `direct_crash_barriers` — 7 real SIGSTOP/SIGKILL/reopen cases
  passed, including next-model-attempt transcript recovery and both pre-/post-TurnClosing mutation
  checkpoint windows without provider replay or Tool redispatch.
- Final `gpt-5.6-sol:xhigh` risk re-review: PASS.
- `cargo fmt --all -- --check` / `git diff --check` / LSP diagnostics — clean.

**Changed areas:** `lattice-conversation` Tool batch contract, `lattice-session` PendingTurn
contracts/store/strict-open validation, `lattice-api` live/reopen checkpoint composition, real crash
barriers, focused durable corruption regressions, crate guidance, and this append-only record.

## GitHub #160: make Rewind placement, locking, partial state, and crash recovery exact — 2026-08-26

**Severity:** P0 / BLOCKER

**Problem:** Rewind still parsed WorkspaceHistory ownership through the retired
`<session>:<parent>` convention and fell back to timestamps, planned before acquiring the
workspace operation lock, accepted a constructible unbound lock marker, filtered a globally bounded
audit page after the bound, conflated target and actual durable refs in partial errors, accepted
non-first-parent private chains, and wrote crash acknowledgements directly to the final path.
Repeated execution after the post-audit crash window could append a duplicate audit record.

**Fix:** Workspace mutation/audit records now carry typed `TurnId`/`SessionId`; history schema v3
requires unique canonical trailers and strict single-parent structure. Full-unwind placement uses
only the selected Session's typed lineage. Rewind acquires one opaque, exact-backing, exclusive
`HeldWorkspaceOperation` before any plan or preflight and retains it through audit publication.
Audit records use bounded completeness-aware per-Session first-parent refs, so foreign Sessions
cannot displace records. Typed partial state separates attempted history target from fresh actual
WorkspaceHistory, Session-head, and audit observations, and CLI JSON/text renders the complete
contract. Exact completed retries replay a strictly validated success without restore, pin, ref
movement, or audit append; a Session-moved/pre-audit crash appends one missing close record.
Feature crash acknowledgements use same-directory unique temp files, complete write + file fsync,
Linux `renameat2(RENAME_NOREPLACE)`, and parent-directory fsync before SIGSTOP.

**Verification:**

- `cargo test -p lancet-runtime` — 243 tests passed across all targets.
- `cargo test -p lancet-api` — 10 tests passed.
- `cargo test -p lattice-session` — 152 passed.
- `cargo test -p lattice-api` — 109 unit tests plus every integration passed; Rewind acceptance
  passed 32/32, including typed placement, token mismatch, partial-state, corruption, and exact-retry
  regressions.
- `cargo test -p lattice-cli` — 132 unit and 4 integration tests passed.
- Worktree-local crash-feature binary plus `direct_rewind_crash_barriers` passed the real
  workspace-applied, Session-moved, and audit-appended SIGSTOP/SIGKILL/retry windows; provider
  requests stayed 4, Tool effects stayed 2, each audit page held exactly one success, and an armed
  completed retry created no append acknowledgement.
- Clippy with `-D warnings` passed for all affected crates under default and crash-barrier features.
- `cargo xtask lancet-boundary` / `cargo xtask harness-boundary` passed; dependency inspection
  preserved the layer graph and the no-two-repository-lock contract.
- Final `gpt-5.6-sol:xhigh` read-only risk review: PASS.
- `cargo fmt --all -- --check`, `git diff --check`, and primary LSP diagnostics were clean.

**Changed areas:** WorkspaceHistory typed records/strict traversal, Rewind audit topology/schema,
opaque operation-lock authority, product Rewind coordinator/partial/retry paths, CLI rendering,
production-binary crash harness, current AGENTS guidance, and this append-only record.

## GitHub #160 follow-up: restore full-CI wall-time margin without weakening coverage — 2026-08-26

**Severity:** P0 / BLOCKER (acceptance infrastructure)

**Problem:** The first combined P0 `cargo xtask ci` run exposed two wall-time failures. Twelve
new #160 contracts had been expressed as separate full-runtime Rewind scenarios, making the Rewind
target take 264 seconds and the complete `lattice-api` package exceed the fixed 300-second per-crate
CI ceiling. The 18-call observation black-box scenario also took 29.80 seconds under a 30-second
per-step budget, so normal four-way CI contention exhausted its margin.

**Fix:** Pure lock-token, completed-replay corruption, typed placement, and partial-state contracts
moved into fast crate-local test modules at their owning seams. Related product assertions remain in
20 shared full-runtime Rewind scenarios, including the real audit-failure, unrelated-prior-Rewind,
partial-state, lock-lifetime, and action-to-audit paths; no assertion was ignored or deleted without
equivalent lower-level coverage. The observation scenario alone now has a 60-second step budget,
with the 180-second total and every global harness/xtask timeout unchanged.

**Verification:**

- Timed `cargo test -p lattice-api` — 137.86 seconds, down from over 300 seconds; 112 unit tests
  plus every integration passed. Rewind acceptance passed 20/20 in 20.76 seconds.
- `direct_observation_visibility_and_bounds` — passed in 30.18 seconds under the scenario-local
  60-second step budget; all 18 Tool calls and 19 provider captures remain asserted.
- `cargo clippy -p lancet-api -p lattice-api -p lattice-system-harness --all-targets --
  -D warnings`, formatting, diff checks, and primary LSP diagnostics — clean.
- `env -u LATTICE_BIN cargo xtask ci` — passed from the final combined P0 tree: format, Clippy,
  branch-local debug binary, both structural boundaries, generated Tool artifacts, 27-crate
  four-way test matrix under the unchanged 300-second ceiling, black-box harness, Component
  identities, and performance-regression gates.

**Changed files:** `crates/lancet-api/src/rewind.rs`, `crates/lattice-api/src/rewind.rs`,
`crates/lattice-api/tests/rewind_acceptance.rs`,
`crates/lattice-system-harness/tests/direct_observation.rs`, and `FIX.md`.
## 2026-08-26 — GitHub #156 TUI mock backend and deterministic visual fixture

- **ID/severity:** GitHub #156 / developer-experience enhancement.
- **Problem:** exercising the real full-screen TUI required a paid provider and
  hard-to-repeat live conversations; the existing fake SSE server, PTY driver, and
  `TestBackend` checks had no reusable scenario catalog or one-command interactive
  entry, and rendering was only checked for non-panicking output.
- **Fix:** added `cargo xtask tui-mock --scenario <name>`, which builds the current
  `lattice` binary and launches its real TUI in an isolated harness world against
  the existing fake provider. The reusable catalog supplies `long-text`,
  `tool-call`, `disconnect`, `slow`, and `error`; the slow case uses the existing
  response gate so Esc exercises cooperative cancellation. The existing PTY smoke
  now consumes the shared long-text scenario, and the TUI has an exact 40×8 raw
  buffer regression without a snapshot framework.
- **Verification:** `cargo test -p lattice-tui` (25 passed);
  `cargo test -p lattice-system-harness --lib` (63 passed);
  `LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness --test pty_tui_smoke`
  (1 passed); `cargo test -p xtask` (115 passed);
  `cargo xtask harness-boundary` passed; one-command entry smoke entered the
  alternate screen and exited cleanly through `/quit`;
  `env -u LATTICE_BIN cargo xtask ci` passed all gates.
- **Changed files:** `crates/lattice-system-harness/src/{lib.rs,tui_mock.rs,bin/tui_mock.rs}`,
  `crates/lattice-system-harness/tests/pty_tui_smoke.rs`,
  `crates/lattice-tui/src/view.rs`, `xtask/src/{main.rs,tui_mock.rs}`, `FIX.md`.

## 2026-08-26 — GitHub #156 follow-up: remove mock prompt startup stall

- **ID/severity:** GitHub #156 / developer-experience performance regression.
- **Problem:** after the TUI was visibly ready, Enter still took about 6.3 seconds
  to show deterministic mock content. Boundary timing proved the fake provider
  read and wrote its response in under 1 ms; the delay occurred before dispatch,
  where the unoptimized dev binary spent about 6.2 seconds compiling the closed
  embedded Component catalog on every Turn.
- **Fix:** the mock xtask now builds `lattice` with a dedicated `tui-mock` Cargo
  profile and selects that profile's binary. Workspace crates remain unoptimized
  for quick TUI iteration, while dependencies use `opt-level=1`, accelerating
  Wasmtime without changing or bypassing production Turn composition, closed-
  catalog validation, policy, audit, or the formal fake-provider route.
- **Verification:** red regression first observed the xtask still selecting
  `target/debug/lattice`; the focused xtask test now passes and pins the profile
  plus artifact path. A PTY timing loop measured 6.299 seconds before the fix and
  0.727/0.727/0.694 seconds after it (about 9× faster).
  `cargo test -p xtask` passed 116 tests; the custom-profile build and PTY smoke
  passed; `env -u LATTICE_BIN cargo xtask ci` passed all gates.
- **Changed files:** `Cargo.toml`, `xtask/src/tui_mock.rs`, `FIX.md`.

## 2026-08-27 — L-21-03 allow explicit loopback LLM endpoints

- **ID/severity:** L-21-03 / MEDIUM configuration correctness.
- **Problem:** `net_guard::validate_base_url` admitted `localhost` but rejected the
  equivalent `127.0.0.1` and `::1` literals as private addresses, even though
  its HTTP-scheme check and the provider request client treated all three as
  loopback. A locally hosted OpenAI-compatible provider therefore failed before
  dispatch with `base_url targets private/reserved IP 127.0.0.1`.
- **Fix:** reuse the shared localhost predicate for both the HTTP-scheme and IP
  checks, exempting only loopback literals while retaining rejection of every
  other private/reserved IP. The fake provider now returns its bound loopback
  address directly so the PTY product path permanently exercises the regression.
- **Verification:**
  - `cargo test -p net-guard`; `cargo test -p lattice-llm`;
    `cargo test -p lattice-system-harness --lib`.
  - Rebuilt `lattice`, then ran the product-level PTY smoke with the fake
    provider's explicit `127.0.0.1` endpoint; passed.
  - `cargo test -p lancet-authority`;
    `cargo test -p lancet-native -- --skip fetch_effect_contract_negative`.
  - Relevant all-target Clippy with `-D warnings`, formatting, and primary LSP
    diagnostics — clean.
  - `env -u LATTICE_BIN cargo xtask ci` — passed every full-workspace gate.
- **Changed files:** `crates/net-guard/src/lib.rs`,
  `crates/lattice-system-harness/src/fake_llm.rs`,
  `crates/lattice-llm/AGENTS.md`, `FINDINGS.md`, and `FIX.md`.

## 2026-08-27 — make `session.todo_write` schema OpenAI-compatible

- **ID/severity:** OpenAI-compatible Tool schema / HIGH functional compatibility.
- **Problem:** the default Tool catalog projected `session.todo_write` content with
  `pattern: "[^\\p{White_Space}]"`. The host validator accepted that Unicode
  property escape, but OpenAI Responses rejected it as
  `invalid_function_parameters` (`is not a 'regex'`). OpenCodex's Chat translator
  masked the upstream 400 as a stream-internal truncated-response error, so every
  default-agent Turn stopped before producing assistant output.
- **Fix:** replaced the unsupported property escape in the sole source manifest's
  params and result schemas with the exact explicit Unicode `White_Space` code-point
  ranges. This preserves `content.trim().is_empty()` parity without weakening to
  `\S`. Regenerated the descriptor, LLM projection, expected identity, and host
  composition through `cargo xtask tool-codegen`; no generated file was hand-edited.
- **Verification:**
  - Before the fix, the captured Lattice request failed deterministically; removing
    only this pattern succeeded, and direct Responses exposed the exact upstream 400.
  - The provider-compatible explicit range succeeded through OpenCodex Chat streaming.
  - `blank_content_rejection_matches_trim_over_all_unicode_whitespace` passed across
    the complete Rust Unicode whitespace set; descriptor params/result remained equal.
  - Rebuilt `lattice` completed a fresh real OpenCodex Turn with all five default
    Tools and reported 1,292 prompt plus 13 completion tokens.
  - `cargo xtask tool-codegen --check`; 69 focused tool-codegen tests; formatting,
    Clippy, all workspace/harness/Component identity checks, and
    `env -u LATTICE_BIN cargo xtask ci` — passed.
- **Changed files:** `tools/todo-write/manifest.toml`,
  `tools/todo-write/generated/{descriptor.json,llm-projection.json,expected-identity.json}`,
  `crates/lancet-runtime/src/generated/host_composition.rs`, and `FIX.md`.

## 2026-08-27 — GitHub #32 migrate `fetch.web` to a closed Component Tool

- **ID/severity:** GitHub #32 / HIGH architecture and network-authority migration.
- **Problem:** `fetch.web` remained the final native catalog method, with a second native
  descriptor/handler/effect-contract stack beside the ADR 0004 manifest-defined Component
  catalog. That kept the native production registry non-empty, split network admission and
  evidence ownership across layers, and blocked retirement of the native/CEP dispatch seam.
- **Fix:** added `tools/fetch-web` as a manifest-defined WASM Component and generated its
  descriptor, LLM projection, identity, and embedded host composition. Added one GET-only
  `network-fetch` Tool Host import in `lancet-runtime` with exact Cell admission, URL and
  header validation, DNS rebinding/SSRF checks, pinned candidates, TLS verification, bounded
  redirects/body/headers, cancellation/deadline handling, pre-I/O observation-budget refusal,
  and typed per-hop network evidence. The native catalog is now empty; native `fetch.web`
  handlers, executor, policy matrix, fuzz scaffolding, dependencies, and obsolete warn report
  were deleted. Legacy non-empty `[lancet.effects] deny` now fails closed and points operators
  to `[lancet.capabilities] deny`; the resulting diff deletes roughly 2,000 more lines than
  it adds.
- **Verification:**
  - Focused `fetch.web` Component round trip, dogfood evidence, localhost denial, scheme
    denial, response limit, and authority/network-policy regressions passed.
  - `cargo test -p lancet-authority -p lancet-runtime -p lancet-api` passed, including 200
    `lancet-runtime` tests; affected all-target Clippy with `-D warnings` passed.
  - Independent general and security reviews found no blocking issue after fixes; the
    security re-review reported zero Critical, High, or Medium findings.
  - `env -u LATTICE_BIN cargo xtask ci` passed formatting, Clippy, boundary gates, generated
    Tool drift checks, every workspace/guest test, Direct-Local harness suites, and doctests.
- **Changed files:** `tools/fetch-web/**`, `crates/lancet-runtime/{wit/tool-host.wit,src/**}`,
  `crates/lancet-protocol/src/evidence.rs`, native catalog/server and authority-policy cleanup,
  LATTICE API/profile/harness migrations, generated host composition, workspace manifests,
  architecture/testing documentation, `FINDINGS.md`, and `FIX.md`.

## 2026-08-27 — GitHub #32 post-implementation security and acceptance review fixes

- **ID/severity:** GitHub #32 review follow-up / HIGH evidence confidentiality plus MEDIUM
  network, resource-bound, and acceptance-contract defects.
- **Problem:** durable network evidence retained URL query credentials; IPv6 special-purpose
  classification omitted current IANA ranges; post-DNS policy refusals were mislabeled as
  pre-I/O denials; and one byte field conflated data received by the host with data returned
  to the guest. Component startup also exposed a caller-supplied production catalog seam and
  created one Tokio runtime per Tool call. Two acceptance tests could pass without traversing
  the policy/finalization contracts they named.
- **Fix:** evidence now strips userinfo/query/fragment and separates bounded raw transport
  bytes from guest-delivered shaped bytes. Tool egress rejects the missing IPv6 ranges,
  DNS-or-later failures stay effects, and network targets require canonical URL-host syntax.
  Production composition always uses the
  shared generated inventory, initializes off async workers, and owns one bounded async/DNS
  runtime. The direct policy test now admits `mutate.create` before checking the write deny;
  dogfood finalization now persists the production canonical `ResultDraft` before terminal
  publication. Regression coverage pins URL redaction, IPv6 boundaries, typed capability
  errors, shared composition, and received/delivered evidence.
- **Verification:**
  - `cargo test -p net-guard -p lancet-protocol -p lancet-runtime -p lancet-native -p
    lancet-api -p lattice-api -p lattice-tui` — passed.
  - `env -u LATTICE_BIN cargo xtask ci` — passed formatting, Clippy, boundary and generated
    artifact gates, every workspace/Component test, Direct-Local harness suites, and doctests.
- **Changed files:** network guard/evidence/runtime composition and host code, API/TUI startup
  seams, Direct-Local and dogfood acceptance tests, #32 decision and architecture documents,
  and `FIX.md`.

## 2026-08-27 — PR #175 newest-20 operator scenario timeout

- **ID/severity:** PR #175 CI follow-up / LOW test-budget defect.
- **Problem:** `direct_operator_status_bounds_twenty_newest_invocations` sends 22 Tool calls in
  one Turn so it can prove `lancet status` keeps exactly the newest 20. After `fetch.web` moved
  to the Component path, the GitHub runner exceeded the generic 30-second command timeout and
  reported `TimedOut(30s)`. The same test took 25.7 seconds locally, leaving too little runner
  margin; product assertions and every other CI test passed.
- **Fix:** give only this intentionally heavy scenario a 60-second step timeout. Keep the shared
  Direct-Local default at 30 seconds and the scenario total budget at 180 seconds.
- **Verification:** the exact test under two same-core CPU burners failed before the fix at
  30.10 seconds and passed after it at 39.30 seconds. The full `direct_operator` target passed
  6/6, and `env -u LATTICE_BIN cargo xtask ci` passed all gates.
- **Changed files:** `crates/lattice-system-harness/tests/direct_operator.rs`, `FIX.md`.

## 2026-08-27 — main concurrent mutation scenario timeout

- **ID/severity:** main CI after PR #175 / LOW test-budget defect.
- **Problem:** `direct_mutation_concurrent_same_target_create_has_one_winner` starts two sends for
  one Session. The full-Turn Session lock intentionally serializes them, so the process that
  loses the lock can spend one command budget waiting for the first Turn and then executing its
  own Component Turn. On the GitHub runner that path exceeded the generic 30-second step timeout;
  one send had no events while the other completed, obscuring the timeout as a 1-winner/0-loser
  assertion failure.
- **Fix:** give only this lock-competition scenario a 60-second step timeout and assert both send
  processes exited normally before checking Tool winner/loser semantics. Keep the global
  Direct-Local default at 30 seconds and the scenario total budget at 180 seconds.
- **Verification:** forcing the waited process behind the lock under four same-core CPU burners
  reproduced the CI event shape and failed at 40.31 seconds before the fix; the same reproducer
  passed after the fix at 55.96 seconds. The exact test and the full `direct_mutation` target
  passed 1/1 and 7/7. `env -u LATTICE_BIN cargo xtask ci` passed all gates.
- **Changed files:** `crates/lattice-system-harness/tests/direct_mutation.rs`, `FIX.md`.
