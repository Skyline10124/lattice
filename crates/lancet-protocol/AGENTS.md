# AGENTS.md

Crate-local guidance for `lancet-protocol`.

## Role

This crate is the pure shared wire/data contract layer. It defines JSON-RPC
envelopes and Tool frames, typed durable identities/bindings, Cell descriptors
and capabilities, typed errors, and versioned evidence. It has no I/O or async
runtime dependency.

## Sub-Architecture

- `src/jsonrpc.rs` owns domain-neutral JSON-RPC 2.0 envelope types.
- `src/frame.rs` gives CEP-specific aliases over JSON-RPC frames.
- `src/cell.rs` owns the ADR 0004 Cell effect core
  (`CtxOp`/`ExtImpact`/`Trust`/`ScopeDecl`/`Cell`/`Capability`/
  `ToolDescriptor`) plus trust-free CellRef layering.
- `src/capability.rs` owns required-capability patterns and resolution errors.
- `src/evidence.rs` owns the versioned admission/effect evidence vocabulary:
  `AdmissionEvidence`, `ObservedEffectsRecord`, `DeniedOperationsRecord`,
  `ObservedScope`, `ObservedTarget`, and `CapabilityAdmissionPolicy`.
- `src/canonical.rs` owns `canonical_json`, the deterministic writer used for
  context measures and persisted evidence payload construction.
- `src/binding.rs` owns `InvocationBinding` and `arguments_digest`.
- `src/identity.rs` owns the strict typed identities (#142, ADR 0005 §1):
  `InvocationId`/`TurnId` (lowercase hyphenated UUID v4) and `SessionId` (32
  lowercase hex). Parse/display/serde/schema only — never a generator.
- `src/address.rs` owns the provider-neutral structured addresses:
  `ModelMessageAddress`, `ToolCallAddress`, `ModelAttemptKey`, `PlanKey`.
- `src/error.rs` owns `CepError`, families, wire codes, and retriability.
- Public re-exports in `src/lib.rs` are the crate contract; keep them stable.

## Boundaries

- No I/O, async runtime, execution engine, or policy side effects.
- Serialized field changes are cross-crate changes, not local refactors.
- Keep serde and schemars behavior explicit for new or renamed fields.
- Do not encode concrete Component-engine or Tool Host implementation assumptions.
- ADR 0004 Cell vocabulary starts here before authority, audit, runtime, or
  generated-descriptor consumers change.
- Turn/audit provenance and in-toto attestation wire types were retired by
  GitHub #114. Preserve artifact identity and bundle provenance contracts where
  they are owned; do not recreate a second operator projection vocabulary here.
- ADR 0005 assigns the shared `InvocationId` and `TurnId` data types here.
  GitHub #142 added strict lowercase hyphenated UUID-v4
  parse/display/serde/schema and tests; this crate validates but does not
  generate IDs. #139-B threads `InvocationId` through production JSON-RPC,
  runtime calls, events, evidence, and operator DTOs. Do not add a second
  String/newtype identity.

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
- Runtime calls use the canonical string form of an already-reserved
  `InvocationId`; reservation creates the invocation. Control notifications may
  reference an existing InvocationId but cannot reserve or execute a Tool.

## Verification

- Local: `cargo test -p lancet-protocol`.
- Schema/wire changes: add serialization or schemars assertions where useful.
- Cross-crate protocol changes: also test affected authority, audit, runtime,
  API, and CLI surfaces.
