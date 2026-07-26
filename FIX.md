# FIX — Changelog

Applied fixes across code review phases. Entries are append-only; never delete or overwrite.

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
