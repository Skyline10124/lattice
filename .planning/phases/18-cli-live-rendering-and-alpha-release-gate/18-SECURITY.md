---
phase: 18
slug: cli-live-rendering-and-alpha-release-gate
status: verified
threats_open: 0
asvs_level: 1
created: 2026-05-22
---

# Phase 18 -- Security

> Per-phase security contract: threat register, accepted risks, and audit trail.

---

## Trust Boundaries

| Boundary | Description | Data Crossing |
|----------|-------------|---------------|
| Network-to-CLI | `lattice_api::stream_turn` returns `TurnEventStream` over internal async channel from provider API responses | TurnEvent values (String, usize, bool, Option<T>) -- no secrets |
| Output-to-User | stdout carries TokenDelta (human) or JSONL TurnEvent objects; stderr carries status/lifecycle/error lines | User-visible content, session/commit identifiers |
| State-to-ExitCode | RunRenderState accumulates stream signals; exit_code_from_state derives process exit code | TerminalSignal, CommitSignal, RunTurnStatus -- integrity boundary |
| Script-to-Build | release-gate.sh executes cargo commands in workspace | Build artifacts -- no secrets |
| Input-to-Profile | `LATTICE_AGENTS_DIR` env var, `--profile` CLI arg, session ID arg determine filesystem paths | Path strings -- traversal risk mitigated by validate_agents_dir |
| Tool-to-FS | DefaultToolExecutor reads/writes files under project_root | File paths -- traversal risk mitigated by path_guard |

---

## Threat Register

| Threat ID | Category | Component | Disposition | Mitigation | Status |
|-----------|----------|-----------|-------------|------------|--------|
| T-18-01 | Spoofing | stream_turn channel | accept | In-memory Rust channel; no external injection path. Typed TurnEvent enum prevents arbitrary data shapes. | closed |
| T-18-02 | Spoofing | JSONL output | accept | Local stdout/stderr only; no network transport. No MITM applicable. | closed |
| T-18-03 | Tampering | TurnEventStream ordering | mitigate | Rust channel semantics guarantee FIFO ordering; `while let Some(event) = stream.next().await` consumes sequentially (run.rs:439) | closed |
| T-18-04 | Tampering | RunRenderState mutation | mitigate | Private struct mutated only by `apply_event_to_state` (run.rs:90) and `flush_before_status` (run.rs:292) within single-threaded stream loop; no concurrent access | closed |
| T-18-05 | Tampering | release-gate.sh integrity | accept | Git version control; `set -euo pipefail` prevents silent failure; no toolchain signing in alpha | closed |
| T-18-06 | Tampering | path traversal via agents_dir/profile | mitigate | `validate_agents_dir` (run.rs:572) rejects `..` components, dangerous system prefixes (`/etc`, `/usr`, `/var`, `/sys`, `/proc`, `/dev`, `/boot`), and `/tmp`. Tool executor `reject_parent_traversal` (path_guard.rs:15) and `ensure_under_project_root` (path_guard.rs:35) guard file operations | closed |
| T-18-07 | Repudiation | stream event audit trail | accept | CLI renders but does not persist audit log; underlying `lattice_session` SQLite stores turn history. Audit logging deferred to post-alpha | closed |
| T-18-08 | Repudiation | exit code derivation | mitigate | `exit_code_from_state` (run.rs:152) uses deterministic D-A4 matrix; defensive `last_terminal.is_some()` guard added per WR-03 fix. Tests cover all 7 rows (run.rs:1211) | closed |
| T-18-09 | Info Disclosure | ReasoningDelta to stdout | mitigate | Human mode: ReasoningDelta only on stderr when verbose (run.rs:223-227), hidden entirely otherwise. JSON mode: emitted as JSONL per D-D3 raw-stream contract, documented in rustdoc and --json help text | closed |
| T-18-10 | Info Disclosure | session_id/commit_id in JSONL | accept | Internal identifiers, not secrets. TurnCommitted/TurnStarted events carry these by API design | closed |
| T-18-11 | Info Disclosure | ANSI escapes in JSON mode | mitigate | `render_json_event` (run.rs:179) uses `serde_json::to_string` without `colored` crate. Test `render_json_event_emits_no_ansi_escapes` (run.rs:941) verifies zero 0x1B bytes. Documented at run.rs:178 | closed |
| T-18-12 | Denial of Service | serde_json::to_string panic in render_json_event | accept | `.expect("TurnEvent serializes infallibly")` at run.rs:180. All TurnEvent fields are String/Option/usize/bool/TurnUsageSummary/RunTurnStatus -- all derive Serialize and are serializable. Type system guarantees preclude failure. No graceful fallback, but crash is unlikely and consistent | closed |
| T-18-13 | Denial of Service | empty TokenDelta state flip | mitigate | `apply_event_to_state` guards with `if !content.is_empty()` (run.rs:107) so empty TokenDelta does not mutate tokens_emitted_without_newline | closed |
| T-18-14 | Denial of Service | unbounded stream accumulation | accept | Channel capacity bounded at 256 (Phase 16). RunRenderState stores last-values (not accumulation); usage_total sums across ModelFinished but bounded by u64. No deadline-based termination in alpha | closed |
| T-18-15 | Denial of Service | broken pipe on stdout/stderr | mitigate | `render_human_event` returns `std::io::Result<()>`; IO errors propagate via `?` through `run_inner` to outer `run()` which maps to exit code 2. Broken pipe is a runtime failure, not a crash | closed |
| T-18-16 | Elevation of Privilege | release-gate.sh cargo commands | mitigate | `set -euo pipefail` (release-gate.sh:2); no sudo/root elevation; runs as current user; operates within workspace only | closed |
| T-18-17 | Elevation of Privilege | tool executor path containment bypass | mitigate | `DefaultToolExecutor::for_project_root` canonicalizes root (executor/mod.rs:105-108). `reject_parent_traversal` (path_guard.rs:15-33) and `ensure_under_project_root` (path_guard.rs:35-48) guard all file tool operations | closed |

*Status: closed*
*Disposition: mitigate (implementation required) -- accept (documented risk) -- transfer (third-party)*

---

## Accepted Risks Log

| Risk ID | Threat Ref | Rationale | Accepted By | Date |
|---------|------------|-----------|-------------|------|
| AR-18-01 | T-18-01 | In-memory channel spoofing requires process memory access, outside alpha threat model | gsd-security-auditor | 2026-05-22 |
| AR-18-02 | T-18-02 | JSONL output is local stdout; no network MITM surface | gsd-security-auditor | 2026-05-22 |
| AR-18-03 | T-18-05 | No toolchain signing in alpha; git version control provides integrity | 18-REVIEW (WR-02 accepted) | 2026-05-22 |
| AR-18-04 | T-18-07 | Audit logging deferred to post-alpha; session SQLite provides persistence | gsd-security-auditor | 2026-05-22 |
| AR-18-05 | T-18-10 | session_id/commit_id are internal identifiers, not secrets | gsd-security-auditor | 2026-05-22 |
| AR-18-06 | T-18-12 | Type system guarantees TurnEvent serialization; .expect() panic is extremely unlikely and consistent | gsd-security-auditor | 2026-05-22 |
| AR-18-07 | T-18-14 | No deadline-based stream termination in alpha; channel capacity provides backpressure | gsd-security-auditor | 2026-05-22 |
| AR-18-08 | WR-02 (review) | release-gate.sh omits lattice-session, lattice-tools, lattice-profile; accepted as alpha scope | 18-REVIEW WR-02 | 2026-05-22 |

---

## Code Review Findings Verification

| Finding | Status | Evidence |
|---------|--------|----------|
| WR-01: mid-token newline trick never resets flag | FIXED | `mid_token_stderr_newline_applied` flag added to RunRenderState (run.rs:81); `flush_before_status` checks flag (run.rs:297) and sets it (run.rs:300); reset on newline via apply_event_to_state (run.rs:110) |
| WR-02: release-gate.sh omits three workspace crates | NOT FIXED (accepted) | No lattice-session/lattice-tools/lattice-profile in release-gate.sh; accepted as alpha scope per AR-18-08 |
| WR-03: exit_code_from_state protocol violation guard | FIXED | Defensive `if state.last_terminal.is_some()` guard added before Completed check (run.rs:158); subsumes old Row 5/6 guard |
| IN-01: render_human_event takes &mut but does not mutate | RESOLVED | WR-01 fix requires &mut for mid_token_stderr_newline_applied reset in flush_before_status |
| IN-02: verbose epilogue {:?} Debug vs lowercase | FIXED | Match arm with lowercase strings added (run.rs:321-325: "completed", "error", "cancelled") |
| IN-03: status_to_exit_code dead code | NOT FIXED (accepted) | Preserved with #[allow(dead_code)] for legacy test compatibility; cosmetic |
| IN-04: handle_profile_error unused _source | NOT FIXED (accepted) | Cosmetic only; no security impact |

---

## Security Audit Trail

| Audit Date | Threats Total | Closed | Open | Run By |
|------------|---------------|--------|------|--------|
| 2026-05-22 | 17 | 17 | 0 | gsd-security-auditor |

---

## Sign-Off

- [x] All threats have a disposition (mitigate / accept / transfer)
- [x] Accepted risks documented in Accepted Risks Log
- [x] `threats_open: 0` confirmed
- [x] `status: verified` set in frontmatter

**Approval:** verified 2026-05-22