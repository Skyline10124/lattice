# AGENTS.md

Crate-local guidance for `lancet-api`.

## Role

This crate is the unified public API for driving LANCET's Controlled Execution
Protocol runtime, independent of any host application. It owns runtime
composition that previously lived inside `lattice-api`, so LANCET can be
embedded without pulling in the LATTICE session/agent/profile layers.

## Sub-Architecture

- `src/runtime.rs` — `LancetRuntime` (full production runtime: server lifecycle,
  `CepClient`, audit, tool defs), `LancetServer::spawn` (low-level registry +
  transport + server task launcher), `ServerLifecycle`, `base_observation_envelope`,
  and `cep_policy_from_policy_config`. Base envelopes are built through
  `lancet-cep::BaseAuthorityPlan` from `ToolExposure`'s catalog-declared
  requirements plus runtime policy inputs.
- `src/config.rs` — `LancetRuntimeConfig` and `AuditBacking`. Policy arrives as
  fully-resolved LANCET-native types; the host translates its own config.
- `src/audit.rs` — `query_audit`, `tail_audit`, `replay_audit` (direct `CepAuditStore` reads; no CEP methods).
- `src/attestation.rs` — `store_turn_attestation` (unsigned in-toto from audit rows).
- `src/error.rs` — `LancetError` (Config / Internal / UnknownTools).
- `src/lib.rs` — curated re-exports of the types appearing in public signatures.

## Boundaries

- No dependency on `lattice-agent`, `lattice-session`, `lattice-profile`, or
  `lattice-cli`. Do not add one.
- Inputs are LANCET-native (`PolicyConfig`, `CommandIsolationRuntimeConfig`,
  `CepMode`). Translating an application profile into policy belongs to the host
  (today: `lattice-api::turn::cep`).
- Native handler/transport implementation stays in `lancet-native`; this crate
  only composes it.
- Tool projection and enabled-method exposure semantics live in `lancet-cep`;
  this crate consumes them internally and should not add compatibility wrappers.

## Recent Issues

- FIXED M-24-05: turn/LANCET runtime composition extracted out of `lattice-api`
  into this crate; turn runtime and CLI `lancet` smoke route through
  `LancetServer`/`LancetRuntime`. CLI `lattice audit` uses `query_audit` /
  `tail_audit` against the project audit store (no CEP server).
- FIXED issue #11: `observe.audit` / `observe.audit_replay` removed from the
  agent catalog; operator reads live in `src/audit.rs` only.
- DOWNGRADED L-11-01/L-11-02 (ACCEPTED RESIDUAL, neither optimally handled):
  `tail_audit` yields between batches via `tokio::task::yield_now().await` so
  drop-cancellation takes effect at batch boundaries, but there is no dedicated
  `CancellationToken` plumbed from `lattice audit --tail` (worst-case bounded by
  `AUDIT_TAIL_MAX_BATCHES * AUDIT_TAIL_MAX_ROWS`); the active-turn concurrent-read
  invariant is covered by a **synthetic-writer** regression test
  (`operator_audit_reads_concurrent_with_active_writer`), not a live-turn
  integration test — accepted because the live turn writes through the same
  `CepAuditStore` API, so the store concurrency boundary is identical. Both
  contracts are documented in `src/audit.rs`; escalation conditions in root
  `FINDINGS.md`.
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

## Record Maintenance

- Update `Recent Issues`/`Next Actions` when the runtime API surface or
  composition changes.
- Record landed fixes in root `FIX.md` with verification commands; keep residual
  coupling risks in root `FINDINGS.md`.
