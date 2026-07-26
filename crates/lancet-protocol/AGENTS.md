# AGENTS.md

Crate-local guidance for `lancet-protocol`.

## Role

This crate is the pure CEP wire/data contract layer. It defines JSON-RPC
request/response shapes, CEP frame aliases, execution envelopes, capability
data, boundary reports, policy hook data, typed errors, and provenance wire
types.

## Sub-Architecture

- `src/jsonrpc.rs` owns domain-neutral JSON-RPC 2.0 envelope types.
- `src/frame.rs` gives CEP-specific aliases over JSON-RPC frames.
- `src/envelope.rs` owns intent, impact, scope, effects, guard verdicts, and
  `ExecutionEnvelope`.
- `src/cell.rs` owns the ADR 0004 Cell effect core (`CtxOp`/`ExtImpact`/`Trust`/
  `ScopeDecl`/`Cell`/`Capability`/`ToolDescriptor`) + trust-free CellRef layering.
- `src/capability.rs` owns required-capability patterns and resolution errors.
- `src/error.rs` owns `CepError`, families, wire codes, and retriability.
- `src/policy.rs` owns deterministic policy hook input/output data.
- `src/boundary.rs` owns host/isolation boundary report wire shapes.
- `src/provenance.rs` owns PROV and unsigned in-toto wire structs.
- Public re-exports in `src/lib.rs` are the crate contract; keep them stable.

## Boundaries

- No I/O, no tokio, no native execution, and no policy side effects.
- Serialized field changes are cross-crate changes, not local refactors.
- Keep serde and schemars behavior explicit for new or renamed fields.
- Do not encode concrete native handler, Component engine, or Tool Host
  implementation assumptions. This crate owns only shared wire/data vocabulary.
- ADR 0004 Cell vocabulary (`CtxOp`, `ExtImpact`, scope, trust, capability, and
  observed effects) starts here before authority, audit, runtime, or generated
  descriptor consumers change.

## Recent Issues

- FIXED M-24-01: `Capability::FetchUrl` uses the explicit `host` field; do not
  reintroduce wildcard/pattern naming unless matching semantics are implemented.
- FIXED P19: JSON-RPC helper, wire-code, and error-object mapping were made
  consistent after review of the old CEP core.
- FIXED P19: catalog sealing doctest coverage was corrected so compile-fail
  proves the intended trait boundary.
- WATCH: boundary/provenance wire additions must remain deterministic because
  audit attestations depend on stable serialized shapes.

## Next Actions

- Keep error-code mapping centralized; new error families should include
  retriability and JSON-RPC wire-code tests.
- If future fetch capabilities need wildcard hosts, update protocol naming,
  authority matching, native effect regions, schemas, and compatibility notes
  in the same series.
- When adding boundary evidence fields, update authority validation and native
  host/runtime projection together.
- ADR 0004 Cell core landed add-only in `src/cell.rs` (#27 (Codeberg #5)
  slice A); legacy `ImpactKind` in `effect_types.rs` coexists, retired by
  #28/#34 (Codeberg #6/#12) — do not extend.
- Component Tool execution error codes (`ToolTrap`/`ToolTimeout`/
  `ToolResourceExhausted`/`CallCancelled`) + `ToolExecutionErrorData` landed
  in `src/error.rs` Execution family (#27 slice C). No guest backtrace/host
  path/secret in the data.

## Record Maintenance

- Update `Recent Issues` and `Next Actions` in this file in the same patch that
  introduces, fixes, or defers a `lancet-protocol` issue.
- Mirror open/deferred/residual items in root `FINDINGS.md` with location,
  problem, status, and plan.
- Mirror landed fixes in root `FIX.md` with issue id, severity, fix summary,
  verification, and changed files when practical.
- Remove closed issues from this file once they are no longer useful local
  navigation; keep durable history in `FIX.md`.
- Keep this file near 80 lines. Prefer replacing stale issue bullets over
  accumulating a long changelog.

## Verification

- Local: `cargo test -p lancet-protocol`.
- Schema/wire changes: add serialization or schemars assertions where useful.
- Cross-crate protocol changes: also test affected catalog, authority, native,
  API, and CLI surfaces.
