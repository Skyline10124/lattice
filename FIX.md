# FIX — Changelog

Applied fixes across code review phases. Entries are append-only; never delete or overwrite.

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
|---|----------|---------|-----|
| CR-01 | CRITICAL | `commit_prepared_turn_impl` performed a non-atomic CAS check (read HEAD OID) followed by a force-update ref write (`repo.reference(..., force=true)`). A concurrent process could move the ref between check and write, and the force update would silently overwrite the other writer's commit. | Replaced the force update with an atomic compare-and-set: `find_reference` → re-verify `target() == head_oid` → `set_target` (fails if ref moved). First-commit path uses `reference(..., force=false)` and errors if the ref unexpectedly exists. Returns `SessionError::Conflict` on any movement. |
| WR-01 | WARNING | `messages_to_jsonl` used `unwrap_or_else(|_| String::new())`, silently emitting empty lines on serialization failure; `parse_messages_jsonl` would then fail on readback, losing the original message. | Changed signature to `Result<String>` and propagate `serde_json::to_string` errors as `SessionError::Git`. Updated the single caller in `commit_prepared_turn_impl`. |
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
|---|----------|---------|-----|
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
|---|----------|---------|-----|
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
|---|----------|---------|-----|
| P29-VG-01 | CRITICAL | Production `exec.command_run` still did not execute the child through the Linux supervisor after the evidence-honesty fix. | `handle_run_as` now calls `execute_argv_supervised` for isolated command plans. The child runs live supervisor setup in pre-exec, reports the setup attempt through a close-on-exec pipe, and parent-side metadata is built from that real report. |
| P29-VG-02 | CRITICAL | Live mount-view and inherited-authority cleanup stages had no production implementation, so filesystem/fd requirements could not be satisfied. | `LiveSupervisorSyscalls` now builds a per-child tmpfs root, bind-mounts `/project`, declared read-only toolchain paths, scratch tmpfs, and minimal devices, pivots away from the old root, verifies old-root cleanup, and runs fd cleanup while preserving only stdio plus the setup-report pipe. |
| P29-VG-03 | HIGH | Audit fallback and setup failures could not distinguish setup failure from backend command execution. | Supervised execution now returns setup attempts on success and failure. Setup failures record `backend_invoked=false` for enforce, while audit fallback records the failed setup attempt and then runs the unbounded backend with honest `unbounded` / `not_applied` boundary metadata. |

Files: `crates/lancet-native/src/handlers/command.rs`, `crates/lancet-native/src/handlers/command_tests.rs`, `crates/lancet-native/src/executor/shell.rs`, `crates/lancet-native/src/executor/supervisor/mod.rs`, `crates/lancet-native/src/executor/supervisor/syscall.rs`, `crates/lancet-native/src/executor/supervisor/live_tests.rs`, `docs/LANCET.md`

---

## Phase 29 review fixes: isolation supervisor evidence honesty — 2026-06-04

Verified: `cargo test -p lancet-native command_boundary` ✓, `cargo test -p lancet-native isolation_supervisor` ✓, `cargo test -p lancet-native shell` ✓, `cargo test -p lancet-native landlock_live_canary_requires_applied_status_to_pass` ✓, `cargo test -p lancet-native live_supervisor_unimplemented_evidence_stages_fail_closed` ✓, `cargo test -p lancet-authority boundary_credit` ✓, `cargo test -p lancet-native env` ✓, `cargo test -p lancet-native fd` ✓, `cargo test -p lancet-native mount` ✓, `cargo test -p lancet-native network` ✓, `cargo test -p lancet-native landlock` ✓, `cargo test -p lattice-cli lancet` ✓, `cargo test -p lattice-api cep_mode_integration` ✓, `cargo test -p lancet-audit effective_boundary` ✓, `cargo test -p lancet-catalog command` ✓, `cargo test -p lancet-native projection` ✓, `cargo fmt --all --check` ✓, `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✓, `cargo test --workspace` ✓, `LATTICE_LINUX_ISOLATION_LIVE=1 cargo test -p lancet-native -- --ignored --nocapture` ✓ (`passed=2 skipped_unsupported=3 failed=0`)

| # | Severity | Problem | Fix |
|---|----------|---------|-----|
| P29-CR-01 | CRITICAL | Production `exec.command_run` used the `Ready` supervised handoff even though the hook only wrote `ready\n` and applied no Linux setup. | Production no longer calls the fake-ready handoff. Pending child-side setup is reported as `native_linux_pre_exec_pending`; required `enforce` boundaries fail closed before backend execution, while `audit` fallback runs with `unbounded` / `not_applied` metadata. Docs and phase summaries now avoid claiming applied production isolation without a real setup report. |
| P29-CR-02 | CRITICAL | `LiveSupervisorSyscalls` returned `Ok(())` for evidence-bearing no-op stages, allowing future wiring to mint applied boundary evidence without doing the work. | Unimplemented live stages now return errors, and the netns topology assertion performs a real no-veth/no-default-route check. Added regression coverage so those stages cannot silently report success before implementation. |
| P29-WR-01 | WARNING | The Landlock live canary passed when the helper printed `landlock_status=not_applied` and exited successfully. | The canary now passes only on `landlock_status=applied`; `not_applied` is classified as failure unless the case is explicitly unsupported. |

Files: `crates/lancet-native/src/handlers/command.rs`, `crates/lancet-native/src/handlers/command_tests.rs`, `crates/lancet-native/src/executor/shell.rs`, `crates/lancet-native/src/executor/supervisor/syscall.rs`, `crates/lancet-native/src/executor/supervisor/live_tests.rs`, `docs/LANCET.md`, `.planning/phases/29-isolation-supervisor/29-05-SUMMARY.md`, `.planning/phases/29-isolation-supervisor/29-06-SUMMARY.md`

---

## 技术债扫描修复 — 2026-05-27

Verified: `cargo fmt --all --check` ✓, `cargo clippy --workspace --all-targets -- -D warnings` ✓, `cargo test --workspace` ✓ (731 tests), `cargo test -p cep-executor` ✓ (121 tests; default sandbox blocked localhost bind, unrestricted local test passed), `cargo test -p lattice-hooks` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
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
|---|--------|------|------|
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
|---|------|------|------|
| M-12 | `hash_json` 依赖 serde_json 序列化顺序 | 引入 `canonical_json` 递归排序 object keys | `lattice-session/src/graph.rs` |
| M-13 | profile `canonicalize` 失败时回退非规范路径 | 改为返回 `ProfileError::SystemPathNotFound` | `lattice-profile/src/load.rs` |
| M-15 | `map_session_error` 丢弃结构化错误信息 | 添加 Conflict/BadRequest/MissingCommit/Io/Database 映射 | `lattice-api/src/session.rs` |
| M-18-01 | release-gate.sh 缺 workspace crate | 添加 lattice-session/tools/profile/hooks | `scripts/release-gate.sh` |
| M-18-02 | `status_to_exit_code` 死代码 | 删除函数及 3 个遗留测试 | `lattice-cli/src/commands/run.rs` |

### LOW

| # | 问题 | 修复 | 文件 |
|---|------|------|------|
| L-02 | `parse_json_data` 用 sentinel 做控制流 | 改为 `Result<Option<Value>, LlmEvent>`，语义清晰 | `lattice-llm/src/providers/mod.rs` + 调用方 |
| L-18-02 | TurnEvent 序列化 `.expect()` panic | 改为 `map_err` + `?` | `lattice-cli/src/commands/run.rs` |
| L-18-03 | 无 stream 超时机制 | CLI 加 5 分钟 idle timeout，超时写 stderr + exit 1；API/Agent durable deadline 留未来 | `lattice-cli/src/commands/run.rs` |

---

## v1.1 收口审查 — 2026-05-19

Verified: `cargo fmt --all --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` 427 pass ✓

### HIGH

| # | 问题 | 修复 | 分支 |
|---|------|------|------|
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
|---|------|------|------|
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
|---|------|------|------|
| L-01 | `u32_field` 静默截断 | `u32::try_from(...).unwrap_or(u32::MAX)` | llm-stream-sse-hardening |
| L-03 | `ToolCallEnd` 顺序不定 | `BTreeMap<u32, String>` 按 index 排序 | llm-stream-sse-hardening |
| L-04 | `display_id` byte 切片 | `char_indices().nth(12)` 安全截断 | misc-fixes |
| L-05 | `config_dir()` 回退相对路径 | `HOME/.config` fallback + panic on missing | misc-fixes |

### 重构

| 改动 | 内容 | 分支 |
|------|------|------|
| trait 统一 | `lattice_tools::ToolExecutor` = `lattice_agent::ToolCallExecutor`，签名 `Result<String, String>`，12 个工具函数返回 `Err` | agent-loop-hardening |
| 工具面简化 | 删除 `read_files`（并行 `read_file` 替代），`web_search` → `web_fetch` | tool-safety-hotfix |
| 测试去重 | `test_helpers` module 提取共享 `user_msg`/`assistant_msg`/`tool_msg` | misc-fixes |
| hooks 移除 | `lattice-hooks` 从 workspace members 移除（文件保留，不编译） | misc-fixes |

### Phase 11 遗留项

| # | 问题 | 修复 |
|---|------|------|
| WR-02 | env var 测试同步 | Router tests 用 local env mutex |
| WR-03 | SQLite per-method connection | 合并到 H-09 |
| WR-04 | tool_output_is_ok 前缀 | 合并到 H-07 |
| WR-06 | turn parent_commit_id TOCTOU | 初始 transcript 读取时 capture parent |
| IN-01 | 重复测试 helper | 提取到 `lattice_session::test_helpers` |
| IN-02 | lattice-hooks 未使用 | 从 workspace members 移除 |
| IN-03 | CLI 依赖 hooks 未 import | 删除依赖 |

### PR #6 遗留项

| # | 问题 | 修复 |
|---|------|------|
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
|---|------|------|
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
|---|------|------|
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
|---|------|------|
| Step 1 | Production `run_turn` used buffered `TurnDeps` path parallel to live `stream_turn` | `run_turn` now drains live `stream_turn` via `RunTurnCollector`; round-aware `assistant_output`, usage sum, synthetic stages |
| Step 2 | `turn/tests.rs` depended on `FakeTurnDeps` buffered seam | Migrated to public `run_turn` / `stream_turn` with `SessionApi` + `FakeStreamingLlmClient`; added optional `RunTurnRequest.cancel_flag` for cooperative-cancel E2E |
| Step 3 | Legacy `TurnDeps` / `execute_turn_inner` / `run_turn_with_deps` / `stream_turn_with_deps` duplicated orchestration | Deleted `deps.rs`, buffered execute path, and `map_tool_results`; single core is `stream_turn_live` |
| Step 4 | Consistency tests compared two full execution paths with dual tempdirs | Rewrote to assert `run_turn` == `collect_turn_result_from_events(drain stream_turn)` plus durable session-store anchor |

---

## Phase 28 review fixes: capability probe and boundary contract — 2026-06-03

Verified: `cargo fmt --all --check` ✓, `git diff --check` ✓, `cargo build` ✓, `cargo test` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test -p lancet-native` ✓, `cargo test -p lattice-cli` ✓, `cargo test -p lattice-cli --bin lattice lancet -- --nocapture` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
| P28-CR-01 | CRITICAL | Verify-failed `exec.command_run` workspace mutations could leave durable metadata at `backend_invoked=false` after the backend had already run | Command execution metadata is now recorded and emitted immediately after `execute_argv` succeeds, before diff/verify return paths; added a verify-failure regression asserting durable metadata and events report `backend_invoked=true` |
| P28-WR-01 | WARNING | Native and CLI host-probe output used a separate Vec-shaped report instead of the protocol primitive-keyed `HostProbeReport` | Native now reuses `lancet_protocol::HostProbeReport`/`HostProbeEntry`; CLI JSON exposes the canonical `host_probe.primitives.<primitive>` shape and serializes gate-missing primitive reasons through the protocol type |
| P28-WR-02 | WARNING | Live host capability probes resolved `unshare`, `sh`, and child helpers through ambient `PATH` | Probes now use trusted absolute helper candidates, sanitize child `PATH`, and record helper paths in diagnostics; missing trusted helpers are reported as unknown instead of falling back to ambient `PATH` |

## Phase 34: CJK double-width character display fixes — 2026-06-07

User reported TUI output label calculation issues with Chinese double-width characters. Root cause: the entire codebase used `.len()` (byte count) or `.chars().count()` (Unicode scalar count) for display-relevant string lengths, with zero awareness of terminal column width (CJK = 2 columns, ASCII = 1 column).

Severity: LOW — display-only, no behavioral impact.

**Changes:**

| File | Change |
|------|--------|
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
