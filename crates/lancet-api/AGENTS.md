# AGENTS.md

Crate-local guidance for `lancet-api`.

## Role

This crate is the LANCET-native facade for runtime composition and operator
reads, independent of LATTICE session/agent/profile layers. It exposes
`LancetRuntime`/`LancetServer` for the lower-level runtime and transport boundary;
only `lattice-api` may initiate a production invocation. Lower-level entry points
remain nameable for internal reservation-gated tests, not as a Standalone
Tool-execution product contract. ADR 0004 moves the remaining concrete native
composition into `lancet-runtime`.

## Sub-Architecture

- `src/runtime.rs` owns `LancetRuntime`, `LancetServer`, server lifecycle, and
  production composition.
- `src/config.rs` owns `LancetRuntimeConfig` over policy, capability admission,
  the strict project `InvocationStore`, and WorkspaceHistory backing.
- `src/audit.rs` is the thin delegating facade over `lancet_audit::operator`.
  It re-exports typed operator DTOs and maps `OperatorAuditError` to
  `LancetError` without changing messages.
- `src/status.rs` projects the newest 20 current records from one pinned Git
  ledger view and exposes the exact `ledger_oid` used by that read.
- `src/rewind.rs` owns the public workspace-Rewind API and delegates durable
  append-only audit records to `lancet_runtime::rewind_audit`.
- `src/error.rs` owns `LancetError`.
- `src/lib.rs` owns curated public re-exports.

## Boundaries

- No dependency on `lattice-agent`, `lattice-session`, `lattice-profile`, or
  `lattice-cli`.
- Inputs are LANCET-native types. Profile-to-policy translation belongs to the
  host (`lattice-api::turn::cep`).
- Native handler/transport implementation stays in `lancet-native` until its
  ADR 0004 migration into `lancet-runtime`; do not add another production
  assembly path.
- Do not add a Standalone invocation context or advertise direct CEP execution
  as a product. Production calls originate in `lattice-api`, carry an exact
  durable `InvocationBinding`, and pass reservation-gated admission.
- Operator reads are direct `InvocationStore` APIs, never Agent Tools. Keep
  pinned-view pagination, filtering, Cell-evidence projection, and DTO behavior
  in `lancet-audit`; this facade stays thin.
- Operator DTOs expose typed identity, lifecycle, digests, lengths, and
  content-free Cell evidence from terminal records. Arguments, results, provider
  payloads, internal metadata, and compatibility pointers are not operator wire
  fields.
- GitHub #114 retired turn/audit provenance reconstruction and turn attestation
  APIs. Do not restore them here. Artifact identity and bundle provenance are
  separate contracts and remain supported where owned.

## Rewind audit + operation-lock ownership (#139-B Commit 3)

- `plan_workspace_rewind` and `rewind_workspace` take an opaque, acquire-produced
  `HeldWorkspaceOperation`; both validate its exact project/store/lock binding
  before history access, and apply also validates the canonical project root.
  Neither lower-level operation reacquires the project lock.
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
