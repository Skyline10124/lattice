# AGENTS.md

Crate-local guidance for `lancet-protocol`.

## Role

This crate is the pure CEP wire/data contract layer. It defines JSON-RPC
request/response shapes, CEP frame aliases, execution envelopes, capability
data, boundary reports, policy hook data, typed errors, and versioned Cell
evidence types. It has no I/O and no Tokio dependency.

## Sub-Architecture

- `src/jsonrpc.rs` owns domain-neutral JSON-RPC 2.0 envelope types.
- `src/frame.rs` gives CEP-specific aliases over JSON-RPC frames.
- `src/envelope.rs` owns intent, impact, scope, effects, guard verdicts, and
  `ExecutionEnvelope`.
- `src/cell.rs` owns the ADR 0004 Cell effect core
  (`CtxOp`/`ExtImpact`/`Trust`/`ScopeDecl`/`Cell`/`Capability`/
  `ToolDescriptor`) plus trust-free CellRef layering.
- `src/evidence.rs` owns the versioned admission/effect evidence vocabulary:
  `AdmissionEvidence`, `ObservedEffectsRecord`, `DeniedOperationsRecord`,
  `ObservedScope`, `ObservedTarget`, and `CapabilityAdmissionPolicy`.
- `src/canonical.rs` owns `canonical_json`, the deterministic writer used for
  context measures and persisted evidence payload construction.
- `src/capability.rs` owns required-capability patterns and resolution errors.
- `src/error.rs` owns `CepError`, families, wire codes, and retriability.
- `src/policy.rs` owns deterministic policy hook input/output data.
- `src/boundary.rs` owns host/isolation boundary report wire shapes.
- Public re-exports in `src/lib.rs` are the crate contract; keep them stable.

## Boundaries

- No I/O, Tokio, native execution, or policy side effects.
- Serialized field changes are cross-crate changes, not local refactors.
- Keep serde and schemars behavior explicit for new or renamed fields.
- Do not encode concrete native handler, Component engine, or Tool Host
  implementation assumptions. This crate owns only shared wire/data vocabulary.
- ADR 0004 Cell vocabulary starts here before authority, audit, runtime, or
  generated-descriptor consumers change.
- Turn/audit provenance and in-toto attestation wire types were retired by
  GitHub #114. Preserve artifact identity and bundle provenance contracts where
  they are owned; do not recreate a second operator projection vocabulary here.
- ADR 0005 assigns the shared `InvocationId` and `TurnId` data types here.
  GitHub #142 adds strict lowercase hyphenated UUID-v4
  parse/display/serde/schema and tests; this crate validates but does not
  generate IDs. Until #139-B, do not thread InvocationId into production
  JSON-RPC, transport, events, evidence, or operator DTOs, and never add a
  second String/newtype identity.

## Recent Issues

- ADDED #30: `ObservedEffect.truncated: Option<bool>` gives three-state bounded
  observation evidence without rewriting older records.
- ADDED #30: `ObservedEffect.visibility: Option<WorkspaceVisibilityEvidence>`
  persists content-free Policy-Visible Workspace decisions.
- ADDED #31: versioned mutation evidence records pre/post revision,
  preconditions, and installation facts.
- ADDED #112: the three Cell-evidence record roots are versioned persisted
  shapes; readers check the version before interpreting contents.
- FIXED #114: removed the retired `provenance.rs` contract. Operator audit
  reads now expose only the existing versioned evidence records through typed
  DTOs owned by `lancet-audit`.

## Next Actions

- Keep error-code mapping centralized; new error families need retriability and
  JSON-RPC wire-code tests.
- When adding evidence fields, update authority validation, durable storage,
  runtime production, and operator DTO tests in the same series.
- ADR 0005/#139-B replaces the numeric JSON-RPC request `id` with the canonical
  string form of an already-reserved InvocationId; it does not reintroduce the
  deleted `session.envelope` Tool. Reservation creates the invocation and the
  request dispatches it. Notification params may reference an existing
  InvocationId for control, but cannot reserve or dispatch an effectful Tool.
- Do not extend the legacy `ImpactKind` vocabulary. Its retirement still needs
  the four preconditions in root `AGENTS.md`.

## Verification

- Local: `cargo test -p lancet-protocol`.
- Schema/wire changes: add serialization or schemars assertions where useful.
- Cross-crate protocol changes: also test affected authority, audit, native,
  API, and CLI surfaces.
