<!-- refreshed: 2026-05-19 -->
# Codebase Concerns

**Analysis Date:** 2026-05-19 (post v1.1 收口审查)

This audit cross-references `FINDINGS.md` (open items deferred to v1.2) and `FIX.md`
(applied changelog) committed at HEAD. Items already shipped have been moved to
the resolved list and removed from the active section.

## Resolved Since 2026-05-17

Resolved during the v1.1 收口审查 series (commits `eaf073f`, `77fb6c6`, plus
the `llm-stream-sse-hardening`, `tool-safety-hotfix`, `agent-loop-hardening`,
`sqlite-consistency`, and `misc-fixes` branches):

- **H-01 SSE thread-local parser leak** — `crates/lattice-llm/src/client.rs` now
  constructs a fresh `ProviderParser` per stream invocation.
- **H-02 fake-streaming OOM** — `stream_sse_response` consumes
  `response.bytes_stream()` chunk by chunk with running size checks.
- **H-04 `truncate_output` panic on non-UTF-8 boundary** — `is_char_boundary`
  rollback in `crates/lattice-llm/src/util` (also referenced from tools).
- **H-05 bash without `current_dir`** — Unix and non-Unix branches both call
  `.current_dir(project_root)` in `crates/lattice-tools/src/executor/shell.rs`.
- **H-07 stringly-typed `tool_output_is_ok`** — `ToolExecutor` and
  `ToolCallExecutor` unified on `Result<String, String>`; prefix-based detection
  removed from `crates/lattice-agent`.
- **H-08 unbounded agent loop** — `MAX_TURN_ROUNDS = 200` hard cap added in
  `crates/lattice-agent/src/agent_loop.rs`.
- **H-09 SQLite per-call connection / TOCTOU** — `SqliteSessionGraphStore` now
  holds `Mutex<Option<Connection>>` with lazy init and WAL journaling
  (`crates/lattice-session/src/sqlite_graph.rs:35-95`).
- **M-01 OpenAI hardcoded `stream: true`** — request struct now controls
  streaming (`crates/lattice-llm/src/providers/openai.rs`).
- **M-02 Gemini ignored generation params** — `generationConfig` block emitted
  in `crates/lattice-llm/src/providers/gemini.rs`.
- **M-03 empty `base_url` accepted** — `validate_base_url` rejects empty input.
- **M-06 grep missing project-root canonicalize** — entry point canonicalizes
  and calls `ensure_under_project_root` in
  `crates/lattice-tools/src/executor/grep.rs`.
- **M-07 `read_text_file` TOCTOU on size** — post-read recheck of content size.
- **M-08 blocking `is_file()`/`is_dir()` in async** — replaced with
  `tokio::fs::metadata`.
- **M-09 SQLite without WAL** — `PRAGMA journal_mode = WAL; synchronous = NORMAL`
  enabled in `init_schema`.
- **M-10 silent JSON corruption swallowed** — `parse_json_column` helper
  surfaces `SessionError::Database` for malformed columns
  (`crates/lattice-session/src/sqlite_graph.rs:241-256`).
- **M-14 misleading `ForbiddenSection` message** — message now lists allowed
  sections only (`crates/lattice-profile/src/schema.rs`).
- **M-16 dead `TurnEvent` variants** — `Reasoning`, `ToolCallDelta`,
  `ToolCallEnd` removed from `lattice-api` event surface.
- **M-17 orphaned `topology.rs`** — file deleted.
- **M-18 `project_root` fallback to empty string** — fallback is `"/"`.
- **L-01 silent `u32` truncation** — `u32::try_from(...).unwrap_or(u32::MAX)` in
  `crates/lattice-llm/src/providers/mod.rs:54`.
- **L-03 unstable `ToolCallEnd` ordering** — `BTreeMap<u32, String>` keyed by
  index.
- **L-04 byte-slice `display_id`** — `char_indices().nth(12)` safe truncation.
- **L-05 missing config dir** — `HOME/.config` fallback then panic with explicit
  message (`crates/lattice-cli/src/commands/run.rs:122-128`).
- **CR-01..CR-05, WR-01..WR-07** — Phase 11 follow-ups (unknown roles, provider
  audit, env-var blocking, credential storage, lint blanket, `api_key` serde
  skip, `PathBuf::join`).
- **IN-01 duplicate test helpers** — extracted into
  `lattice_session::test_helpers`.
- **IN-02 `lattice-hooks` unused** — removed from workspace `members` list.
- **IN-03 stale CLI dep on hooks** — dependency dropped.
- **P2-01..P2-05** — model override persistence, NULL-tip checkout, detached
  HEAD branching, branch-move HEAD sync.

Earlier milestones already documented in the prior audit:

- Phase 6 removed: orchestrator, plugin, and plugins crates.
- Phase 11 removed: core, runtime, prompt, sandbox, and policy crates; legacy
  TUI; unsupported CLI commands; API migration `topology.rs`.
- Phase 11 collapsed: legacy `Agent` path into `AgentLoop`; Landlock duplication
  removed alongside the sandbox crate.

---

## Active Concerns

### High Severity

(None currently tracked in this codebase snapshot.)

### Resolved / Retired

**Production `stream_turn` buffered replay concern retired.**
- Previous tracking: FINDINGS.md `H-06`.
- Current state: production `stream_turn` uses a spawned live task and bounded
  channel; callers receive `TurnEvent` values before commit.
- Remaining boundary: `stream_turn_with_deps` is still buffered replay, but is
  a test seam. CLI live rendering remains Phase 18 work because the CLI still
  calls `run_turn`.

### Medium Severity

**DNS-rebinding TOCTOU in `lattice-llm` URL validation.**
- Location: `crates/lattice-llm/src/security.rs:100-117`
- Description: Validation resolves the host once and checks for private IPs.
  reqwest re-resolves at request time, allowing a rebinding attacker to flip
  the answer between checks.
- Impact: An adversary-controlled host could pass validation and then resolve
  to internal IP space, exposing metadata services.
- Fix approach: pin the validated IP via `reqwest::ClientBuilder::resolve()`,
  or perform the connection from within the validator and reuse the resulting
  socket.
- Tracking: FINDINGS.md `M-04`.

**DNS-rebinding TOCTOU + incomplete rebinding domain blacklist in
`lattice-tools` web fetch.**
- Location: `crates/lattice-tools/src/executor/web.rs:53-101`
- Description: Same TOCTOU as M-04. The rebinding domain blacklist only covers
  `nip.io` and `xip.io`; well-known equivalents (e.g., `sslip.io`,
  `traefik.me`, `lvh.me`, `localtest.me`, `vcap.me`) are not enforced.
- Impact: SSRF via tool-driven HTTP fetches.
- Fix approach: pin resolved IP in the same `reqwest::Client::get(url)` call
  that performed validation, and expand the blacklist to cover the documented
  set; consider ignoring it entirely in favour of strict private-IP rejection.
- Tracking: FINDINGS.md `M-05`.

**`head_transcript` only walks first-parent ancestry.**
- Location: `crates/lattice-session/src/sqlite_graph.rs:278-287`
- Description: The transcript replay loop calls
  `parents.into_iter().next()` and discards remaining parents. Merge commits
  drop the side branch entirely, so messages introduced by a merged branch are
  invisible to the agent on the merged HEAD.
- Impact: Conversational state silently lost after merges; the agent reasons
  on a partial history without any indicator of missing context.
- Fix approach: topologically sort reachable commits and include all ancestors,
  or persist a flattened transcript at merge time.
- Tracking: FINDINGS.md `M-11`.

**`hash_json` commit IDs depend on serde_json serialization order.**
- Location: `crates/lattice-session/src/graph.rs:329-336`
- Description: Commit IDs are `sha256("v1_turn_commit\0" || serde_json::to_string(payload))`.
  If the `preserve_order` feature flag toggles or `HashMap` ordering changes
  between builds, every existing commit ID becomes unreachable.
- Impact: A future cargo update or feature flip can invalidate every persisted
  session's history.
- Fix approach: introduce a canonical JSON encoder (sorted keys, fixed numeric
  formatting) before hashing.
- Tracking: FINDINGS.md `M-12`.

**Profile `canonicalize` fallback compares non-canonical paths.**
- Location: `crates/lattice-profile/src/load.rs:148-160`
- Description: When `package_dir.canonicalize()` fails, the original path is
  used while `resolved.canonicalize()` is required to succeed. The
  `starts_with` containment check then mixes a possibly non-canonical prefix
  with a canonical descendant, which can succeed for unintended targets.
- Impact: Profile system-prompt loading can read files outside the package
  directory under specific filesystem layouts (e.g., a relative `package_dir`
  that fails canonicalization but happens to be a string prefix of a
  canonicalized symlink target).
- Fix approach: fail closed on canonicalize errors; require both sides
  canonicalized before comparison.
- Tracking: FINDINGS.md `M-13`.

**`map_session_error` flattens structured errors to opaque strings.**
- Location: `crates/lattice-api/src/session.rs:131-136`
- Description: All `SessionError` variants except `NotFound` collapse to
  `ApiError::Internal(other.to_string())`. Database, IO, and conflict errors
  lose their structure on the public boundary.
- Impact: Callers cannot distinguish recoverable (e.g., conflict, busy) from
  unrecoverable failures; observability suffers.
- Fix approach: extend `ApiError` with `Database`, `Conflict`, and
  `Validation` variants and map each `SessionError` accordingly.
- Tracking: FINDINGS.md `M-15`.

### Low Severity

**`parse_json_data` uses `Err(LlmEvent::Started)` as a "skip" sentinel.**
- Location: `crates/lattice-llm/src/providers/mod.rs:14-22`
- Description: Empty SSE chunks and the `[DONE]` sentinel return
  `Err(LlmEvent::Started)` to signal "no event to emit." This conflates
  control flow with event types and makes the function impossible to read
  without context.
- Impact: Maintenance hazard; any future provider parser that takes
  `Err(LlmEvent::Started)` to mean an actual `Started` event will misbehave.
- Fix approach: change the return type to `Result<Option<serde_json::Value>, LlmEvent>`
  or introduce a dedicated `enum ParseOutcome`.
- Tracking: FINDINGS.md `L-02`.

**`FORBIDDEN_SECTIONS` retains migration-guard validation.**
- Location: `crates/lattice-profile/src/load.rs`,
  `crates/lattice-profile/src/schema.rs`,
  `crates/lattice-profile/src/defaults.rs`
- Description: Explicit rejection of removed TOML sections (e.g.,
  pre-Phase-11 `policy`, `sandbox`, `pipeline`, `topology` keys) is intentional
  but adds permanent surface area.
- Impact: Cosmetic; complicates schema reading.
- Fix approach: review at the v1.1 stabilization boundary; convert to
  documented compatibility notes if no longer needed.

**No CI enforcement of architecture boundary.**
- Location: Layering declared in `AGENTS.md` and `CLAUDE.md`
  (`hooks -> llm -> session/profile/tools -> agent -> api -> cli`).
- Description: The unidirectional dependency graph is enforced manually only.
  A new `Cargo.toml` edge could silently invert the direction.
- Impact: Architectural drift risk as the crate count grows.
- Fix approach: add a CI step that parses `cargo metadata --format-version 1`
  and asserts the declared topological ordering across workspace crates.

**Stale `lattice-hooks` crate retained in workspace.**
- Location: `crates/lattice-hooks/Cargo.toml`,
  `crates/lattice-hooks/src/lib.rs` (404 lines), root `Cargo.toml` workspace
  `[workspace.dependencies]` entry.
- Description: Removed from `members` (so it does not compile in normal
  workspace builds) but still listed under `[workspace.dependencies]` and
  physically present on disk. The crate has no current consumers.
- Impact: Repository clutter; dormant dead-on-disk code is easy to mistake for
  active.
- Fix approach: delete the directory and the workspace dependency entry, or
  restore membership and add a feature gate if a near-term consumer is
  planned. Coordinate with the deferred plugin/swarm work.

---

## Partial Mitigations (Tracked for v1.2)

**`write_file` symlink TOCTOU window.**
- Location: `crates/lattice-tools/src/executor/fs.rs:99-130`
- Already done: pre-write `symlink_metadata` rejects symlinks at `canonical`.
- Residual: between the metadata check and `tokio::fs::write`, an attacker
  with write access to the parent directory can replace the target with a
  symlink that escapes the project root.
- Complete fix: open the file with `O_NOFOLLOW` on Unix (via
  `tokio::fs::OpenOptions::custom_flags(libc::O_NOFOLLOW)`); fall back to
  `*at` family openers for the parent directory.
- Tracking: FINDINGS.md `H-03 (partial)`.

**`validate_agents_dir` does not resolve embedded symlinks.**
- Location: `crates/lattice-cli/src/commands/run.rs` (validation helper)
- Already done: canonicalize-fallback now performs manual `Component`
  normalization that rejects `..` traversal past the project root.
- Residual: manual normalization does not resolve existing symlinks inside
  the path. A symlinked subdirectory inside `agents/` can still hide outside
  content.
- Complete fix: anchor-walk algorithm that resolves each component via
  `symlink_metadata` and refuses to follow links pointing outside the
  validated root.
- Tracking: FINDINGS.md `H-10 (partial)`.

---

## Watch Items

**Crate dependency graph cleanliness.**
- Current graph (workspace `members`): `llm -> session/profile/tools -> agent -> api -> cli`.
  No cycles detected. The `hooks` crate is excluded.
- Watch for any future import that reverses the direction or reintroduces a
  removed crate.

**LOC trend.**
- Current strict source LOC under `crates/*/src/**/*.rs`: 18,136 (down from
  19,922 at the previous audit). Soft budget: 20,000.
- The `lattice-session/src/sqlite_graph.rs` file is 2,852 lines (mixed
  schema, store impl, and tests). Watch for organic growth; consider
  splitting tests into a sibling `tests` module file once the file exceeds
  3,000 lines.

**Test distribution.**
- `crates/lattice-hooks` is excluded from members; its 404-line test surface
  no longer runs in CI.
- `crates/lattice-api` (~2k lines) relies on `turn/tests.rs` for facade-level
  coverage; ensure new public methods get integration tests there.

**Hot files to monitor.**
- `crates/lattice-session/src/sqlite_graph.rs` (2,852 lines)
- `crates/lattice-cli/src/commands/session.rs` (1,004 lines)
- `crates/lattice-tools/src/edit/apply.rs` (933 lines)
- `crates/lattice-session/src/graph.rs` (813 lines)

---

## Security Audit Notes

**Tool safety in v1.1.**
- Project-root containment, path-traversal rejection, symlink rejection on
  `write_file` (with the residual TOCTOU window above), file-size limits, and
  profile-gated optional shell/web tools.
- Bash command construction (`crates/lattice-tools/src/executor/shell.rs`)
  invokes `pre_exec` with `unsafe` blocks that call `setrlimit` for `RLIMIT_AS`,
  `RLIMIT_NPROC`, and `RLIMIT_CPU`. Documented as async-signal-safe; review on
  any future addition.
- Metacharacter blocklist now covers `$` and `${` to prevent
  `${ANTHROPIC_API_KEY}` style env-var leakage in `check_command`.

**Credential handling.**
- `CredentialStore` warns on literal API keys without rejecting them.
- Debug formatting redacts key values; keys are zeroized on `Drop`.
- `ProviderConfig.api_key` carries `#[serde(skip)]` so literal keys cannot
  leak via serialization.
- CLI smoke tests verify no secret leakage in command output
  (`crates/lattice-cli/tests/release_smoke.rs`).

**SSRF surface.**
- Both URL validators (`lattice-llm` and `lattice-tools`) share the same
  TOCTOU weakness. Treat them as a single fix unit when introducing
  IP pinning.

---

## Deferred Per `CLAUDE.md` "Future Work"

These are intentional non-goals for the v1.1 release. Listing here so they do
not get re-flagged as bugs.

- **TUI** — A minimal terminal UI will be rebuilt on top of `lattice-api`
  after the streaming H-06 fix lands.
- **Rich prompt compilation** — Layer priorities, token budgets, and
  provider-specific assembly are not implemented; the current prompt path is
  intentionally flat.
- **Redesigned security model** — A new sandbox and approval system will
  follow the Landlock/sandbox crate removals; current safety relies on
  containment plus optional shell/web gating.
- **Plugins and orchestration** — Plugin contracts and DAG runners removed in
  Phase 6 are not present in this release. The dormant `lattice-hooks` crate
  is the most visible remnant.
- **Swarm and multi-agent collaboration** — Session-graph-based coordination
  features are deferred.

---

*Concerns audit: 2026-05-19*
