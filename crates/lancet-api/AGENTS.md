# AGENTS.md

Crate-local guidance for `lancet-api`.

## Role

This crate is the LANCET-native facade for runtime composition and operator
reads, independent of LATTICE session/agent/profile layers. During the current
migration it still exposes `LancetRuntime`/`LancetServer` and composes native
CEP crates; ADR 0004 moves concrete composition into `lancet-runtime`.
ADR 0005 supersedes the current direct-embedder product interpretation:
#139-B permits only `lattice-api` to initiate a production invocation. The
lower-level API remains nameable for internal runtime/transport tests, not as a
Standalone Tool-execution product contract.

## Sub-Architecture

- `src/runtime.rs` owns `LancetRuntime`, `LancetServer`, server lifecycle, and
  production composition.
- `src/config.rs` owns the reduced `LancetRuntimeConfig` over policy, capability
  admission, the strict project `InvocationStore`, and WorkspaceHistory backing.
- `src/audit.rs` is the thin delegating facade over `lancet_audit::operator`.
  It re-exports typed operator DTOs and maps `OperatorAuditError` to
  `LancetError` without changing messages.
- `src/status.rs` reads the current bounded finalized audit rows directly and
  exposes optional typed Cell evidence with each recent call.
- `src/rewind.rs` owns the public workspace-rewind API and durable rewind audit
  records.
- `src/error.rs` owns `LancetError`.
- `src/lib.rs` owns curated public re-exports.

## Boundaries

- No dependency on `lattice-agent`, `lattice-session`, `lattice-profile`, or
  `lattice-cli`.
- Inputs are LANCET-native types. Profile-to-policy translation belongs to the
  host (currently `lattice-api::turn::cep`).
- Native handler/transport implementation stays in `lancet-native` until its
  migration into `lancet-runtime`; do not add another production assembly path.
- Do not add a Standalone invocation context or advertise direct CEP execution
  as a supported product. Until #139-B the existing entry points are migration
  reality; the hard cut, not an additive compatibility wrapper, removes their
  product invocation role.
- Operator audit reads are direct-store APIs, never Agent Tools. Keep pagination,
  filtering, evidence materialization, and DTO behavior in `lancet-audit`; this
  facade stays thin.
- Do not expose raw audit `metadata_json`. An absent pointer is a valid
  `evidence: null` state even for a Component row. If a pointer exists, a
  missing target or unreadable evidence is an internal read error.
- GitHub #114 retired turn/audit provenance reconstruction and turn attestation
  APIs. Do not restore them here. Artifact identity and bundle provenance are
  separate contracts and remain supported where owned.

## Rewind audit + operation-lock ownership (#139-B Commit 3)

- `rewind_workspace` takes the caller-held `HeldWorkspaceOperation` guard and
  never acquires the project operation lock itself; a project backing without
  the held guard fails with `WorkspaceRewindError::MissingOperationLock`.
- `record_rewind_audit`/`rewind_audit_records` append to and read the
  independent Git chain owned by `lancet_runtime::rewind_audit` (re-exported
  here). The SQLite rewind store in `lancet-audit` was deleted in the same
  commit; there is no second Rewind adapter and no row import.

## Recent Issues

- ADDED #63/#64: runtime composition owns private workspace history and public
  rewind, while session-graph movement stays above this crate.
- FIXED #39: operator audit behavior and DTO ownership moved to `lancet-audit`;
  public `lancet_api` paths remain stable through a small error-mapping facade.
- LANDED #139-B Commit 6: query/tail/replay/status project the production Git
  InvocationStore through typed identities and pinned OID cursors; status applies
  its newest-20 bound to one pinned ledger view. SQLite audit/evidence adapters,
  attestation reconstruction, and row import paths are deleted.

## Verification

- Local: `cargo test -p lancet-api`.
- Runtime composition changes: also run affected native/API round trips.
- Operator-read changes: also run `cargo test -p lancet-audit` and the exact
  Direct-Local operator matrix against a built binary.
