# AGENTS.md

Crate-local guidance for `lancet-audit`.

## Role

This crate owns the production project-keyed Git `InvocationStore` and typed
operator query, tail, replay, and status projections. The append-only invocation
ledger is the sole production authority for reservation, dispatch lifecycle,
terminal outcome/material, and Cell evidence.

## Sub-Architecture

- `src/invocation_store.rs` owns the topology-validated `invocations.git` ledger,
  ordered batch reservation, exact admission, lifecycle/terminal publication,
  pinned views, high-water witness, and authenticated derived checkpoint.
- `src/operator.rs` projects one pinned ledger view into typed operator DTOs.
  Tail cursors are exclusive commit OIDs; replay uses typed SessionId/TurnId and
  structural binding order.
- `src/test_support.rs` (feature `test-support`) owns the workspace-wide
  `ScopedXdgStateHome` test guard.
- `tests/invocation_qualification.rs` is the non-production cross-domain seam
  that projects a pinned view into `lattice_session::pending` facts through a
  dev-dependency. Production source must never name session types.

## Boundaries

- Keep this crate free of `lancet-native`, `lancet-dispatch`, `lancet-cep`,
  `lancet-runtime`, and all LATTICE session/agent/profile production dependencies.
  Operator reads must remain usable without composing a CEP server.
- Do not make live policy, authorization, effect, or workspace decisions here.
- `InvocationId` is the only public invocation identity. Do not restore numeric
  call/audit/evidence IDs, SQLite stores, dual writes, importers, or compatibility
  adapters.
- Operator DTOs expose typed identities, lifecycle, digests, lengths, and Cell
  evidence; never argument/result secret material or Tool schemas.
- `lattice-store` supplies generic Git, locking, CAS, and durability mechanics.
  Invocation lifecycle and operator semantics remain owned here.
- The data ledger ref is authoritative. The ordered witness and derived checkpoint
  are repairable accelerators and never second authorities.

## Qualification

The performance campaign remains diagnostic-only under #153. The official runner
writes ignored bundles under `target/invocation-store-bundles`; historical bundles
and verdicts are immutable evidence. `summarize_qualification.py benchmark` is
non-blocking; strict `qualify` remains available for the redesign.

## Verification

- Local: `cargo test -p lancet-audit`.
- Boundary: `cargo xtask lancet-boundary`.
- Operator changes: also run `cargo test -p lancet-api`, Direct-Local
  `direct_operator`, and feature-gated `release_operator`.
