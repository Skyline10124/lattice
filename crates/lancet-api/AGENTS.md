# AGENTS.md

Crate-local guidance for `lancet-api`.

## Role

This crate is the unified public API for driving LANCET's Controlled Execution
Protocol runtime, independent of any host application. It owns runtime
composition that previously lived inside `lattice-api`, so LANCET can be
embedded without pulling in the LATTICE session/agent/profile layers. During
migration it composes native CEP crates; the ADR 0004 target keeps this facade
but delegates concrete composition to `lancet-runtime`.

## Sub-Architecture

- `src/runtime.rs` — `LancetRuntime` (full production runtime: server lifecycle,
  `CepClient`, audit, tool defs), `LancetServer::spawn` (low-level registry +
  transport + server task launcher), `ServerLifecycle`, `base_observation_envelope`,
  and `cep_policy_from_policy_config`. Base envelopes are built through
  `lancet-cep::BaseAuthorityPlan` from `ToolExposure`'s catalog-declared
  requirements plus runtime policy inputs.
- `src/config.rs` — `LancetRuntimeConfig` and `AuditBacking`. Policy arrives as
  fully-resolved LANCET-native types; the host translates its own config. The
  external project-keyed store-path helpers (`audit_state_root`,
  `project_storage_key`, `audit_db_path_for_project`,
  `audit_db_path_for_project_in`, `ExternalAuditPathError`) are re-exported from
  `lancet_audit::store_path`, not defined here.
- `src/audit.rs` — thin delegating facade over `lancet_audit::operator`:
  re-exports the operator DTOs under their established `lancet_api` paths and
  maps `OperatorAuditError` onto `LancetError` (messages pass through verbatim).
  `query_audit` / `tail_audit` / `replay_audit` are one-line delegations; the
  behavior and its contract docs live in `lancet-audit`.
- `src/attestation.rs` — `store_turn_attestation` (unsigned in-toto from audit rows).
- `src/error.rs` — `LancetError` (Config / Internal / UnknownTools).
- `src/lib.rs` — curated re-exports of the types appearing in public signatures.

## Boundaries

- No dependency on `lattice-agent`, `lattice-session`, `lattice-profile`, or
  `lattice-cli`. Do not add one.
- Inputs are LANCET-native (`PolicyConfig`, `CommandIsolationRuntimeConfig`,
  `CepMode`). Translating an application profile into policy belongs to the host
  (today: `lattice-api::turn::cep`).
- Native handler/transport implementation currently stays in `lancet-native`;
  after #34 (Codeberg #12) this crate composes `lancet-runtime`. Do not add
  another production assembly path during the transition.
- Tool projection currently lives in `lancet-cep`; generated Source Tool
  descriptors replace it. Do not add compatibility wrappers or a parallel Tool
  schema here.
- Operator audit read remains a direct-store API, never an Agent Tool. GitHub
  #39 landed the move of its behavior/DTO ownership to `lancet-audit`; this facade
  stays stable and stays thin. Pagination, filter parsing, wire projection, and
  attestation comparison must not come back into this crate.

## Recent Issues

- ADDED #28 (Codeberg #6): `LancetRuntimeConfig.capability_admission` threads
  the host's Component Tool admission policy into `CepPolicyConfig`;
  `spawn_composed` validates it against the composed catalog, so an unknown deny
  name fails composition instead of a spawned server task.
  `store_turn_attestation` writes `statement_bytes` (canonical for v2, stored
  serde order for v1) and `replay_audit` — now delegating into
  `lancet_audit::operator` — rebuilds under the stored `predicate_type`, so a
  turn attested as v1 never re-projects as v2.
- ADDED #64: `rewind_workspace` composes the private history's plan/ref
  movement with the `lancet-native` restore executor under one recording
  lock, and `record_rewind_audit` / `rewind_audit_records` persist the
  durable Rewind record into the project's `cep_audit.db`. Both follow the
  `query_audit` operator shape — direct store access, never an Agent Tool.
  The Session History half stays above this crate: lancet-api must not see
  `lattice-session`.
- ADDED #63: `LancetRuntimeConfig.workspace_history` selects the private
  mutation-history backing. `LancetRuntime::build` opens it before the server
  starts, threads it through `ServerRunConfig`, and exposes it via
  `workspace_history()` / `LancetRuntimeParts` so a host can read the object
  id a Turn arrived at. `project_state_dir{,_in}` is the shared per-project
  private state directory; `workspace_history_backing{,_in}` resolves
  `<state>/lattice/projects/<key>/workspace-history.git` next to the audit
  database. `WorkspaceHistoryBacking::Disabled` is the default so an embedder
  never gets durable history by accident. The per-project private state
  directory now lives in `lancet_audit::store_path` (moved there by #39) and is
  re-exported here, so the audit DB and the history repository resolve from one
  definition rather than two.
- FIXED GitHub #39 (= Codeberg #17): operator audit read sank into `lancet-audit`.
  `src/audit.rs` is now a re-export + three one-line delegations; the
  pagination loop, filter construction, wire projection, attestation
  comparison, and their contract tests live in `lancet_audit::operator` /
  `operator_tests`, and the store-path helpers in `lancet_audit::store_path`.
  Every `lancet_api::*` public path is unchanged and error strings are
  byte-identical. `lancet-catalog/src/methods/audit.rs` (six operator DTOs) was
  deleted, not migrated to Tool descriptors. The now-unused `dirs` / `sha2`
  deps were dropped from this crate. Because `lancet-audit` depends on none of
  `lancet-native` / `lancet-dispatch` / `lancet-cep` / `lancet-runtime`, "the
  operator read never composes a CEP server" is now a structural
  crate-dependency guarantee rather than a source-string-scan assertion.
- ADDED #27 (Codeberg #5) slice D: `LancetRuntime::build` composes
  `ComponentRuntime::from_embedded()` once and threads the same
  `Arc<ComponentRuntime>` into `ServerRunConfig.components` (D.3 dispatch) and
  into the LLM tool definitions projection (D.4).
  `LancetServer::spawn` composes from embedded inventory and delegates to
  `pub(crate) spawn_composed`; there is no public path to inject Component
  bytes, paths, or handlers. Production builds have zero Component Tools
  until #29–#32 (Codeberg #7–#10).
- FIXED #60 (Codeberg #38): `ServerRunConfig.execution` and the
  `ExecutionRouting`/`UpperContext`/`NullExecutionBackend` re-exports are gone.
  The native server constructs the one concrete Local Workspace from
  `cep_context.policy.project_root` inside `run_native`; embedders supply
  only the canonical project root (via resolved policy). `LancetServer::spawn`
  and direct test servers must use `CepServerNativeExt::run_native`.
- FIXED M-24-05: turn/LANCET runtime composition extracted out of `lattice-api`
  into this crate; turn runtime and CLI `lancet` smoke route through
  `LancetServer`/`LancetRuntime`. CLI `lattice audit` uses `query_audit` /
  `tail_audit` against the project audit store (no CEP server).
- FIXED issue #33 (Codeberg #11): `observe.audit` / `observe.audit_replay`
  removed from the agent catalog; `src/audit.rs` is the only operator entry
  point here (its implementation moved to `lancet-audit` in #39).
- DOWNGRADED L-11-01/L-11-02 (ACCEPTED RESIDUAL, neither optimally handled):
  `tail_audit` yields between batches via `tokio::task::yield_now().await` so
  drop-cancellation takes effect at batch boundaries, but there is no dedicated
  `CancellationToken` plumbed from `lattice audit --tail` (worst-case bounded by
  `AUDIT_TAIL_MAX_BATCHES * AUDIT_TAIL_MAX_ROWS`); the active-turn concurrent-read
  invariant is covered by a **synthetic-writer** regression test
  (`operator_audit_reads_concurrent_with_active_writer`), not a live-turn
  integration test — accepted because the live turn writes through the same
  `CepAuditStore` API, so the store concurrency boundary is identical. Since
  #39 both contracts are documented in `lancet-audit/src/operator.rs` (not
  `src/audit.rs`); escalation conditions in root `FINDINGS.md`.
- FIXED M-24-04: `ToolExposure` and `BaseAuthorityPlan` now live behind the
  `lancet-cep` facade. `MethodDescriptor::base_authority` supplies method-local
  requirements, this crate no longer maps method names directly to base envelope
  grants, and projection wrapper shims were removed.
- FIXED L-llm-coupling: shared URL/DNS safety primitives moved to the neutral
  `net-guard` leaf; the LANCET dependency graph has no LATTICE runtime crates.

## Next Actions

- If a profile/config translation needs a new policy field, extend
  `LancetRuntimeConfig` with a LANCET-native type — never accept `LancetSection`.
- When adding a public entry point, re-export only the types in its signature
  from `lib.rs`; do not add pass-through functions for lower-crate APIs.
- Keep the public facade stable while #34 replaces hand-wired native/CEP/dispatch
  composition with `lancet-runtime`; do not preserve both paths afterward.
- Keep durable per-project state under `project_state_dir`; a new store goes
  beside the audit DB and the workspace history, never in the project root.

## Record Maintenance

- Update `Recent Issues`/`Next Actions` when the runtime API surface or
  composition changes.
- Record landed fixes in root `FIX.md` with verification commands; keep residual
  coupling risks in root `FINDINGS.md`.
